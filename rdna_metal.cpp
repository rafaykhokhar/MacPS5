#include "rdna_metal.hpp"
#include <RdnaDecoder/RdnaInstructionDecoder.hpp>
#include <stdexcept>

namespace NativePS5 {
namespace {
using namespace ShaderRecompiler;
std::string Source(const RdnaOperand& operand) {
    switch (operand.kind) {
        case RdnaOperandKind::VectorRegister:
            if (operand.reg >= 256) break;
            return "v["+std::to_string(operand.reg)+"]";
        case RdnaOperandKind::ScalarRegister:
            if (operand.reg >= 106) break;
            return "sgpr["+std::to_string(operand.reg)+"]";
        case RdnaOperandKind::IntegerInlineConstant:
        case RdnaOperandKind::FloatInlineConstant:
        case RdnaOperandKind::LiteralConstant:
            return std::to_string(operand.value)+"u";
        default: break;
    }
    throw std::invalid_argument("Unsupported RDNA Metal source register");
}
std::string Expression(const RdnaInstruction& ins) {
    const auto a=Source(ins.source0);
    if (ins.op==RdnaOpcode::VMovB32) return a;
    if (ins.op==RdnaOpcode::VNotB32) return "~("+a+")";
    const auto b=Source(ins.source1);
    switch (ins.op) {
        case RdnaOpcode::VAddNcU32: return a+" + "+b;
        case RdnaOpcode::VSubNcU32: return a+" - "+b;
        case RdnaOpcode::VSubrevNcU32: return b+" - "+a;
        case RdnaOpcode::VAndB32: return a+" & "+b;
        case RdnaOpcode::VOrB32: return a+" | "+b;
        case RdnaOpcode::VXorB32: return a+" ^ "+b;
        case RdnaOpcode::VXnorB32: return "~("+a+" ^ "+b+")";
        case RdnaOpcode::VLshlrevB32: return b+" << ("+a+" & 31u)";
        case RdnaOpcode::VLshrrevB32: return b+" >> ("+a+" & 31u)";
        default: throw std::invalid_argument("Unsupported RDNA Metal opcode at PC "+std::to_string(ins.programCounter));
    }
}
std::string EmitInteger(const RdnaInstruction& ins) {
    if (ins.family!=RdnaInstructionFamily::VOP1 && ins.family!=RdnaInstructionFamily::VOP2)
        throw std::invalid_argument("Unsupported RDNA Metal instruction family");
    const auto encoding=ins.rawWords[0]&0x1ff;
    if (encoding==249 || encoding==250 || encoding==233 || encoding==234)
        throw std::invalid_argument("SDWA and DPP are not implemented by the Metal backend");
    if (ins.destination.kind!=RdnaOperandKind::VectorRegister || ins.destination.reg>=256 || ins.is64Bit || ins.clampResult)
        throw std::invalid_argument("Unsupported RDNA Metal destination");
    return "        v["+std::to_string(ins.destination.reg)+"] = "+Expression(ins)+";\n";
}
RdnaProgram DecodeBlock(std::span<const uint32_t> words) {
    if (words.empty() || words.size()>4096) throw std::invalid_argument("Invalid RDNA block size");
    auto program=RdnaInstructionDecoder{}.Decode(words);
    const auto& last=program.instructions.back();
    if (last.op!=RdnaOpcode::SEndpgm || last.rawWords[0]!=0xbf810000u
        || size_t(last.programCounter)/4+last.wordCount!=words.size())
        throw std::invalid_argument("Only terminal s_endpgm 0 accepted");
    return program;
}
}

std::string TranslateRdnaIntegerBlock(std::span<const uint32_t> words) {
    auto program=DecodeBlock(words);
    std::string source=R"(#include <metal_stdlib>
using namespace metal;
kernel void rdna_block(device const uint* input [[buffer(0)]],
                       constant uint* sgpr [[buffer(1)]],
                       device uint* output [[buffer(2)]],
                       constant uint& count [[buffer(3)]],
                       device const uint* active [[buffer(4)]],
                       uint i [[thread_position_in_grid]]) {
    if (i >= count) return;
    uint v[256];
    for (uint r=0; r<256; ++r) v[r]=input[size_t(i)*256+r];
    if (active[i] != 0) {
)";
    for (const auto& ins:program.instructions) {
        if (ins.op==RdnaOpcode::SEndpgm) break;
        source+=EmitInteger(ins);
    }
    source+="    }\n    for (uint r=0; r<256; ++r) output[size_t(i)*256+r]=v[r];\n}\n";
    return source;
}

std::string TranslateRdnaRenderBlock(std::span<const uint32_t> words,RdnaRenderStage stage) {
    if (stage!=RdnaRenderStage::Vertex && stage!=RdnaRenderStage::Fragment)
        throw std::invalid_argument("Invalid RDNA render stage");
    const bool vertex=stage==RdnaRenderStage::Vertex;
    auto program=DecodeBlock(words);
    std::string source="#include <metal_stdlib>\nusing namespace metal;\n";
    if (vertex) {
        source+="struct RdnaVertexOutput { float4 position [[position]]; };\n"
                "vertex RdnaVertexOutput rdna_vertex(device const uint* input [[buffer(0)]], constant uint* sgpr [[buffer(1)]], uint i [[vertex_id]]) {\n"
                "uint v[256]; for (uint r=0;r<256;++r) v[r]=input[size_t(i)*256+r];\n";
    } else {
        source+="fragment float4 rdna_fragment(device const uint* input [[buffer(0)]], constant uint* sgpr [[buffer(1)]]) {\n"
                "uint v[256]; for (uint r=0;r<256;++r) v[r]=input[r];\n";
    }
    source+="float4 exported;\n";
    unsigned exports=0;
    for (const auto& ins:program.instructions) {
        if (ins.op==RdnaOpcode::SEndpgm) break;
        if (ins.op!=RdnaOpcode::Exp) { source+=EmitInteger(ins); continue; }
        if (exports++ || ins.exportTarget!=(vertex?12u:0u) || ins.exportEnableMask!=15
            || ins.exportIsCompressed || !ins.exportIsLast || ins.exportValidMask==vertex)
            throw std::invalid_argument("Only one full-width POS0(done) or MRT0(done,vm) export is supported");
        source+="exported=as_type<float4>(uint4("+Source(ins.source0)+","+Source(ins.source1)+","+
                Source(ins.source2)+","+Source(ins.source3)+"));\n";
    }
    if (exports!=1) throw std::invalid_argument("Missing RDNA stage export");
    source+=vertex?"RdnaVertexOutput result; result.position=exported; return result;\n}\n":"return exported;\n}\n";
    return source;
}
}
