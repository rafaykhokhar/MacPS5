#include "guest_cpu.hpp"
#include "logical-fixtures.hpp"
#include <bit>
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
uint64_t Mask(unsigned bits) { return bits==64?UINT64_MAX:(uint64_t(1)<<bits)-1; }
uint64_t Expected(unsigned op,uint64_t a,uint64_t b) {
    if (op==0 || op==3) return a&b;
    if (op==1) return a|b;
    if (op==2) return a^b;
    return ~a;
}
}
int main() {
    try {
        const uint64_t values[]={0,1,0x7f,0x80,0xff,0x7fffffff,0x80000000,0xffffffff,
            0x8000000000000000,UINT64_MAX,0xfedcba9876543210};
        std::array<std::byte,16> bytes{}; GuestMemory memory; memory.Add(bytes,true);
        const auto address=reinterpret_cast<uintptr_t>(bytes.data());
        size_t checks=0;
        for (const auto& fixture:logicalFixtures) {
            unsigned total=fixture.bits==8?256:std::size(values);
            for (unsigned ai=0;ai<total;++ai) for (unsigned bi=0;bi<(fixture.form==3 || fixture.operation==4?1:total);++bi) {
                uint64_t a=fixture.bits==8?ai:values[ai],b=fixture.bits==8?bi:values[bi];
                if (fixture.form==3) b=fixture.immediate;
                GuestCpu cpu; cpu.memory=&memory; cpu.fuel=10; cpu.flags=0xcd7;
                cpu.registers[0]=0x13579bdf24680000|(a&0xffff);
                if (fixture.bits!=8) cpu.registers[0]=a;
                cpu.registers[1]=b; cpu.registers[2]=address+1;
                memory.Store(address+1,fixture.form==1?a:b,fixture.bits/8);
                const auto before=cpu; const auto snapshot=bytes;
                const auto result=Expected(fixture.operation,a,b)&Mask(fixture.bits);
                fixture.block(&cpu);
                Require(!cpu.fault && cpu.stop==CpuStop::Return,"Logical instruction failed");
                auto expected=before.registers;
                if (fixture.operation!=3 && fixture.form!=1)
                    expected[0]=fixture.bits==8?(before.registers[0]&~uint64_t(255))|result:result;
                Require(cpu.registers==expected,"Logical instruction changed wrong register bits");
                if (fixture.form==1 && fixture.operation!=3) Require(memory.Load(address+1,fixture.bits/8)==result,"Logical memory result mismatch");
                else Require(bytes==snapshot,"Read-only logical operand was modified");
                if (fixture.operation==4) Require(cpu.flags==before.flags,"NOT modified flags");
                else {
                    uint64_t flags=before.flags&~uint64_t(0x8c5);
                    if (!result) flags|=0x40;
                    if (result&(uint64_t(1)<<(fixture.bits-1))) flags|=0x80;
                    if (!(std::popcount(static_cast<unsigned>(result&255))&1)) flags|=4;
                    Require((cpu.flags&~uint64_t(0x10))==(flags&~uint64_t(0x10)),"Logical flags mismatch");
                }
                ++checks;
            }
        }
        GuestMemory readonly; readonly.Add(bytes,false);
        for (const auto& fixture:logicalFixtures) if (fixture.form==1) {
            GuestCpu cpu; cpu.memory=&readonly; cpu.fuel=10; cpu.flags=0xcd7; cpu.registers[2]=address;
            const auto snapshot=bytes;
            fixture.block(&cpu);
            if (fixture.operation==3) Require(!cpu.fault,"TEST required write permission");
            else Require(cpu.fault==2 && cpu.flags==0xcd7 && bytes==snapshot,"Faulting logical write changed state");
        }
        GuestCpu high; high.fuel=10; high.flags=0xcd7; high.registers[0]=0x123456789abcdef0;
        logical_not_ah(&high);
        Require(high.registers[0]==0x123456789abc21f0 && high.flags==0xcd7,"NOT high-byte preservation failed");
        for (const auto& fixture:bmiShiftFixtures) for (uint64_t value:values) for (unsigned count=0;count<256;++count) {
            GuestCpu cpu; cpu.fuel=10; cpu.flags=0xcd7; cpu.registers[1]=0x8765432100000000|count; cpu.registers[2]=value;
            uint64_t expected=value&Mask(fixture.bits);
            for (unsigned i=0;i<(count&(fixture.bits-1));++i) {
                if (fixture.operation==0) expected=(expected<<1)&Mask(fixture.bits);
                else {
                    const auto sign=expected&(uint64_t(1)<<(fixture.bits-1));
                    expected>>=1;
                    if (fixture.operation==2) expected|=sign;
                }
            }
            fixture.block(&cpu);
            Require(!cpu.fault && cpu.registers[0]==expected && cpu.flags==0xcd7 && cpu.registers[2]==value,"BMI shift result/flags mismatch");
            ++checks;
        }
        std::cout<<"PASS: native bitwise operations and flag-preserving BMI shifts; "<<checks<<" checks including exhaustive byte inputs and memory permissions\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
