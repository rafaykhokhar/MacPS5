#include "guest_cpu.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
extern "C" void cpu_indexed_load(GuestCpu*);
extern "C" void cpu_memory_add(GuestCpu*);
namespace {
void Require(bool b,const char* s) { if(!b) throw std::runtime_error(s); }
template<class F> void Reject(F action) {
 bool rejected=false;try {action();} catch(const std::runtime_error&) {rejected=true;}
 Require(rejected,"Invalid virtual mapping/access accepted");
}
}
int main() {
 try {
  constexpr uint64_t kernel=0xffff800000100000,codeBase=0x400000,stackBase=0x700000;
  std::array<std::byte,128> storage{},code{},stack{},other{};
  GuestMemory memory;
  memory.AddAt(kernel,storage,true);
  memory.AddAt(codeBase,code,false,true);
  memory.AddAt(stackBase,stack,true);
  size_t checks=0;
  for (unsigned width:{1,2,4,8}) for(unsigned offset=0;offset<=storage.size()-width;++offset) {
   memory.Store(kernel+offset,0xfedcba9876543210,width);
   const uint64_t mask=width==8?UINT64_MAX:(uint64_t(1)<<(width*8))-1;
   Require(memory.Load(kernel+offset,width)==(0xfedcba9876543210&mask),"Virtual scalar round trip failed");++checks;
  }
  std::array<std::byte,32> vector{},read{};
  for(size_t i=0;i<vector.size();++i) vector[i]=std::byte(i*7);
  for(unsigned width:{16,32}) {
   memory.WriteVector(kernel+3,std::span(vector).first(width));
   memory.ReadVector(kernel+3,std::span(read).first(width));
   Require(std::equal(read.begin(),read.begin()+width,vector.begin()),"Virtual vector transfer failed");++checks;
  }
  memory.Store(kernel+4,0xfedcba9876543210,8);
  GuestCpu cpu;cpu.memory=&memory;cpu.fuel=8;cpu.registers[3]=kernel;cpu.registers[1]=3;
  cpu_indexed_load(&cpu);
  Require(!cpu.fault && cpu.registers[0]==0xfedcba9876543210,"Translated SIB guest address was not resolved");++checks;
  cpu={};cpu.memory=&memory;cpu.fuel=8;cpu.registers[3]=kernel+4;cpu.registers[0]=5;
  cpu_memory_add(&cpu);
  Require(!cpu.fault && memory.Load(kernel+4,8)==0xfedcba9876543215,"Translated arithmetic failed at virtual address");++checks;
  cpu={};cpu.memory=&memory;cpu.loadBias=codeBase;cpu.target=codeBase+16;cpu.stop=CpuStop::Call;cpu.registers[4]=stackBase+64;
  Require(EnterGuestCall(cpu,codeBase+32) && cpu.rip==16 && cpu.registers[4]==stackBase+56,"Virtual guest call failed");
  Require(memory.Load(stackBase+56,8)==codeBase+32,"Guest return address corrupted");
  cpu.stop=CpuStop::Return;
  Require(CompleteGuestReturn(cpu) && cpu.rip==32 && cpu.registers[4]==stackBase+64,"Virtual guest return failed");++checks;
  const auto snapshot=storage;
  Reject([&]{memory.AddAt(kernel+64,other,true);});
  Reject([&]{memory.AddAt(0x900000,storage,false);});
  Reject([&]{memory.AddAt(0x900000,std::span(code).subspan(8,16),true);});
  Reject([&]{memory.AddAt(UINT64_MAX-16,other,true);});
  Reject([&]{memory.AddAt(0x900000,{},true);});
  Reject([&]{memory.Load(reinterpret_cast<uintptr_t>(storage.data()),8);});
  Reject([&]{memory.Store(kernel+127,0,8);});
  Reject([&]{memory.WriteVector(kernel+120,vector);});
  Reject([&]{memory.Store(codeBase,0,1);});
  Require(storage==snapshot,"Rejected access changed backing memory");checks+=9;
  // Failed mapping attempts must not reserve the new guest address.
  memory.AddAt(0x900000,other,true);memory.Store(0x900000,42,8);
  Require(memory.Load(0x900000,8)==42,"Mapping failure was not transactional");++checks;
  std::cout<<"PASS: virtual guest addresses, native translated accesses, calls/returns and mapping rejection; "<<checks<<" checks\n";
 }catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}
}
