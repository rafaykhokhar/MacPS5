#include "guest_cpu.hpp"
#include "rorx-fixtures.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s) { if(!b) throw std::runtime_error(s); }
int main() {
 try {
  size_t checks=0; std::array<std::byte,16> bytes{};GuestMemory memory;memory.Add(bytes,false);
  const uint64_t values[]={0,1,UINT64_MAX,0x80000000,0x8000000000000000,0x123456789abcdef0};
  for (const auto& f:rorxFixtures) for (auto value:values) {
   GuestCpu cpu;cpu.memory=&memory;cpu.fuel=10;cpu.flags=0xcd7;
   cpu.registers[f.source]=value;
   if(f.memory) {
    for(unsigned i=0;i<f.bits/8;++i) bytes[i+1]=std::byte(value>>(8*i));
    cpu.registers[f.source]=reinterpret_cast<uintptr_t>(bytes.data())+1;
   }
   const auto before=cpu;const auto snapshot=bytes;
   const uint64_t mask=f.bits==64?UINT64_MAX:0xffffffff;
   auto result=value&mask;
   for(unsigned i=0;i<(f.count&(f.bits-1));++i) result=(result>>1)|((result&1)<<(f.bits-1));
   auto expected=before.registers;expected[f.destination]=result;
   f.block(&cpu);
   Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected,"RORX result or registers failed");
   Require(cpu.flags==before.flags && bytes==snapshot,"RORX modified flags or memory");++checks;
   if(f.memory) {
    cpu=before;cpu.registers[f.source]=reinterpret_cast<uintptr_t>(bytes.data())+15;expected=cpu.registers;
    f.block(&cpu);Require(cpu.fault && cpu.registers==expected && cpu.flags==before.flags && bytes==snapshot,"RORX failed read changed state");++checks;
   }
  }
  std::cout<<"PASS: RORX all immediate counts, widths, aliases, extended registers and guarded loads; "<<checks<<" checks\n";
 } catch(const std::exception& e) {std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}
}
