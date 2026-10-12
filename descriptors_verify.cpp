#include "bootstrap_services.hpp"
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
extern "C" void metadata_syscall(GuestCpu*);
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 GuestMemory memory;std::array<std::byte,0x4000> code{};std::array<std::byte,256> stack{};
 std::array<std::byte,2048> data{};std::array<std::byte,8> slot{};
 memory.AddAt(0x400000,code,false,true);memory.AddAt(0x700000,stack,true);
 memory.AddAt(0x900000,data,true);memory.AddAt(0x400000+0x58f890,slot,true);
 BootstrapServices services(memory);memory.Store(0x400000+0x58f890,services.SyscallAddress(),8);
 auto [reader,writer]=services.CreatePipe(512);
 for(size_t i=0;i<1024;++i)data[i]=std::byte(i*17);
 GuestCpu cpu;unsigned checks=0;
 // Execute the real homebrew CRT wrapper for each operation.
 auto call=[&](uint64_t number,uint64_t fd,uint64_t address,uint64_t count){
  cpu={};cpu.memory=&memory;cpu.loadBias=0x400000;cpu.fuel=50;cpu.rip=0x380;cpu.flags=0x246;
  cpu.registers[4]=0x700080;cpu.registers[7]=number;cpu.registers[6]=fd;cpu.registers[2]=address;cpu.registers[1]=count;
  metadata_syscall(&cpu);Require(cpu.stop==CpuStop::Call && services.Dispatch(cpu),"Descriptor bridge failed");
  metadata_syscall(&cpu);Require(!cpu.fault && cpu.stop==CpuStop::Return && cpu.registers[4]==0x700080,"Descriptor CRT return failed");++checks;
  Require(!(cpu.flags&1) || cpu.registers[0]==UINT64_MAX,"Syscall error not converted to -1");
  return IoResult{cpu.registers[0],uint32_t((cpu.flags&1)?services.ErrorValue():0)};
 };
 Require(call(3,reader,0x900400,1).error==35,"Empty nonblocking pipe should block");
 Require(call(4,writer,0x900000,600).value==512 && !(cpu.flags&1),"Large write did not return short count");
 Require(call(4,writer,0x900000,1).error==35,"Full pipe accepted bytes");
 Require(call(3,reader,0x9007ff,2).error==14,"Cross-boundary read accepted");
 Require(call(3,reader,0x400000,2).error==14,"Read overwrote executable memory");
 Require(call(3,reader,0x900400,300).value==300 && !(cpu.flags&1),"Partial read failed");
 for(size_t i=0;i<300;++i)Require(data[1024+i]==std::byte(i*17),"FIFO read corruption");
 Require(call(4,writer,0x900000,301).error==35,"Small write was not atomic");
 Require(call(4,writer,0x9007ff,300).error==14,"Invalid source consumed pipe space");
 Require(call(4,writer,0x900200,300).value==300 && !(cpu.flags&1),"Wraparound write failed");
 Require(call(6,writer,0,0).value==0 && !(cpu.flags&1),"Close failed");
 Require(call(3,reader,0x900400,1024).value==512 && !(cpu.flags&1),"Drain after close failed");
 for(size_t i=0;i<512;++i)Require(data[1024+i]==std::byte((i+300)*17),"Ring order failed");
 Require(call(3,reader,0,1).value==0 && !(cpu.flags&1),"Closed writer did not produce EOF");
 Require(call(6,writer,0,0).error==9,"Double close accepted");
 Require(call(4,reader,0,0).error==9,"Wrong endpoint accepted");
 auto [r2,w2]=services.CreatePipe(512);
 Require(call(6,r2,0,0).value==0 && !(cpu.flags&1),"Reader close failed");
 Require(call(4,w2,0x900000,1).error==32,"Missing reader did not produce EPIPE");
 Require(call(4,w2,0,0).value==0 && !(cpu.flags&1),"Zero-length operation accessed pointer");
 Require(call(4,w2,0,1048577).error==22,"Oversized transfer accepted");
 Require(call(3,1,0,0).error==9,"Host descriptor leaked into guest table");
 GuestDescriptors bounded;for(unsigned i=0;i<128;++i)(void)bounded.CreatePipe(512);
 bool failed=false;try{bounded.CreatePipe(512);}catch(const std::runtime_error&){failed=true;}
 Require(failed,"Descriptor count unbounded");
 std::cout<<"PASS: "<<checks<<" translated CRT pipe operations, FIFO wraparound, errors, close/EOF and descriptor limits\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
