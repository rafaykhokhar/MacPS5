#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include "rdna_metal.hpp"
#include <algorithm>
#include <array>
#include <cstring>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <vector>

#if !defined(__aarch64__) || !defined(__APPLE__)
#error Requires native ARM64 macOS
#endif

namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
uint32_t Vop1(uint32_t opcode,uint32_t dst,uint32_t source) { return 0x7e000000u | dst<<17 | opcode<<9 | source; }
uint32_t Vop2(uint32_t opcode,uint32_t dst,uint32_t a,uint32_t b) { return opcode<<25 | dst<<17 | b<<9 | a; }
constexpr uint32_t End=0xbf810000;
}

int main(int argc,char** argv) {
    @autoreleasepool {
        try {
            std::vector<uint32_t> words={
                Vop2(0x25,3,256,1), Vop2(0x26,4,256,1), Vop2(0x27,5,256,1),
                Vop2(0x1b,6,256,1), Vop2(0x1c,7,256,1), Vop2(0x1d,8,256,1), Vop2(0x1e,9,256,1),
                Vop2(0x1a,10,258,0), Vop2(0x16,11,258,0), Vop1(0x37,12,256),
                Vop1(1,13,255), 0xdeadbeef, Vop1(1,14,5), Vop1(1,15,193), Vop1(1,16,242),
                Vop2(0x25,3,259,3), Vop2(0x1d,17,269,14), Vop1(1,255,273), End};
            auto source=NativePS5::TranslateRdnaIntegerBlock(words);
            if (argc==2) { std::ofstream out(argv[1]); out<<source; Require(bool(out),"Cannot save generated MSL"); }
            unsigned rejected=0;
            const std::vector<std::vector<uint32_t>> invalid={
                {}, {Vop1(1,0,255)}, {Vop2(0x25,0,256,1)}, {End,End}, {0xbf810001},
                {Vop2(3,0,256,1),End}, {Vop1(1,0,126),End}, {Vop1(1,0,249),0x06000600,End},
                {Vop1(1,0,250),0,End}, {0xbf820000,End}, {0xffffffff,End}};
            for (const auto& fixture:invalid) {
                try { NativePS5::TranslateRdnaIntegerBlock(fixture); }
                catch (const std::exception&) { ++rejected; }
            }
            Require(rejected==invalid.size(),"Unsupported RDNA encoding accepted");
            id<MTLDevice> device=MTLCreateSystemDefaultDevice();
            Require(device && device.hasUnifiedMemory,"Apple unified-memory GPU required");
            NSError* error=nil;
            NSString* text=[[NSString alloc] initWithBytes:source.data() length:source.size() encoding:NSUTF8StringEncoding];
            id<MTLLibrary> library=[device newLibraryWithSource:text options:nil error:&error];
            if (!library) throw std::runtime_error(error.description.UTF8String);
            id<MTLFunction> function=[library newFunctionWithName:@"rdna_block"];
            id<MTLComputePipelineState> pipeline=[device newComputePipelineStateWithFunction:function error:&error];
            if (!pipeline) throw std::runtime_error(error.description.UTF8String);
            constexpr uint32_t lanes=1025, guardLanes=3, registers=256;
            const auto bytes=size_t(lanes+guardLanes)*registers*sizeof(uint32_t);
            id<MTLBuffer> input=[device newBufferWithLength:bytes options:MTLResourceStorageModeShared];
            id<MTLBuffer> output=[device newBufferWithLength:bytes options:MTLResourceStorageModeShared];
            id<MTLBuffer> scalars=[device newBufferWithLength:106*sizeof(uint32_t) options:MTLResourceStorageModeShared];
            id<MTLBuffer> active=[device newBufferWithLength:lanes*sizeof(uint32_t) options:MTLResourceStorageModeShared];
            Require(input && output && scalars && active,"Metal buffer allocation failed");
            auto* in=static_cast<uint32_t*>(input.contents);
            auto* out=static_cast<uint32_t*>(output.contents);
            auto* mask=static_cast<uint32_t*>(active.contents);
            auto* sgpr=static_cast<uint32_t*>(scalars.contents);
            uint32_t random=0x6d2b79f5;
            for (size_t i=0;i<bytes/4;++i) { random=random*1664525u+1013904223u; in[i]=random; }
            for (uint32_t i=0;i<106;++i) sgpr[i]=i*0x1020304u;
            const std::array<uint32_t,8> boundary={0,1,31,32,33,0x7fffffff,0x80000000,0xffffffff};
            for (uint32_t i=0;i<lanes;++i) {
                mask[i]=i<64 || (i%5)!=0;
                if (i<64) { in[i*256]=boundary[i%8]; in[i*256+1]=boundary[i/8]; }
                in[i*256+2]=i<64?i:boundary[i%8];
            }
            std::memset(out,0xa5,bytes);
            id<MTLCommandQueue> queue=[device newCommandQueue];
            id<MTLCommandBuffer> command=[queue commandBuffer];
            id<MTLComputeCommandEncoder> encoder=[command computeCommandEncoder];
            Require(queue && command && encoder,"Metal command allocation failed");
            [encoder setComputePipelineState:pipeline];
            [encoder setBuffer:input offset:0 atIndex:0]; [encoder setBuffer:scalars offset:0 atIndex:1];
            [encoder setBuffer:output offset:0 atIndex:2]; [encoder setBytes:&lanes length:sizeof(lanes) atIndex:3];
            [encoder setBuffer:active offset:0 atIndex:4];
            const auto group=std::min(NSUInteger(64),pipeline.maxTotalThreadsPerThreadgroup);
            [encoder dispatchThreads:MTLSizeMake(lanes+guardLanes,1,1) threadsPerThreadgroup:MTLSizeMake(group,1,1)];
            [encoder endEncoding]; [command commit]; [command waitUntilCompleted];
            if (command.status!=MTLCommandBufferStatusCompleted) throw std::runtime_error(command.error.description.UTF8String);
            size_t checks=0;
            for (uint32_t i=0;i<lanes;++i) {
                std::array<uint32_t,256> expected;
                std::memcpy(expected.data(),in+i*256,sizeof(expected));
                if (mask[i]) {
                    uint32_t a=expected[0],b=expected[1],shift=expected[2]&31;
                    expected[3]=uint32_t(uint64_t(a+b)*2); expected[4]=a-b; expected[5]=b-a;
                    expected[6]=a&b; expected[7]=a|b; expected[8]=a^b; expected[9]=~(a^b);
                    expected[10]=a<<shift; expected[11]=a>>shift; expected[12]=~a;
                    expected[13]=0xdeadbeef; expected[14]=sgpr[5]; expected[15]=0xffffffff; expected[16]=0x3f800000;
                    expected[17]=0xdeadbeef^sgpr[5]; expected[255]=expected[17];
                }
                for (uint32_t r=0;r<256;++r,++checks) {
                    if (out[i*256+r]!=expected[r]) throw std::runtime_error("RDNA mismatch at lane "+std::to_string(i)+" VGPR "+std::to_string(r));
                }
            }
            for (size_t i=size_t(lanes)*256;i<bytes/4;++i) Require(out[i]==0xa5a5a5a5,"Out-of-range GPU write");
            std::cout<<"PASS: RDNA binary -> AnyPS5 decoder -> generated MSL -> "<<device.name.UTF8String
                     <<"; "<<checks<<" register checks; "<<rejected<<" rejected inputs\n"
                     <<"Integer-block verification only; no PS5 graphics command stream or gameplay\n";
        } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
    }
}
