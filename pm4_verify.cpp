#include "pm4.hpp"
#include <array>
#include <iostream>
#include <stdexcept>

namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
}
int main() {
    try {
        const std::vector<uint32_t> words={
            0x80000000,0xc0001000,0xdeadbeef,
            0xc0017900,0x24a,5,0xc0002f00,2,
            0xc0027600,0x8c,0x1234,0x5678,
            0xc0016900,0x8e,0xf,
            0xc0012d00,3,2,
            0xc0017600,0x8c,0xabcd,0xc0002f00,7,0xc0012d00,6,0x22};
        const auto parsed=NativePS5::DecodePm4(words);
        Require(parsed.draws.size()==2,"Draw count mismatch");
        const auto& a=parsed.draws[0]; const auto& b=parsed.draws[1];
        Require(a.firstVertex==5 && a.count==3 && a.instances==2 && a.flags==0,"First draw parameters wrong");
        Require(b.count==6 && b.instances==7 && b.flags==0x20,"Second draw parameters wrong");
        Require(a.state.shader.at(0x8c)==0x1234 && a.state.shader.at(0x8d)==0x5678 && a.state.context.at(0x8e)==0xf,"Register write decoding failed");
        Require(b.state.shader.at(0x8c)==0xabcd && a.state.shader.at(0x8c)==0x1234,"Draw snapshot mutated");
        const auto continued=NativePS5::DecodePm4(std::array<uint32_t,3>{0xc0012d00,1,2},parsed.finalState);
        Require(continued.draws[0].instances==7 && continued.draws[0].firstVertex==5,"Submission state lost");
        const std::vector<std::vector<uint32_t>> bad={
            {0}, {0x40000000}, {0x80000001}, {0xc0012d00,3}, {0xc0012d01,3,2},
            {0xc0012d02,3,2}, {0xc0012d00,3,0}, {0xc0017900,0x10000,1},
            {0xc0027900,0xffff,1,2}, {0xc0007600,0}, {0xc0012f00,1,2},
            {0xc0009900,0}, {0xc0017900,0x24a,0xffffffff,0xc0012d00,2,2}};
        unsigned rejections=0;
        for (const auto& input:bad) {
            try { NativePS5::DecodePm4(input,parsed.finalState); }
            catch (const std::invalid_argument&) { ++rejections; }
        }
        Require(rejections==bad.size(),"Invalid PM4 accepted");
        bool missing=false;
        try { NativePS5::DecodePm4(std::array<uint32_t,3>{0xc0012d00,3,2}); }
        catch (const std::invalid_argument&) { missing=true; }
        Require(missing,"Uninitialized draw state accepted");
        Require(parsed.finalState.userConfig.at(0x24a)==5,"Failed submission changed initial state");
        unsigned limits=0;
        try { NativePS5::DecodePm4(std::vector<uint32_t>(65537,0x80000000)); }
        catch (const std::invalid_argument&) { ++limits; }
        auto fullState=parsed.finalState;
        fullState.shader.clear();
        for (uint32_t i=0;i<510;++i) fullState.shader[i]=i;
        // One context and one user register leave exactly 512 entries.
        try { NativePS5::DecodePm4(std::array<uint32_t,3>{0xc0017600,510,1},fullState); }
        catch (const std::invalid_argument&) { ++limits; }
        Require(fullState.shader.size()==510 && !fullState.shader.contains(510),"Failed register update escaped decoder");
        std::vector<uint32_t> manyDraws;
        for (unsigned i=0;i<128;++i) manyDraws.insert(manyDraws.end(),{0xc0012d00,1,2});
        Require(NativePS5::DecodePm4(manyDraws,parsed.finalState).draws.size()==128,"Valid draw limit rejected");
        manyDraws.insert(manyDraws.end(),{0xc0012d00,1,2});
        try { NativePS5::DecodePm4(manyDraws,parsed.finalState); }
        catch (const std::invalid_argument&) { ++limits; }
        Require(limits==3,"PM4 resource limits were not enforced");
        std::cout<<"PASS: PM4 register banks, immutable draw snapshots, submission state; 17 invalid/over-limit inputs rejected\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
