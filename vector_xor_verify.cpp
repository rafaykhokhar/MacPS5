#include "guest_cpu.hpp"
#include "vector-xor-fixtures.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 std::array<std::byte,128> bytes{};GuestMemory memory;memory.AddAt(0x900000,bytes,false);
 size_t checks=0;
 for(const auto& f:vectorXorFixtures)for(unsigned offset=0;offset<32;++offset){
  GuestCpu cpu;cpu.memory=&memory;cpu.fuel=10;cpu.flags=0xcd7;cpu.registers[0]=0x900000+offset;
  for(size_t r=0;r<16;++r)for(size_t b=0;b<32;++b)cpu.vectors[r][b]=std::byte((r*73+b*19+offset*37)&255);
  for(size_t i=0;i<bytes.size();++i)bytes[i]=std::byte((i*13+offset*41)&255);
  const auto before=cpu;const auto snapshot=bytes;auto expected=cpu.vectors;
  expected[f.destination].fill(std::byte(0));
  for(size_t i=0;i<f.size;++i)expected[f.destination][i]=before.vectors[f.first][i]^(f.memory?bytes[offset+i]:before.vectors[f.second][i]);
  f.block(&cpu);
  Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.vectors==expected,"Vector XOR result or upper clearing failed");
  Require(cpu.registers==before.registers && cpu.flags==before.flags && bytes==snapshot,"Vector XOR modified unrelated state");++checks;
  if(f.memory){
   cpu=before;cpu.registers[0]=0x900000+127;const auto registers=cpu.registers;
   f.block(&cpu);
   Require(cpu.fault==1 && cpu.vectors==before.vectors && cpu.registers==registers && cpu.flags==before.flags && bytes==snapshot,"Faulting vector XOR changed state");++checks;
  }
 }
 std::cout<<"PASS: vector XOR native NEON, aliases, widths, unaligned memory and fault preservation; "<<checks<<" checks\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
