#include "guest_cpu.hpp"
#include "byte-fixtures.hpp"
#include <iostream>
#include <stdexcept>

using namespace NativePS5;
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
GuestCpu Seed() {
    GuestCpu cpu; cpu.flags=0x8d7; cpu.fuel=10;
    for (size_t i=0;i<16;++i) cpu.registers[i]=0xa5b6c7d8e9fa0123+0x0101010101010101*i;
    return cpu;
}
std::pair<unsigned,unsigned> ByteRegister(unsigned encoding,bool rex) {
    return !rex && encoding>=4?std::pair(encoding-4,8u):std::pair(encoding,0u);
}
void Check(const GuestCpu& cpu,const GuestCpu& before,const std::array<uint64_t,16>& expected) {
    Require(!cpu.fault && cpu.stop==CpuStop::Return,"Byte/extension instruction failed");
    Require(cpu.registers==expected && cpu.flags==before.flags,"Register bits or flags corrupted");
}
}
int main() {
    try {
        size_t checks=0;
        for (const auto& fixture:byteMoves) for (unsigned value=0;value<256;++value) {
            const auto [source,sourceBit]=ByteRegister(fixture.source,fixture.rex);
            const auto [destination,destinationBit]=ByteRegister(fixture.destination,fixture.rex);
            auto cpu=Seed();
            cpu.registers[source]=(cpu.registers[source]&~(uint64_t(255)<<sourceBit))|(uint64_t(value)<<sourceBit);
            const auto before=cpu;
            auto expected=cpu.registers;
            expected[destination]=(expected[destination]&~(uint64_t(255)<<destinationBit))|(uint64_t(value)<<destinationBit);
            fixture.block(&cpu); Check(cpu,before,expected); ++checks;
        }
        alignas(8) std::array<std::byte,8> bytes{};
        GuestMemory memory; memory.Add(bytes,true);
        const auto address=reinterpret_cast<uintptr_t>(bytes.data());
        for (const auto& fixture:extensionMoves) {
            const unsigned iterations=fixture.sourceBits==32?8:1u<<fixture.sourceBits;
            const uint64_t wide[]={0,1,0x7fffffff,0x80000000,0xffffffff,0x12345678,0x87654321,0xfffffffe};
            for (unsigned iteration=0;iteration<iterations;++iteration) {
                const uint64_t value=fixture.sourceBits==32?wide[iteration]:iteration;
                auto cpu=Seed(); cpu.memory=&memory;
                if (fixture.memory) {
                    cpu.registers[2]=address+1;
                    memory.Store(address+1,value,fixture.sourceBits/8);
                } else {
                    const uint64_t sourceMask=((uint64_t(1)<<fixture.sourceBits)-1)<<fixture.sourceBit;
                    cpu.registers[fixture.source]=(cpu.registers[fixture.source]&~sourceMask)|(value<<fixture.sourceBit);
                }
                const auto before=cpu;
                auto expected=cpu.registers;
                uint64_t result=value;
                if (fixture.sign && (value&(uint64_t(1)<<(fixture.sourceBits-1)))) result|=~((uint64_t(1)<<fixture.sourceBits)-1);
                expected[fixture.destination]=fixture.destinationBits==32?uint32_t(result):result;
                fixture.block(&cpu); Check(cpu,before,expected); ++checks;
            }
        }
        for (const auto& fixture:immediateMoves) {
            auto cpu=Seed(); const auto before=cpu; auto expected=cpu.registers;
            expected[fixture.destination]=(expected[fixture.destination]&~(uint64_t(255)<<fixture.bit))|(uint64_t(0x80)<<fixture.bit);
            fixture.block(&cpu); Check(cpu,before,expected); ++checks;
        }
        for (unsigned value=0;value<256;++value) {
            auto cpu=Seed(); cpu.memory=&memory; cpu.registers[2]=address+1;
            memory.Store(address+1,value,1); const auto before=cpu;
            auto expected=cpu.registers; expected[0]=(expected[0]&~uint64_t(0xff00))|(uint64_t(value)<<8);
            byte_memory_load(&cpu); Check(cpu,before,expected);
            memory.Store(address+1,0,1); const auto storeBefore=cpu;
            byte_memory_store(&cpu); Check(cpu,storeBefore,expected);
            Require(memory.Load(address+1,1)==value,"Byte store wrote wrong register slice"); checks+=2;
        }
        GuestMemory readonly; readonly.Add(bytes,false);
        auto bad=Seed(); bad.memory=&readonly; bad.registers[2]=address;
        const auto snapshot=bytes;
        const auto registers=bad.registers;
        byte_memory_store(&bad);
        Require(bad.fault==2 && bytes==snapshot && bad.registers==registers && bad.flags==0x8d7,"Failed byte store changed state");
        for (const auto& fixture:extensionMoves) if (fixture.memory) {
            auto cpu=Seed(); cpu.memory=&memory; cpu.registers[2]=address+8;
            const auto before=cpu;
            fixture.block(&cpu);
            Require(cpu.fault==1 && cpu.registers==before.registers && cpu.flags==before.flags,"Faulting extension changed state");
        }
        std::cout<<"PASS: byte register encodings, preserved bits, MOVZX/MOVSX/MOVSXD and guarded memory; "<<checks<<" checks\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
