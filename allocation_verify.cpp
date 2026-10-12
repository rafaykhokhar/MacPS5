#include "bootstrap_services.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
extern "C" void metadata_syscall(GuestCpu*);
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 GuestMemory memory;std::array<std::byte,0x4000> code{};std::array<std::byte,256> stack{};std::array<std::byte,8> slot{};
 memory.AddAt(0x400000,code,false,true);memory.AddAt(0x700000,stack,true);memory.AddAt(0x400000+0x58f890,slot,true);
 BootstrapServices services(memory);memory.Store(0x400000+0x58f890,services.SyscallAddress(),8);
 unsigned checks=0;
 auto call=[&](uint64_t number,uint64_t a,uint64_t b,uint64_t c=3,uint64_t d=0x1002,uint64_t e=UINT32_MAX,uint64_t f=0){
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=0x400000;cpu.fuel=50;cpu.rip=0x380;cpu.flags=0x246;
  cpu.registers[4]=0x700080;cpu.registers[7]=number;cpu.registers[6]=a;cpu.registers[2]=b;
  cpu.registers[1]=c;cpu.registers[8]=d;cpu.registers[9]=e;memory.Store(0x700088,f,8);
  metadata_syscall(&cpu);Require(cpu.stop==CpuStop::Call && services.Dispatch(cpu),"Allocation syscall dispatch failed");
  metadata_syscall(&cpu);Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers[4]==0x700080,"Allocation CRT return failed");++checks;
  Require(!(cpu.flags&1) || cpu.registers[0]==UINT64_MAX,"Allocation error not converted to -1");
  return IoResult{cpu.registers[0],uint32_t((cpu.flags&1)?services.ErrorValue():0)};
 };
 auto mapped=call(477,0,16385);Require(!mapped.error && !(mapped.value%16384) && services.AllocatedBytes()==32768,"Mapping rounding/accounting failed");
 for(uint64_t i=0;i<32768;++i)Require(memory.Load(mapped.value+i,1)==0,"Anonymous mapping not zeroed");
 memory.Store(mapped.value+32767,0xab,1);
 Require(call(73,mapped.value,16384).error==22 && memory.Load(mapped.value+32767,1)==0xab,"Partial unmap changed mapping");
 Require(call(73,mapped.value+1,16385).error==22,"Unaligned unmap accepted");
 Require(!call(73,mapped.value,16385).error && services.AllocatedBytes()==0,"Unmap failed");
 bool missing=false;try{memory.Load(mapped.value,1);}catch(const std::runtime_error&){missing=true;}
 Require(missing && call(73,mapped.value,16385).error==22,"Freed mapping remained accessible");
 auto readonly=call(477,0,1,1);Require(!readonly.error && !memory.Executable(readonly.value),"Read-only data mapping failed");
 bool denied=false;try{memory.Store(readonly.value,1,1);}catch(const std::runtime_error&){denied=true;}
 Require(denied && !call(73,readonly.value,1).error,"Read-only mapping writable");
 Require(call(477,0,0).error==22 && call(477,0,UINT64_MAX).error==22,"Invalid mapping length accepted");
 Require(call(477,0,16*1024*1024+1).error==12,"Mapping budget not enforced");
 Require(call(477,0,1,7).error==22 && call(477,0,1,0).error==22,"Unsupported permissions accepted");
 Require(call(477,1,1).error==22 && call(477,0,1,3,0x1012).error==22,"Unsupported address policy accepted");
 Require(call(477,0,1,3,0x1002,1).error==22 && call(477,0,1,3,0x1002,UINT32_MAX,1).error==22,"File-backed mapping accepted");
 Require(call(73,0x400000,16384).error==22 && memory.Executable(0x400000),"Unmap removed unowned code");
 uint64_t stale=0;
 {
  GuestAllocations owned(memory);
  for(unsigned i=0;i<256;++i){auto r=owned.Map(0,1,3,0x1002,UINT32_MAX,0);Require(!r.error,"Allocation slot prematurely full");stale=r.value;}
  Require(owned.Map(0,1,3,0x1002,UINT32_MAX,0).error==12 && owned.BytesAllocated()==256*16384,"Allocation count not bounded");
 }
 missing=false;try{memory.Load(stale,1);}catch(const std::runtime_error&){missing=true;}
 Require(missing,"Owner destruction left dangling mappings");
 Require(!memory.RemoveData(0x400000,code) && memory.Executable(0x400000),"Data removal accepted code region");
 std::cout<<"PASS: "<<checks<<" real CRT mmap/munmap calls, zeroing, permissions, bounds, stale-access rejection and owner cleanup\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
