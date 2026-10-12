#include "guest_shader.hpp"
#include <SceShaders.hpp>
#include <algorithm>
#include <stdexcept>

namespace NativePS5 {
namespace {
static_assert(sizeof(Shader)==96 && offsetof(Shader,code)==16 && offsetof(Shader,header_size)==64 && offsetof(Shader,type)==90);
void Require(bool ok,const char* message) { if (!ok) throw std::invalid_argument(message); }
uint64_t Field(std::span<const std::byte> bytes,size_t offset,size_t size) {
    Require(offset<=bytes.size() && size<=bytes.size()-offset,"Truncated shader header field");
    uint64_t result=0;
    for (size_t i=0;i<size;++i) result|=uint64_t(std::to_integer<uint8_t>(bytes[offset+i]))<<(i*8);
    return result;
}
std::vector<std::byte> Copy(const GuestMemory& memory,uint64_t address,size_t size) {
    Require(address && size<=UINT64_MAX-address,"Invalid guest shader memory range");
    std::vector<std::byte> bytes(size);
    for (size_t i=0;i<size;++i) bytes[i]=std::byte(memory.Load(address+i,1));
    return bytes;
}
}
GuestShaderRegistration RegisterGuestShader(ShaderRegistry& registry,const GuestMemory& memory,uint64_t address) {
    auto fixed=Copy(memory,address,sizeof(Shader));
    Require(Field(fixed,0,4)==0x34333231 && Field(fixed,4,4)==0x18,"Invalid AGC shader magic or version");
    const auto headerSize=Field(fixed,offsetof(Shader,header_size),4);
    const auto codeSize=Field(fixed,offsetof(Shader,shader_size),4);
    const auto codeAddress=Field(fixed,offsetof(Shader,code),8);
    const auto type=Field(fixed,offsetof(Shader,type),1);
    Require(headerSize>=sizeof(Shader) && headerSize<=65536,"Unsupported AGC header size");
    Require(codeSize && codeSize<=16384 && !(codeSize&3) && codeAddress && !(codeAddress&255),"Invalid AGC code range");
    Require(type==1 || type==2,"Unsupported AGC shader stage");
    GuestShaderRegistration result{address,codeAddress,type==1?RdnaRenderStage::Fragment:RdnaRenderStage::Vertex,{},Copy(memory,address,headerSize)};
    // Read one immutable header snapshot; reject concurrent fixed-field changes.
    Require(std::equal(fixed.begin(),fixed.end(),result.header.begin()),"Guest shader header changed during registration");
    auto field=[&](size_t offset,size_t size) { return Field(result.header,offset,size); };
    auto range=[&](uint64_t pointer,uint64_t count,size_t elementSize)->size_t {
        if (!count) return 0;
        Require(pointer>=address && pointer-address<=headerSize,"Shader metadata outside header");
        auto offset=size_t(pointer-address);
        Require(count<=(headerSize-offset)/elementSize,"Truncated shader metadata array");
        return offset;
    };
    auto registers=[&](size_t pointerField,size_t countField,auto& bank) {
        const auto count=field(countField,1);
        const auto offset=range(field(pointerField,8),count,sizeof(ShaderRegister));
        for (size_t i=0;i<count;++i) {
            const auto reg=field(offset+i*8,4),value=field(offset+i*8+4,4);
            Require(reg<=0xffff,"Unsupported shader metadata register index");
            bank.insert_or_assign(uint32_t(reg),uint32_t(value));
        }
    };
    registers(offsetof(Shader,sh_registers),offsetof(Shader,num_sh_registers),result.registers.shader);
    registers(offsetof(Shader,cx_registers),offsetof(Shader,num_cx_registers),result.registers.context);
    range(field(offsetof(Shader,input_semantics),8),field(offsetof(Shader,num_input_semantics),4),sizeof(ShaderSemantic));
    range(field(offsetof(Shader,output_semantics),8),field(offsetof(Shader,num_output_semantics),2),sizeof(ShaderSemantic));
    const auto special=field(offsetof(Shader,specials),8);
    if (special) range(special,1,sizeof(ShaderSpecialRegs));
    const auto user=field(offsetof(Shader,user_data),8);
    if (user) {
        const auto offset=range(user,1,sizeof(ShaderUserData));
        range(field(offset+offsetof(ShaderUserData,direct_resource_offset),8),field(offset+offsetof(ShaderUserData,direct_resource_count),2),2);
        for (size_t i=0;i<4;++i)
            range(field(offset+offsetof(ShaderUserData,sharp_resource_offset)+i*8,8),field(offset+offsetof(ShaderUserData,sharp_resource_count)+i*2,2),sizeof(ShaderSharp));
    }
    const auto bytes=Copy(memory,codeAddress,codeSize);
    std::vector<uint32_t> words(codeSize/4);
    for (size_t i=0;i<words.size();++i) words[i]=uint32_t(Field(bytes,i*4,4));
    registry.Register(codeAddress,result.stage,words);
    return result;
}
}
