#include "guest_shader.hpp"
#include <SceShaders.hpp>
#include <array>
#include <cstring>
#include <iostream>
#include <stdexcept>

int main() {
    try {
        using namespace NativePS5;
        alignas(256) std::array<uint32_t,128> code{0xbf810000};
        alignas(8) std::array<std::byte,512> header{};
        const auto base=reinterpret_cast<uintptr_t>(header.data());
        const auto codeAddress=reinterpret_cast<uintptr_t>(code.data());
        Shader fields{};
        fields.file_header=0x34333231; fields.version=0x18; fields.header_size=header.size();
        fields.shader_size=4; fields.code=code.data(); fields.type=2;
        fields.sh_registers=reinterpret_cast<ShaderRegister*>(base+96); fields.num_sh_registers=2;
        fields.user_data=reinterpret_cast<ShaderUserData*>(base+128);
        fields.input_semantics=reinterpret_cast<ShaderSemantic*>(base+208); fields.num_input_semantics=1;
        fields.specials=reinterpret_cast<ShaderSpecialRegs*>(base+224);
        std::memcpy(header.data(),&fields,sizeof(fields));
        std::array<ShaderRegister,2> regs{{{0xc8,uint32_t(codeAddress>>8)},{0xc9,uint32_t(codeAddress>>40)}}};
        std::memcpy(header.data()+96,regs.data(),sizeof(regs));
        ShaderUserData user{}; user.direct_resource_count=1; user.direct_resource_offset=reinterpret_cast<uint16_t*>(base+200);
        std::memcpy(header.data()+128,&user,sizeof(user));
        auto original=header;
        GuestMemory memory; memory.Add(header,false); memory.Add(std::as_writable_bytes(std::span(code)),false);
        ShaderRegistry registry;
        auto registered=RegisterGuestShader(registry,memory,base);
        if (registered.header!=std::vector<std::byte>(original.begin(),original.end()) || registered.codeAddress!=codeAddress
            || registry.Resolve(registered.registers,RdnaRenderStage::Vertex).Code()[0]!=0xbf810000)
            throw std::runtime_error("Guest shader registration snapshot mismatch");
        code[0]=0xdeadbeef;
        unsigned rejected=0;
        auto reject=[&](auto mutate) {
            header=original; mutate();
            try { RegisterGuestShader(registry,memory,base); }
            catch (const std::exception&) { ++rejected; }
        };
        auto write=[&](size_t offset,uint64_t value,size_t size) { std::memcpy(header.data()+offset,&value,size); };
        reject([&] { write(0,0,4); });
        reject([&] { write(4,0x19,4); });
        reject([&] { write(64,95,4); });
        reject([&] { write(64,65537,4); });
        reject([&] { write(68,3,4); });
        reject([&] { write(68,16388,4); });
        reject([&] { write(16,codeAddress+1,8); });
        reject([&] { write(16,0x100,8); });
        reject([&] { write(90,6,1); });
        reject([&] { write(32,base+508,8); });
        reject([&] { write(48,base-4,8); });
        reject([&] { write(128+offsetof(ShaderUserData,direct_resource_offset),base+511,8); });
        if (rejected!=12 || registry.Resolve(registered.registers,RdnaRenderStage::Vertex).Code()[0]!=0xbf810000)
            throw std::runtime_error("Malformed registration accepted or changed existing snapshot");
        std::cout<<"PASS: guarded AGC header/code import, nested metadata bounds, owned snapshots; 12 malformed registrations rejected\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
