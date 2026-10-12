#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>
#include "rdna_metal.hpp"
#include "pm4.hpp"
#include "shader_registry.hpp"
#include "guest_shader.hpp"
#include "guest_submission.hpp"
#include "graphics_services.hpp"
#include "dispatcher.hpp"
#include "elf_image.hpp"
#include "import_registry.hpp"
#include "graphics-discovered.hpp"
#include <nid/NidCompute.hpp>
#include <fstream>
#include <iterator>
#include <SceShaders.hpp>
#include <algorithm>
#include <array>
#include <cstring>
#include <stdexcept>
#include <cstdint>
#include <iostream>
#include <vector>

#if !defined(__aarch64__) || !defined(__APPLE__)
#error Requires native ARM64 macOS
#endif

int Render(const NativePS5::Pm4Submission& submission,std::span<std::byte> guestCode,const char* outputPath) {
    @autoreleasepool {
        alignas(8) std::array<std::byte,2*sizeof(Shader)> guestHeaders{};
        try {
            if (submission.draws.size()!=1) throw std::invalid_argument("Render fixture requires exactly one draw");
            for (const auto& draw:submission.draws) {
                if (draw.state.shader.size()!=4 || !draw.state.context.empty() || draw.state.userConfig.size()!=2
                    || draw.state.userConfig.at(0x242)!=4 || draw.flags || draw.instances!=1
                    || uint64_t(draw.firstVertex)+draw.count>4)
                    throw std::invalid_argument("PM4 state exceeds the explicit test pipeline bindings");
            }
        } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 19; }
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device || !device.hasUnifiedMemory) return 2;
        NSError* error = nil;
        const std::vector<uint32_t> vertexWords={
            0x7e080303, 0x7e0a0302, 0x7e0c0301, 0x7e0e0300,
            0xf80008cf, 0x04050607, 0x7e0e0280, 0xbf810000};
        const std::vector<uint32_t> fragmentWords={
            0x7e0002f2, 0x7e0202ff, 0x3dcccccd, 0x7e0402ff, 0x3e4ccccd,
            0x7e0602ff, 0x3f4ccccd, 0xf800180f, 0x00010203, 0x7e060280, 0xbf810000};
        std::string translated;
        try {
            NativePS5::ShaderRegistry registry;
            std::memcpy(guestCode.data(),vertexWords.data(),vertexWords.size()*4);
            std::memcpy(guestCode.data()+256,fragmentWords.data(),fragmentWords.size()*4);
            for (unsigned i=0;i<2;++i) {
                Shader header{};
                header.file_header=0x34333231; header.version=0x18; header.type=i==0?2:1;
                header.header_size=sizeof(Shader); header.shader_size=uint32_t((i==0?vertexWords.size():fragmentWords.size())*4);
                header.code=guestCode.data()+i*256;
                std::memcpy(guestHeaders.data()+i*sizeof(Shader),&header,sizeof(header));
            }
            NativePS5::GuestMemory memory;
            memory.Add(guestCode,false); memory.Add(guestHeaders,false);
            NativePS5::RegisterGuestShader(registry,memory,reinterpret_cast<uintptr_t>(guestHeaders.data()));
            NativePS5::RegisterGuestShader(registry,memory,reinterpret_cast<uintptr_t>(guestHeaders.data()+sizeof(Shader)));
            const auto& state=submission.draws.front().state;
            const auto vertex=registry.Resolve(state,NativePS5::RdnaRenderStage::Vertex);
            const auto fragment=registry.Resolve(state,NativePS5::RdnaRenderStage::Fragment);
            translated=NativePS5::TranslateRdnaRenderBlock(vertex.Code(),NativePS5::RdnaRenderStage::Vertex)
                       +NativePS5::TranslateRdnaRenderBlock(fragment.Code(),NativePS5::RdnaRenderStage::Fragment);
            unsigned rejected=0;
            for (uint32_t bad:{0xf80008c7u,0xf8000ccfu,0xf800080fu,0xf80000cfu,0xf80018cfu}) {
                auto invalid=vertexWords; invalid[4]=bad;
                try { NativePS5::TranslateRdnaRenderBlock(invalid,NativePS5::RdnaRenderStage::Vertex); }
                catch (const std::exception&) { ++rejected; }
            }
            auto invalid=fragmentWords; invalid[7]=0xf800080f;
            try { NativePS5::TranslateRdnaRenderBlock(invalid,NativePS5::RdnaRenderStage::Fragment); }
            catch (const std::exception&) { ++rejected; }
            auto noExport=vertexWords; noExport.erase(noExport.begin()+4,noExport.begin()+6);
            try { NativePS5::TranslateRdnaRenderBlock(noExport,NativePS5::RdnaRenderStage::Vertex); }
            catch (const std::exception&) { ++rejected; }
            auto duplicate=vertexWords; duplicate.insert(duplicate.end()-1,{0xf80008cf,0x04050607});
            try { NativePS5::TranslateRdnaRenderBlock(duplicate,NativePS5::RdnaRenderStage::Vertex); }
            catch (const std::exception&) { ++rejected; }
            if (rejected!=8) throw std::runtime_error("Invalid stage export accepted");
        } catch (const std::exception& failure) { std::cerr<<failure.what()<<"\n"; return 15; }
        NSString* source=[[NSString alloc] initWithBytes:translated.data() length:translated.size() encoding:NSUTF8StringEncoding];
        id<MTLBuffer> vertexInput=[device newBufferWithLength:4*256*4 options:MTLResourceStorageModeShared];
        id<MTLBuffer> fragmentInput=[device newBufferWithLength:256*4 options:MTLResourceStorageModeShared];
        id<MTLBuffer> scalarInput=[device newBufferWithLength:106*4 options:MTLResourceStorageModeShared];
        if (!vertexInput || !fragmentInput || !scalarInput) return 16;
        std::memset(vertexInput.contents,0,vertexInput.length);
        std::memset(fragmentInput.contents,0,fragmentInput.length);
        std::memset(scalarInput.contents,0,scalarInput.length);
        const float positions[4][4]={{10,10,0,1},{-0.8f,-0.8f,0,1},{0.8f,-0.8f,0,1},{0,0.8f,0,1}};
        for (size_t i=0;i<4;++i) std::memcpy(static_cast<uint32_t*>(vertexInput.contents)+i*256,positions[i],16);
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) { std::cerr << error.description.UTF8String << "\n"; return 3; }
        MTLRenderPipelineDescriptor* descriptor = [MTLRenderPipelineDescriptor new];
        descriptor.vertexFunction = [library newFunctionWithName:@"rdna_vertex"];
        descriptor.fragmentFunction = [library newFunctionWithName:@"rdna_fragment"];
        descriptor.colorAttachments[0].pixelFormat = MTLPixelFormatRGBA8Unorm;
        id<MTLRenderPipelineState> pipeline = [device newRenderPipelineStateWithDescriptor:descriptor error:&error];
        if (!pipeline) { std::cerr << error.description.UTF8String << "\n"; return 4; }
        constexpr NSUInteger width = 256;
        MTLTextureDescriptor* textureDescriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:width height:width mipmapped:NO];
        textureDescriptor.storageMode = MTLStorageModeShared;
        textureDescriptor.usage = MTLTextureUsageRenderTarget;
        id<MTLTexture> texture = [device newTextureWithDescriptor:textureDescriptor];
        if (!texture) return 5;
        MTLRenderPassDescriptor* pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = texture;
        pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore;
        pass.colorAttachments[0].clearColor = MTLClearColorMake(0, 0, 0, 1);
        id<MTLCommandQueue> queue = [device newCommandQueue];
        id<MTLCommandBuffer> command = [queue commandBuffer];
        id<MTLRenderCommandEncoder> encoder = [command renderCommandEncoderWithDescriptor:pass];
        if (!queue || !command || !encoder) return 6;
        [encoder setRenderPipelineState:pipeline];
        [encoder setVertexBuffer:vertexInput offset:0 atIndex:0];
        [encoder setVertexBuffer:scalarInput offset:0 atIndex:1];
        [encoder setFragmentBuffer:fragmentInput offset:0 atIndex:0];
        [encoder setFragmentBuffer:scalarInput offset:0 atIndex:1];
        for (const auto& draw:submission.draws)
            [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:draw.firstVertex vertexCount:draw.count instanceCount:draw.instances];
        [encoder endEncoding];
        [command commit];
        [command waitUntilCompleted];
        if (command.status != MTLCommandBufferStatusCompleted) {
            std::cerr << command.error.description.UTF8String << "\n"; return 7;
        }
        std::vector<uint8_t> pixels(width * width * 4);
        [texture getBytes:pixels.data() bytesPerRow:width * 4 fromRegion:MTLRegionMake2D(0, 0, width, width) mipmapLevel:0];
        auto* center = &pixels[(128 * width + 128) * 4];
        if (center[0] < 203 || center[0] > 205 || center[1] < 50 || center[1] > 52 || center[2] < 25 || center[2] > 27 || center[3] != 255) return 8;
        if (pixels[0] || pixels[1] || pixels[2] || pixels[3] != 255) return 9;
        size_t colored = 0;
        for (NSUInteger i = 0; i < width * width; ++i) if (pixels[i * 4] > 0) ++colored;
        if (colored < 20000 || colored > 22000) return 10;
        size_t checkedPixels=0;
        for (NSUInteger y=0;y<width;++y) for (NSUInteger x=0;x<width;++x) {
            // Independent triangle half-plane test in NDC, excluding rasterization edges.
            const double nx=2*(double(x)+0.5)/width-1;
            const double ny=1-2*(double(y)+0.5)/width;
            const double a=(0.8-ny)/1.6;
            const double b=(nx+0.8*a)/1.6;
            const double c=a-b;
            const double minimum=std::min({b,c,1-a});
            const auto* pixel=&pixels[(y*width+x)*4];
            if (minimum>0.02) {
                if (pixel[0]<203 || pixel[0]>205 || pixel[1]<50 || pixel[1]>52
                    || pixel[2]<25 || pixel[2]>27 || pixel[3]!=255) return 17;
            } else if (minimum< -0.02) {
                if (pixel[0] || pixel[1] || pixel[2] || pixel[3]!=255) return 18;
            } else continue;
            ++checkedPixels;
        }
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGBitmapInfo bitmapInfo = CGBitmapInfo(kCGImageAlphaPremultipliedLast) | CGBitmapInfo(kCGBitmapByteOrder32Big);
        CGContextRef context = CGBitmapContextCreate(pixels.data(), width, width, 8, width * 4, space, bitmapInfo);
        CGColorSpaceRelease(space);
        if (!context) return 11;
        CGImageRef image = CGBitmapContextCreateImage(context);
        CGContextRelease(context);
        if (!image) return 12;
        NSURL* url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:outputPath]];
        CGImageDestinationRef destination = CGImageDestinationCreateWithURL((__bridge CFURLRef)url, CFSTR("public.png"), 1, nullptr);
        if (!destination) { CGImageRelease(image); return 13; }
        CGImageDestinationAddImage(destination, image, nullptr);
        bool saved = CGImageDestinationFinalize(destination);
        CFRelease(destination);
        CGImageRelease(image);
        if (!saved) return 14;
        std::cout << "PASS: PM4 draw -> RDNA vertex/pixel binaries -> Metal; " << colored << " colored pixels; " << checkedPixels << " reference pixel checks; " << device.name.UTF8String << "\n";
    }
    return 0;
}

int main(int argc,char** argv) {
    try {
        if (argc!=3) throw std::invalid_argument("Usage: rdna-render-verify output.png graphics-call.elf");
        alignas(256) std::array<std::byte,512> guestCode{};
        const uint64_t vertexAddress=reinterpret_cast<uintptr_t>(guestCode.data()),fragmentAddress=vertexAddress+256;
        std::vector<uint32_t> commands={0x80000000,0xc0017900,0x242,4,
            0xc0017900,0x24a,1,0xc0002f00,1,
            0xc0027600,0xc8,uint32_t(vertexAddress>>8),uint32_t(vertexAddress>>40),
            0xc0027600,8,uint32_t(fragmentAddress>>8),uint32_t(fragmentAddress>>40),0xc0012d00,3,2};
        std::array<uint64_t,2> packet={reinterpret_cast<uintptr_t>(commands.data()),commands.size()};
        std::ifstream stream(argv[2],std::ios::binary);
        if (!stream) throw std::runtime_error("Cannot open graphics ELF fixture");
        std::vector<char> file((std::istreambuf_iterator<char>(stream)),{});
        {
            NativePS5::ElfImage missing(std::as_bytes(std::span(file)));
            missing.Relocate([](std::string_view)->std::optional<uint64_t> { return {}; });
            uint64_t slot=0;
            std::memcpy(&slot,missing.Read(0x4000,8).data(),8);
            if (missing.ImportsResolved() || missing.Unresolved().size()!=1 || slot
                || missing.Unresolved()[0].name!=Nid::ComputeNid("sceAgcDriverSubmitDcb","")+"#A#B"
                || missing.Unresolved()[0].identity.module!="libSceAgcDriver.prx")
                throw std::runtime_error("Missing graphics ELF service was falsely resolved");
        }
        NativePS5::ElfImage executable(std::as_bytes(std::span(file)));
        const std::array<unsigned char,10> translatedBytes={0xff,0x15,0xfa,0x3e,0,0,0x89,0xc3,0xeb,0x36};
        if (executable.Entry()-executable.LoadBias()!=0x100
            || std::memcmp(executable.Read(0x100,translatedBytes.size()).data(),translatedBytes.data(),translatedBytes.size()))
            throw std::runtime_error("ELF code differs from the compiled translation");
        alignas(16) std::array<std::byte,4096> stack{};
        NativePS5::GuestMemory memory;
        memory.Add(std::as_writable_bytes(std::span(commands)),false);
        memory.Add(std::as_writable_bytes(std::span(packet)),false);
        for (const auto& segment:executable.Segments()) {
            auto bytes=executable.Read(segment.address,segment.memorySize);
            memory.Add({const_cast<std::byte*>(bytes.data()),bytes.size()},segment.flags&2,segment.flags&1);
        }
        memory.Add(stack,true);
        NativePS5::BootstrapServices bootstrap(memory);
        unsigned completed=0;
        NativePS5::GraphicsServices graphics(memory,[&](const NativePS5::GuestSubmission& prepared) {
            if (Render(prepared.decoded,guestCode,argv[1])!=0) throw std::runtime_error("Metal render/verification failed");
            ++completed;
        });
        NativePS5::ImportRegistry imports;
        imports.Register("libSceAgcDriver.prx","sceAgcDriverSubmitDcb",*graphics.Resolve("sceAgcDriverSubmitDcb"));
        executable.RelocateScoped([&](const auto& identity) { return imports.Resolve(identity); });
        if (!executable.ImportsResolved() || executable.AppliedRelocations()!=1
            || executable.Dependencies()!=std::vector<std::string>{"libSceAgcDriver.prx"})
            throw std::runtime_error("Graphics ELF import/dependency resolution failed");
        if (memory.Load(executable.LoadBias()+0x4000,8)!=*graphics.Resolve("sceAgcDriverSubmitDcb"))
            throw std::runtime_error("Graphics ELF JUMP_SLOT points to the wrong service");
        NativePS5::GuestCpu cpu;
        cpu.memory=&memory; cpu.loadBias=executable.LoadBias(); cpu.rip=0x100; cpu.fuel=100;
        const auto callerStack=reinterpret_cast<uintptr_t>(stack.data()+stack.size());
        cpu.registers[4]=callerStack; cpu.registers[7]=reinterpret_cast<uintptr_t>(packet.data());
        cpu.registers[0]=0xdeadbeef;
        NativePS5::Dispatcher dispatcher;
        Register_graphics(dispatcher,executable);
        const auto result=dispatcher.Run(cpu,bootstrap,100,[&](auto& current) { return graphics.Dispatch(current); });
        if (result.reason!=NativePS5::DispatchStop::MissingCode || result.pc!=0x140 || completed!=1
            || cpu.fault || cpu.registers[0]!=0 || cpu.registers[3]!=0 || cpu.registers[4]!=callerStack)
            throw std::runtime_error("Translated graphics call did not complete: "+std::string(graphics.LastError()));
        std::cout<<"PASS: ELF JUMP_SLOT -> translated x86 indirect CALL -> graphics bridge -> Metal completion -> guest continuation; synthetic fixture only\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
