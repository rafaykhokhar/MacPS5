#include "bootstrap_services.hpp"
#include <cstring>
#include <iostream>
#include <stdexcept>
using namespace NativePS5;
extern "C" void metadata_syscall(GuestCpu*);
extern "C" void kernel_packet_setup(GuestCpu*);
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 constexpr uint64_t base=0x400000,stackBase=0x700000,kernelBase=0xffff800000100000;
 GuestMemory memory,kernel;
 std::array<std::byte,0x4000> code{};std::array<std::byte,256> stack{};
 std::array<std::byte,8> slot{},pair{};std::array<std::byte,64> state{},immutable{};
 memory.AddAt(base,code,false,true);memory.AddAt(stackBase,stack,true);
 memory.AddAt(base+0x58f890,slot,true);memory.AddAt(base+0x530000,pair,true);
 kernel.AddAt(kernelBase,state,true);kernel.AddAt(kernelBase+0x1000,immutable,false);
 BootstrapServices services(memory);auto [selector,writer]=services.CreatePacketControlPair(kernel);
 memory.Store(base+0x58f890,services.SyscallAddress(),8);memory.Store(base+0x530000,selector,4);
 GuestCpu cpu;unsigned checks=0;
 auto option=[&](uint64_t fd,uint64_t address,uint64_t length=20,uint64_t level=41,uint64_t name=46){
  cpu={};cpu.memory=&memory;cpu.loadBias=base;cpu.fuel=50;cpu.rip=0x380;cpu.registers[4]=stackBase+128;
  cpu.registers[7]=105;cpu.registers[6]=fd;cpu.registers[2]=level;cpu.registers[1]=name;cpu.registers[8]=address;cpu.registers[9]=length;
  metadata_syscall(&cpu);Require(services.Dispatch(cpu),"Packet option dispatch failed");metadata_syscall(&cpu);
  Require(!cpu.fault && cpu.stop==CpuStop::Return,"Packet option CRT return failed");++checks;
  Require(!(cpu.flags&1) || cpu.registers[0]==UINT64_MAX,"Packet option error not converted to -1");
  return uint32_t((cpu.flags&1)?services.ErrorValue():0);
 };
 Require(option(writer,stackBase)==14,"Unconfigured packet writer succeeded");
 // Run the real kernel-copy setup and its first socket option syscall.
 cpu={};cpu.memory=&memory;cpu.loadBias=base;cpu.fuel=100;cpu.rip=0x2b5c;
 cpu.registers[5]=stackBase+224;cpu.registers[4]=stackBase+128;cpu.registers[0]=kernelBase;
 kernel_packet_setup(&cpu);Require(EnterGuestCall(cpu,base+cpu.nextRip),"Kernel setup call entry failed");
 metadata_syscall(&cpu);Require(services.Dispatch(cpu),"Real selector syscall failed");
 metadata_syscall(&cpu);Require(CompleteGuestReturn(cpu) && cpu.rip==0x2b99 && !cpu.registers[0],"Real selector syscall return failed");
 for(size_t i=0;i<20;++i)stack[i]=std::byte(i*11+3);
 Require(option(writer,stackBase)==0,"Packet writer failed");
 for(size_t i=0;i<state.size();++i)Require(state[i]==(i<20?stack[i]:std::byte(0)),"Packet writer changed wrong bytes");
 auto snapshot=state;
 Require(option(selector,stackBase+255)==14,"Bad selector pointer accepted");
 stack[0]^=std::byte(0xff);Require(option(writer,stackBase)==0 && state[0]==stack[0],"Rejected selector changed previous target");snapshot=state;
 Require(option(writer,stackBase+255)==14 && state==snapshot,"Bad source modified kernel state");
 for(uint64_t length:{0,19,21})Require(option(writer,stackBase,length)==22,"Wrong packet length accepted");
 Require(option(writer,stackBase,20,0)==42 && option(writer,stackBase,20,41,0)==42,"Unknown packet option accepted");
 auto [reader,pipeWriter]=services.CreatePipe(512);(void)pipeWriter;
 Require(option(reader,stackBase)==38 && option(999,stackBase)==9,"Descriptor type not checked");
 memory.Store(stackBase+32,kernelBase+48,8);Require(option(selector,stackBase+32)==0,"Target change failed");
 Require(option(writer,stackBase)==14 && state==snapshot,"Cross-boundary kernel write modified state");
 memory.Store(stackBase+32,kernelBase+0x1000,8);Require(option(selector,stackBase+32)==0 && option(writer,stackBase)==14,"Readonly kernel state modified");
 memory.Store(stackBase+32,stackBase,8);Require(option(selector,stackBase+32)==0 && option(writer,stackBase)==14,"Control escaped separate kernel address space");
 bool rejected=false;try{services.CreatePacketControlPair(memory);}catch(const std::invalid_argument&){rejected=true;}
 Require(rejected,"Shared user/kernel address space accepted for control pair");
 std::cout<<"PASS: "<<checks<<" translated packet-control queries, real kernel setup, separate address spaces and atomic failure checks\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
