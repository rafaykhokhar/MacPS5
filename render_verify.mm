#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <CoreGraphics/CoreGraphics.h>
#import <ImageIO/ImageIO.h>
#include <cstdint>
#include <iostream>
#include <vector>

#if !defined(__aarch64__) || !defined(__APPLE__)
#error Requires native ARM64 macOS
#endif

int main(int argc, char** argv) {
    @autoreleasepool {
        if (argc != 2) { std::cerr << "Usage: render-verify output.png\n"; return 1; }
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device || !device.hasUnifiedMemory) return 2;
        NSError* error = nil;
        NSString* source = @"#include <metal_stdlib>\n"
            "using namespace metal;\n"
            "struct Vertex { float4 position [[position]]; };\n"
            "vertex Vertex triangle(uint i [[vertex_id]]) { float2 p[3] = {float2(-0.8,-0.8),float2(0.8,-0.8),float2(0,0.8)}; Vertex v; v.position=float4(p[i],0,1); return v; }\n"
            "fragment float4 color() { return float4(0.8,0.2,0.1,1); }\n";
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) { std::cerr << error.description.UTF8String << "\n"; return 3; }
        MTLRenderPipelineDescriptor* descriptor = [MTLRenderPipelineDescriptor new];
        descriptor.vertexFunction = [library newFunctionWithName:@"triangle"];
        descriptor.fragmentFunction = [library newFunctionWithName:@"color"];
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
        [encoder drawPrimitives:MTLPrimitiveTypeTriangle vertexStart:0 vertexCount:3];
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
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGBitmapInfo bitmapInfo = CGBitmapInfo(kCGImageAlphaPremultipliedLast) | CGBitmapInfo(kCGBitmapByteOrder32Big);
        CGContextRef context = CGBitmapContextCreate(pixels.data(), width, width, 8, width * 4, space, bitmapInfo);
        CGColorSpaceRelease(space);
        if (!context) return 11;
        CGImageRef image = CGBitmapContextCreateImage(context);
        CGContextRelease(context);
        if (!image) return 12;
        NSURL* url = [NSURL fileURLWithPath:[NSString stringWithUTF8String:argv[1]]];
        CGImageDestinationRef destination = CGImageDestinationCreateWithURL((__bridge CFURLRef)url, CFSTR("public.png"), 1, nullptr);
        if (!destination) { CGImageRelease(image); return 13; }
        CGImageDestinationAddImage(destination, image, nullptr);
        bool saved = CGImageDestinationFinalize(destination);
        CFRelease(destination);
        CGImageRelease(image);
        if (!saved) return 14;
        std::cout << "PASS: native Metal triangle rasterization; " << colored << " colored pixels; " << device.name.UTF8String << "\n";
    }
}
