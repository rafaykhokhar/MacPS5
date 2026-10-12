#include "guest_cpu.hpp"
#include <iostream>
#include <stdexcept>

using namespace NativePS5;
extern "C" void bextr32(GuestCpu*);
extern "C" void bextr64(GuestCpu*);
extern "C" void bextr_alias(GuestCpu*);
extern "C" void bextr_control_alias(GuestCpu*);
extern "C" void bextr_high(GuestCpu*);
extern "C" void bextr_memory(GuestCpu*);
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
uint64_t Reference(uint64_t value,unsigned start,unsigned length,unsigned bits) {
    uint64_t result=0;
    for (unsigned i=0;i<bits;++i) {
        if (i>=start && i-start<length && (value&(uint64_t(1)<<i))) result|=uint64_t(1)<<(i-start);
    }
    return result;
}
}
int main() {
    try {
        size_t checks=0;
        for (unsigned bits:{32u,64u}) for (uint64_t value:{uint64_t(0),UINT64_MAX,uint64_t(0x8000000080000001),uint64_t(0xa5f097e132684bdc)}) {
            for (unsigned control=0;control<65536;++control) {
                GuestCpu cpu; cpu.fuel=10; cpu.flags=0xed7;
                cpu.registers[0]=0xdeadbeef12345678;
                cpu.registers[1]=0xbadf00d123450000|control;
                cpu.registers[2]=value;
                const auto expected=Reference(value,control&255,control>>8,bits);
                (bits==32?bextr32:bextr64)(&cpu);
                Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers[0]==expected,"Native BEXTR result mismatch");
                const uint64_t flags=(0xed7&~uint64_t(0x841))|(expected?0:0x40);
                Require(cpu.flags==flags && cpu.registers[2]==value && cpu.registers[1]==(0xbadf00d123450000|control),"BEXTR flags or sources changed");
                ++checks;
            }
        }
        GuestCpu alias; alias.fuel=10; alias.registers[0]=0xfedcba98; alias.registers[1]=0x1004;
        bextr_alias(&alias);
        Require(alias.registers[0]==Reference(0xfedcba98,4,16,32),"BEXTR source/destination alias failed");
        alias={}; alias.fuel=10; alias.registers[0]=0x4000; alias.registers[2]=0xfedcba9876543210;
        bextr_control_alias(&alias);
        Require(alias.registers[0]==alias.registers[2],"BEXTR control/destination alias or full-width extraction failed");
        GuestCpu high; high.fuel=10; high.registers[9]=0x81; high.registers[15]=0xdeadbeef;
        bextr_high(&high);
        Require(!high.fault && high.registers[8]==0 && (high.flags&0x40),"BEXTR extended registers failed");
        high={}; high.fuel=10; high.registers[9]=0x3f01; high.registers[15]=0xdeadbeef;
        bextr_high(&high);
        Require(high.registers[8]==0x6f56df77,"BEXTR extended source/control decode failed");
        std::array<std::byte,16> bytes{};
        GuestMemory memory; memory.Add(bytes,true);
        const auto address=reinterpret_cast<uintptr_t>(bytes.data());
        memory.Store(address+1,0xfedcba9876543210,8);
        GuestCpu load; load.memory=&memory; load.registers[2]=address+1; load.registers[1]=0x801; load.fuel=10;
        bextr_memory(&load);
        Require(!load.fault && load.registers[0]==8,"Unaligned BEXTR memory source failed");
        load.fault=0; load.fuel=10; load.registers[2]=address+12; load.registers[0]=123; load.flags=0xed7;
        bextr_memory(&load);
        Require(load.fault==1 && load.registers[0]==123 && load.flags==0xed7,"Faulting BEXTR changed state");
        std::cout<<"PASS: native BEXTR 32/64-bit extraction; "<<checks<<" all-control-field reference checks; aliases, extended registers and memory faults\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
