#include "pm4.hpp"
#include <stdexcept>
#include <string>

namespace NativePS5 {
namespace {
void Check(bool ok,size_t pc,const char* message) {
    if (!ok) throw std::invalid_argument("PM4 DWORD "+std::to_string(pc)+": "+message);
}
size_t RegisterCount(const Pm4State& state) { return state.shader.size()+state.context.size()+state.userConfig.size(); }
}
Pm4Submission DecodePm4(std::span<const uint32_t> words,const Pm4State& initial) {
    Check(words.size()<=65536,0,"submission exceeds 256 KiB limit");
    Check(RegisterCount(initial)<=512,0,"register-state limit exceeded");
    for (const auto* bank:{&initial.shader,&initial.context,&initial.userConfig})
        for (const auto& [offset,value]:*bank) { (void)value; Check(offset<=0xffff,0,"invalid initial register offset"); }
    Pm4Submission result{initial,{}};
    for (size_t pc=0;pc<words.size();) {
        const auto header=words[pc];
        if (header==0x80000000u) { ++pc; continue; }
        Check((header>>30)==3,pc,"unsupported packet type");
        Check((header&0xff)==0,pc,"predication, compute and custom flags unsupported");
        const size_t length=((header>>16)&0x3fff)+2;
        Check(length<=words.size()-pc,pc,"truncated packet");
        const auto packet=words.subspan(pc,length);
        const auto opcode=(header>>8)&0xff;
        auto& state=result.finalState;
        if (opcode==0x10) {
            // Ordinary NOP payload has no register or memory effects.
        } else if (opcode==0x2f) {
            Check(length==2,pc,"invalid NUM_INSTANCES length");
            state.instances=packet[1];
        } else if (opcode==0x69 || opcode==0x76 || opcode==0x79) {
            Check(length>=3,pc,"empty register write");
            const auto offset=packet[1];
            Check(offset<=0xffff && length-2<=0x10000u-offset,pc,"unsupported register index or range");
            auto& bank=opcode==0x69?state.context:opcode==0x76?state.shader:state.userConfig;
            for (size_t i=2;i<length;++i) {
                bank.insert_or_assign(offset+uint32_t(i-2),packet[i]);
                Check(RegisterCount(state)<=512,pc,"register-state limit exceeded");
            }
        } else if (opcode==0x2d) {
            Check(length==3,pc,"invalid DRAW_INDEX_AUTO length");
            Check((packet[2]&~0x20u)==2,pc,"unsupported draw initiator");
            Check(state.instances.has_value(),pc,"missing NUM_INSTANCES");
            auto offset=state.userConfig.find(0x24a);
            Check(offset!=state.userConfig.end(),pc,"missing GE_INDX_OFFSET");
            Check(packet[1]==0 || offset->second<=UINT32_MAX-(packet[1]-1),pc,"vertex range overflow");
            Check(result.draws.size()<128,pc,"draw snapshot limit exceeded");
            result.draws.push_back({offset->second,packet[1],*state.instances,packet[2]&0x20u,state});
        } else {
            throw std::invalid_argument("PM4 DWORD "+std::to_string(pc)+": unsupported opcode "+std::to_string(opcode));
        }
        pc+=length;
    }
    return result;
}
}
