#include "shader_registry.hpp"
#include <stdexcept>

namespace NativePS5 {
namespace {
void Require(bool ok,const char* message) { if (!ok) throw std::invalid_argument(message); }
void ValidateStage(RdnaRenderStage stage) {
    Require(stage==RdnaRenderStage::Vertex || stage==RdnaRenderStage::Fragment,"Unsupported shader registration stage");
}
}
void ShaderRegistry::Register(uint64_t address,RdnaRenderStage stage,std::span<const uint32_t> words) {
    ValidateStage(stage);
    constexpr uint64_t addressLimit=uint64_t(1)<<48;
    Require(address && !(address&255) && address<addressLimit,"Invalid shader registration address");
    Require(!words.empty() && words.size()<=4096 && words.size_bytes()<=addressLimit-address,"Invalid shader registration size");
    Require(images.contains(address) || images.size()<64,"Shader registry capacity reached");
    for (const auto& [base,image]:images) {
        if (base==address) continue;
        Require(address>=base+image->words.size()*4 || base>=address+words.size_bytes(),"Overlapping shader registrations");
    }
    // Existing draw references retain immutable bytes when an address is replaced.
    auto image=std::make_shared<const ShaderImage>(ShaderImage{address,stage,{words.begin(),words.end()}});
    images.insert_or_assign(address,std::move(image));
}
ShaderEntry ShaderRegistry::Resolve(const Pm4State& state,RdnaRenderStage stage) const {
    ValidateStage(stage);
    // AnyPS5 registered type-2 vertex and type-1 pixel program registers.
    const uint32_t lowRegister=stage==RdnaRenderStage::Vertex?0xc8:0x8;
    auto low=state.shader.find(lowRegister),high=state.shader.find(lowRegister+1);
    Require(low!=state.shader.end() && high!=state.shader.end(),"Missing shader program address registers");
    Require(high->second<=0xff,"Unsupported shader address high bits");
    const uint64_t address=(uint64_t(high->second)<<40)|(uint64_t(low->second)<<8);
    auto found=images.upper_bound(address);
    Require(found!=images.begin(),"Unregistered shader program address");
    --found;
    const auto& image=found->second;
    Require(address-image->address<image->words.size()*4,"Shader entry outside registered code");
    Require(image->stage==stage,"Shader stage does not match registration");
    return {image,size_t((address-image->address)/4)};
}
}
