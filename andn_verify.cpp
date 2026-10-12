#include "guest_cpu.hpp"
#include "andn-fixtures.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s) { if (!b) throw std::runtime_error(s); }
int main() {
 try {
  size_t checks=0;
  std::array<std::byte,16> bytes{}; GuestMemory memory; memory.Add(bytes,true);
  uint64_t random=0x123456789abcdef0;
  for (const auto& f:andnFixtures) for (unsigned i=0;i<4096;++i) {
   GuestCpu cpu;cpu.memory=&memory;cpu.fuel=10;cpu.flags=0xcd7;
   for (auto& reg:cpu.registers) { random^=random<<13;random^=random>>7;random^=random<<17;reg=random; }
   if (i<4) { cpu.registers[f.first]=i&1?0:UINT64_MAX;cpu.registers[f.second]=i&2?0:UINT64_MAX; }
   const auto address=reinterpret_cast<uintptr_t>(bytes.data())+1;
   uint64_t second=cpu.registers[f.second];
   if (f.memory) { memory.Store(address,second,f.bits/8);cpu.registers[f.second]=address; }
   const auto before=cpu;const auto snapshot=bytes;
   const uint64_t mask=f.bits==64?UINT64_MAX:0xffffffff;
   const auto result=(~cpu.registers[f.first]&second)&mask;
   f.block(&cpu);
   auto expected=before.registers;expected[f.destination]=result;
   const auto flags=(before.flags&~uint64_t(0x8c1))|(result==0?0x40:0)|((result>>(f.bits-1))<<7);
   Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected,"ANDN operands or aliases failed");
   Require(cpu.flags==flags && bytes==snapshot,"ANDN flags or memory preservation failed");++checks;
   if (f.memory) {
    cpu=before;cpu.registers[f.second]=address+bytes.size()-2;auto registers=cpu.registers;
    f.block(&cpu);Require(cpu.fault && cpu.registers==registers && cpu.flags==before.flags && bytes==snapshot,"ANDN memory failure not atomic");++checks;
   }
  }
  std::cout<<"PASS: ANDN widths, flags, aliases, extended registers and guarded memory; "<<checks<<" checks\n";
 } catch(const std::exception& e) { std::cerr<<"FAIL: "<<e.what()<<'\n';return 1; }
}
