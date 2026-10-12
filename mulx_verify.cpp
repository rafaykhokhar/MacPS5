#include "guest_cpu.hpp"
#include "mulx-fixtures.hpp"
#include <cstring>
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 size_t checks=0;uint64_t random=0x123456789abcdef0;
 std::array<std::byte,16> bytes{};GuestMemory memory;memory.Add(bytes,false);
 constexpr std::array<uint64_t,8> boundary{0,1,2,UINT32_MAX,UINT64_MAX,1ULL<<63,0xaaaaaaaaaaaaaaaa,0x5555555555555555};
 for(const auto& f:mulxFixtures)for(unsigned i=0;i<4096;++i){
  GuestCpu cpu;cpu.memory=&memory;cpu.fuel=10;cpu.flags=i;
  for(auto& reg:cpu.registers){random^=random<<13;random^=random>>7;random^=random<<17;reg=random;}
  if(i<64){cpu.registers[2]=boundary[i/8];cpu.registers[f.source]=boundary[i%8];}
  auto source=cpu.registers[f.source];
  const auto address=reinterpret_cast<uintptr_t>(bytes.data())+1;
  if(f.memory){std::memcpy(bytes.data()+1,&source,f.bits/8);cpu.registers[f.source]=address;}
  const auto before=cpu;const auto snapshot=bytes;
  const uint64_t mask=f.bits==64?UINT64_MAX:UINT32_MAX;
  const auto a=cpu.registers[2]&mask,b=source&mask;
  unsigned __int128 product=0;
  for(unsigned bit=0;bit<f.bits;++bit)if((b>>bit)&1)product+=static_cast<unsigned __int128>(a)<<bit;
  auto expected=before.registers;expected[f.low]=static_cast<uint64_t>(product)&mask;expected[f.high]=static_cast<uint64_t>(product>>f.bits);
  f.block(&cpu);
  Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected,"MULX result or alias mismatch");
  Require(cpu.flags==before.flags && bytes==snapshot,"MULX changed flags or source bytes");++checks;
  if(f.memory){
   cpu=before;cpu.registers[f.source]=address+14;const auto registers=cpu.registers;
   f.block(&cpu);Require(cpu.fault && cpu.registers==registers && cpu.flags==before.flags && bytes==snapshot,"MULX failed load changed state");++checks;
  }
 }
 std::cout<<"PASS: MULX unsigned products, both widths, aliases, flags and guarded memory; "<<checks<<" bit-serial reference checks\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
