#include "guest_cpu.hpp"
#include "shift-fixtures.hpp"
#include <bit>
#include <iostream>
#include <stdexcept>

using namespace NativePS5;
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
std::pair<uint64_t,uint64_t> Reference(uint64_t value,uint64_t count,unsigned bits,unsigned op,uint64_t flags) {
    const auto mask=bits==64?UINT64_MAX:uint64_t(UINT32_MAX);
    const auto high=uint64_t(1)<<(bits-1);
    value&=mask; count&=bits-1;
    if (!count) return {value,flags};
    bool originalSign=value&high,carry=false;
    // Independent bit-serial reference, rather than the native emitter's
    // variable shifts or the flag helper's extraction formulas.
    for (uint64_t i=0;i<count;++i) {
        if (op==0 || op==3) {
            carry=value&high;
            value=((value<<1)&mask)|(op==3 && carry?1:0);
        } else {
            carry=value&1;
            const bool sign=value&high;
            value>>=1;
            if ((op==2 && sign) || (op==4 && carry)) value|=high;
        }
    }
    flags=(flags&~uint64_t(1))|uint64_t(carry);
    if (op<3) {
        flags&=~uint64_t(0xc4);
        if (!value) flags|=0x40;
        if (value&high) flags|=0x80;
        unsigned ones=0;
        for (unsigned bit=0;bit<8;++bit) ones+=bool(value&(uint64_t(1)<<bit));
        if (!(ones&1)) flags|=4;
    }
    if (count==1) {
        bool overflow=op==1?originalSign:(op==2?false:originalSign!=bool(value&high));
        flags=(flags&~uint64_t(0x800))|(overflow?0x800:0);
    }
    return {value,flags};
}
}
int main() {
    try {
        const uint64_t values[]={0,1,2,0x7fffffff,0x80000000,0xffffffff,0x100000000,
            0x7fffffffffffffff,0x8000000000000000,UINT64_MAX,0x123456789abcdef0,0xfedcba9876543210};
        size_t checks=0;
        for (const auto& fixture:shiftFixtures) for (auto value:values) for (uint64_t flags:{uint64_t(2),uint64_t(0x8d7)}) {
            const unsigned countEnd=fixture.count<0?256:1;
            for (unsigned iteration=0;iteration<countEnd;++iteration) {
                const uint64_t count=fixture.count<0?iteration:fixture.count;
                GuestCpu cpu; cpu.flags=flags; cpu.fuel=10;
                cpu.registers[0]=value; cpu.registers[1]=0x123456789abcde00|count;
                const auto expected=Reference(value,count,fixture.bits,fixture.operation,flags);
                fixture.block(&cpu);
                Require(!cpu.fault && cpu.stop==CpuStop::Return,"Shift fixture did not return");
                Require(cpu.registers[0]==expected.first && cpu.flags==expected.second,"Native shift/rotate result or flags mismatch");
                Require(cpu.registers[1]==(0x123456789abcde00|count),"Shift modified count register");
                ++checks;
            }
        }
        // CL is read before updating a destination which aliases RCX.
        GuestCpu alias; alias.registers[1]=0x8123456789abcdef; alias.flags=0x8d7; alias.fuel=10;
        const auto expected=Reference(alias.registers[1],alias.registers[1],64,0,alias.flags);
        shift_alias(&alias);
        Require(alias.registers[1]==expected.first && alias.flags==expected.second,"Shift count/destination alias failed");
        GuestCpu high; high.registers[11]=0x8000000000000001; high.registers[1]=63; high.fuel=10;
        shift_high(&high);
        Require(!high.fault && high.registers[11]==3,"Extended-register rotate mismatch");
        alignas(8) std::array<std::byte,16> bytes{};
        const auto address=reinterpret_cast<uintptr_t>(bytes.data());
        GuestMemory writable; writable.Add(bytes,true);
        writable.Store(address,0x8000000000000000,8);
        GuestCpu memory; memory.memory=&writable; memory.registers[2]=address; memory.registers[1]=1; memory.flags=0x8d7; memory.fuel=10;
        shift_memory(&memory);
        Require(!memory.fault && writable.Load(address,8)==0xc000000000000000,"Memory arithmetic shift failed");
        GuestMemory readOnly; readOnly.Add(bytes,false);
        memory.memory=&readOnly; memory.stop=CpuStop::None; memory.flags=0x8d7;
        const auto snapshot=bytes; shift_memory(&memory);
        Require(memory.fault==2 && memory.flags==0x8d7 && bytes==snapshot,"Faulting shift store changed flags or memory");
        memory.fault=0; memory.memory=&writable; memory.registers[2]=address+12; memory.fuel=10;
        shift_memory(&memory);
        Require(memory.fault==1 && bytes==snapshot,"Out-of-range shift access changed memory");
        std::cout<<"PASS: native 32/64-bit shifts and rotates against bit-serial reference; "<<checks<<" result/flag checks; aliases and memory faults\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
