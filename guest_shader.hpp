#pragma once
#include "guest_cpu.hpp"
#include "shader_registry.hpp"

namespace NativePS5 {
struct GuestShaderRegistration {
    uint64_t headerAddress,codeAddress;
    RdnaRenderStage stage;
    Pm4State registers;
    std::vector<std::byte> header;
};
// Validates and snapshots the AGC registration structure. Resource metadata is
// retained, not implemented by this reader or silently converted into defaults.
GuestShaderRegistration RegisterGuestShader(ShaderRegistry& registry,const GuestMemory& memory,uint64_t headerAddress);
}
