#include "guest_cpu.hpp"
#include "movbe-fixtures.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
int main() {
    try {
        size_t checks=0;
        for (const auto& fixture:movbeFixtures) {
            std::array<std::byte,32> bytes{};
            GuestMemory memory; memory.Add(bytes,true);
            uint64_t random=0x123456789abcdef0;
            for (unsigned i=0;i<(fixture.bits==16?65536u:4096u);++i) {
                random^=random<<13; random^=random>>7; random^=random<<17;
                const auto value=fixture.bits==16?i:random;
                const auto mask=fixture.bits==64?UINT64_MAX:(uint64_t(1)<<fixture.bits)-1;
                uint64_t reversed=0;
                for (unsigned b=0;b<fixture.bits/8;++b) reversed=(reversed<<8)|((value>>(8*b))&255);
                const auto address=reinterpret_cast<uintptr_t>(bytes.data())+(i%8);
                bytes.fill(std::byte(0xab));
                memory.Store(address,value,fixture.bits/8);
                GuestCpu cpu; cpu.memory=&memory; cpu.fuel=10; cpu.flags=0xcd7;
                cpu.registers[0]=fixture.store?value:0x123456789abcdef0;
                cpu.registers[1]=address;
                auto expected=cpu.registers; const auto snapshot=bytes;
                fixture.block(&cpu);
                Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.flags==0xcd7,"MOVBE execution/flags failed");
                if (fixture.store) {
                    Require(memory.Load(address,fixture.bits/8)==reversed,"MOVBE store byte order failed");
                    for (size_t b=0;b<bytes.size();++b)
                        if (b<i%8 || b>=i%8+fixture.bits/8) Require(bytes[b]==snapshot[b],"MOVBE store escaped operand");
                } else {
                    expected[0]=fixture.bits==16?(expected[0]&~mask)|reversed:reversed;
                    Require(bytes==snapshot,"MOVBE load wrote memory");
                }
                Require(cpu.registers==expected,"MOVBE register preservation failed");
                ++checks;
            }
            for (bool readonly:{false,true}) {
                GuestMemory guarded; guarded.Add(bytes,!readonly);
                GuestCpu cpu; cpu.memory=&guarded; cpu.fuel=10; cpu.flags=0xcd7;
                cpu.registers[0]=UINT64_MAX;
                cpu.registers[1]=reinterpret_cast<uintptr_t>(bytes.data())+(readonly?0:bytes.size()-1);
                const auto snapshot=bytes; const auto registers=cpu.registers;
                fixture.block(&cpu);
                if (readonly && !fixture.store) Require(!cpu.fault,"MOVBE readonly load rejected");
                else Require(cpu.fault && cpu.registers==registers,"MOVBE failed access changed registers");
                Require(bytes==snapshot && cpu.flags==0xcd7,"MOVBE failed access changed memory/flags");
                ++checks;
            }
        }
        std::cout<<"PASS: MOVBE loads/stores, partial registers, unaligned access and guarded failures; "<<checks<<" checks\n";
    } catch (const std::exception& e) { std::cerr<<"FAIL: "<<e.what()<<'\n'; return 1; }
}
