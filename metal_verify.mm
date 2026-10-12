#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#include <cstdint>
#include <iostream>

#if !defined(__aarch64__) || !defined(__APPLE__)
#error Requires native ARM64 macOS
#endif

extern "C" uint32_t guest_add(uint32_t, uint32_t);

int main() {
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device || !device.hasUnifiedMemory) {
            std::cerr << "A unified-memory Metal GPU is required\n";
            return 1;
        }
        constexpr NSUInteger count = 1 << 20;
        constexpr NSUInteger bytes = count * sizeof(uint32_t);
        id<MTLBuffer> input = [device newBufferWithLength:bytes options:MTLResourceStorageModeShared];
        id<MTLBuffer> output = [device newBufferWithLength:bytes options:MTLResourceStorageModeShared];
        if (!input || !output) return 2;
        auto* data = static_cast<uint32_t*>(input.contents);
        for (NSUInteger i = 0; i < count; ++i) data[i] = guest_add(uint32_t(i), 0xffff0000u);
        NSString* source = @"#include <metal_stdlib>\n"
            "using namespace metal;\n"
            "kernel void transform(device const uint* in [[buffer(0)]], device uint* out [[buffer(1)]], uint i [[thread_position_in_grid]]) { out[i] = in[i] * 1664525u + 1013904223u; }\n";
        NSError* error = nil;
        id<MTLLibrary> library = [device newLibraryWithSource:source options:nil error:&error];
        if (!library) { std::cerr << error.description.UTF8String << "\n"; return 3; }
        id<MTLFunction> function = [library newFunctionWithName:@"transform"];
        id<MTLComputePipelineState> pipeline = [device newComputePipelineStateWithFunction:function error:&error];
        if (!pipeline) { std::cerr << error.description.UTF8String << "\n"; return 4; }
        id<MTLCommandQueue> queue = [device newCommandQueue];
        id<MTLCommandBuffer> command = [queue commandBuffer];
        id<MTLComputeCommandEncoder> encoder = [command computeCommandEncoder];
        if (!queue || !command || !encoder) return 5;
        [encoder setComputePipelineState:pipeline];
        [encoder setBuffer:input offset:0 atIndex:0];
        [encoder setBuffer:output offset:0 atIndex:1];
        NSUInteger group = MIN(NSUInteger(256), pipeline.maxTotalThreadsPerThreadgroup);
        [encoder dispatchThreads:MTLSizeMake(count, 1, 1) threadsPerThreadgroup:MTLSizeMake(group, 1, 1)];
        [encoder endEncoding];
        [command commit];
        [command waitUntilCompleted];
        if (command.status != MTLCommandBufferStatusCompleted) {
            std::cerr << command.error.description.UTF8String << "\n";
            return 6;
        }
        auto* result = static_cast<uint32_t*>(output.contents);
        for (NSUInteger i = 0; i < count; ++i) {
            uint32_t expected = uint32_t(uint32_t(uint32_t(i) + 0xffff0000u) * 1664525u + 1013904223u);
            if (result[i] != expected) { std::cerr << "Mismatch at " << i << "\n"; return 7; }
        }
        std::cout << "PASS: translated ARM64 CPU -> shared memory -> Metal GPU -> CPU verification\n"
                  << "GPU: " << device.name.UTF8String << "\n"
                  << "Verified elements: " << count << "; buffer bytes: " << 2 * bytes << "\n";
    }
}
