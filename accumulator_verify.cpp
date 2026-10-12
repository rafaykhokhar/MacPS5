#include "guest_cpu.hpp"
#include "accumulator-fixtures.hpp"
#include <bit>
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s) {if(!b) throw std::runtime_error(s);}
int main() {
 try {
  size_t checks=0;
  const uint64_t values[]={0,1,15,16,0x7fffffff,0x80000000,0xffffffff,0x7fffffffffffffff,0x8000000000000000,UINT64_MAX};
  for(const auto& f:accumulatorFixtures) for(unsigned i=0;i<(f.bits==8?256:std::size(values));++i) {
   const uint64_t mask=f.bits==64?UINT64_MAX:(uint64_t(1)<<f.bits)-1;
   const uint64_t a=(f.bits==8?i:values[i])&mask,b=f.immediate&mask;
   const auto result=(f.operation==0?a+b:a-b)&mask;
   GuestCpu cpu;cpu.fuel=10;cpu.flags=0x602;
   cpu.registers.fill(0xabcdef0123456789);cpu.registers[0]=f.bits==8?0xabcdef0123456700|a:a;
   const auto before=cpu.registers;
   f.block(&cpu);
   auto expected=before;
   if(f.operation!=2) expected[0]=f.bits==8?(before[0]&~uint64_t(255))|result:result;
   Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers==expected,"Accumulator destination semantics failed");
   const __int128 modulus=__int128(1)<<f.bits;
   const auto signedValue=[&] (uint64_t v)->__int128 {return v&(uint64_t(1)<<(f.bits-1))?__int128(v)-modulus:__int128(v);};
   const auto signedResult=f.operation==0?signedValue(a)+signedValue(b):signedValue(a)-signedValue(b);
   const bool carry=f.operation==0?__uint128_t(a)+b>mask:a<b;
   const bool overflow=signedResult<-(modulus/2)||signedResult>=modulus/2;
   const bool auxiliary=f.operation==0?(a&15)+(b&15)>15:(a&15)<(b&15);
   const uint64_t flags=0x602|carry|(std::popcount(unsigned(result&255))%2==0?4:0)|(auxiliary?16:0)|(result==0?64:0)|((result>>(f.bits-1))<<7)|(overflow?0x800:0);
   Require(cpu.flags==flags,"Accumulator arithmetic flags failed");++checks;
  }
  std::cout<<"PASS: accumulator ADD/SUB/CMP results, preservation and flags; "<<checks<<" checks\n";
 }catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}
}
