#include "shader_registry.hpp"
#include <array>
#include <iostream>
#include <stdexcept>

int main() {
    try {
        using namespace NativePS5;
        constexpr uint64_t address=0x123400010000;
        ShaderRegistry registry;
        std::vector<uint32_t> code(65,0xdeadbeef);
        code.back()=0xbf810000;
        registry.Register(address,RdnaRenderStage::Vertex,code);
        Pm4State state;
        state.shader={{0xc8,uint32_t((address+256)>>8)},{0xc9,uint32_t(address>>40)}};
        auto entry=registry.Resolve(state,RdnaRenderStage::Vertex);
        if (entry.wordOffset!=64 || entry.Code().size()!=1 || entry.Code()[0]!=0xbf810000) throw std::runtime_error("Interior shader entry decoded incorrectly");
        code.back()=0;
        if (entry.Code()[0]!=0xbf810000) throw std::runtime_error("Registration retained mutable input");
        registry.Register(address,RdnaRenderStage::Vertex,code);
        if (registry.Resolve(state,RdnaRenderStage::Vertex).Code()[0]!=0 || entry.Code()[0]!=0xbf810000)
            throw std::runtime_error("Replacement mutated in-flight snapshot");
        unsigned failures=0;
        auto reject=[&](auto action) { try { action(); } catch (const std::invalid_argument&) { ++failures; } };
        reject([&] { registry.Register(address+256,RdnaRenderStage::Fragment,std::array<uint32_t,1>{0}); });
        reject([&] { registry.Register(address+1,RdnaRenderStage::Vertex,code); });
        reject([&] { registry.Register(0,RdnaRenderStage::Vertex,code); });
        reject([&] { registry.Register(address,RdnaRenderStage::Vertex,{}); });
        reject([&] { registry.Resolve({},RdnaRenderStage::Vertex); });
        auto bad=state; bad.shader[0xc9]=256;
        reject([&] { registry.Resolve(bad,RdnaRenderStage::Vertex); });
        bad=state; bad.shader[0xc8]+=1;
        reject([&] { registry.Resolve(bad,RdnaRenderStage::Vertex); });
        bad.shader={{8,uint32_t(address>>8)},{9,uint32_t(address>>40)}};
        reject([&] { registry.Resolve(bad,RdnaRenderStage::Fragment); });
        if (failures!=8) throw std::runtime_error("Invalid shader binding accepted");
        std::cout<<"PASS: 48-bit shader address decoding, interior entries, immutable snapshots, replacement and 8 binding rejections\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
