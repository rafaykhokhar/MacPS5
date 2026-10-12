#include "guest_cpu.hpp"
#include "conditional-fixtures.hpp"
#include <iostream>
#include <stdexcept>

using namespace NativePS5;
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
bool Reference(unsigned condition,unsigned mask) {
    const bool carry=mask&1,parity=mask&2,zero=mask&4,sign=mask&8,overflow=mask&16;
    const bool predicates[]={overflow,carry,zero,carry||zero,sign,parity,sign!=overflow,zero||sign!=overflow};
    return predicates[condition/2] != bool(condition&1);
}
uint64_t Flags(unsigned mask) {
    return 0x612|(mask&1)|((mask&2)<<1)|((mask&4)<<4)|((mask&8)<<4)|((mask&16)<<7);
}
GuestCpu Seed(unsigned mask) {
    GuestCpu cpu; cpu.flags=Flags(mask); cpu.fuel=10;
    for (size_t i=0;i<16;++i) cpu.registers[i]=0xfedcba9876543210+i*0x0101010101010101;
    return cpu;
}
}
int main() {
    try {
        std::array<std::byte,16> bytes{};
        GuestMemory memory; memory.Add(bytes,true);
        const auto address=reinterpret_cast<uintptr_t>(bytes.data());
        constexpr uint64_t source=0x8123456789abcdef;
        unsigned checks=0;
        for (const auto& fixture:conditionalMoves) for (unsigned mask=0;mask<32;++mask) {
            memory.Store(address+1,source,8);
            auto cpu=Seed(mask); cpu.memory=&memory; cpu.registers[2]=address+1;
            const auto before=cpu;
            auto expected=cpu.registers;
            uint64_t chosen=Reference(fixture.condition,mask)?(fixture.memory?source:before.registers[fixture.source]):before.registers[fixture.destination];
            expected[fixture.destination]=fixture.bits==32?uint32_t(chosen):chosen;
            fixture.block(&cpu);
            Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected && cpu.flags==before.flags,"Conditional move result/flags mismatch");
            ++checks;
            if (fixture.memory) {
                cpu=before; cpu.registers[2]=address+16;
                const auto snapshot=cpu;
                fixture.block(&cpu);
                Require(cpu.fault==1 && cpu.flags==snapshot.flags && cpu.registers==snapshot.registers,"CMOV suppressed a memory fault or changed state");
                ++checks;
            }
        }
        for (const auto& fixture:conditionalBytes) for (unsigned mask=0;mask<32;++mask) {
            memory.Store(address+1,0xff,1);
            auto cpu=Seed(mask); cpu.memory=&memory; cpu.registers[2]=address+1;
            const auto before=cpu;
            auto expected=cpu.registers;
            const uint64_t value=Reference(fixture.condition,mask);
            if (!fixture.memory)
                expected[fixture.destination]=(expected[fixture.destination]&~(uint64_t(255)<<fixture.bit))|(value<<fixture.bit);
            fixture.block(&cpu);
            Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected && cpu.flags==before.flags,"SETcc partial-register result/flags mismatch");
            if (fixture.memory) Require(memory.Load(address+1,1)==value,"SETcc memory result mismatch");
            ++checks;
        }
        GuestMemory readonly; readonly.Add(bytes,false);
        for (const auto& fixture:conditionalBytes) if (fixture.memory) for (unsigned mask=0;mask<32;++mask) {
            auto cpu=Seed(mask); cpu.memory=&readonly; cpu.registers[2]=address;
            const auto before=cpu; const auto snapshot=bytes;
            fixture.block(&cpu);
            Require(cpu.fault==2 && cpu.flags==before.flags && cpu.registers==before.registers && bytes==snapshot,"Faulting SETcc altered guest state");
            ++checks;
        }
        std::cout<<"PASS: all 16 CMOV/SETcc conditions and all flag combinations; "<<checks<<" native checks including false-condition faults and partial registers\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
