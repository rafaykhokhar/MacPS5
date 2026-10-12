#include "bootstrap_services.hpp"
#include <iostream>
#include <cstring>
#include <stdexcept>
using namespace NativePS5;
extern "C" void firmware_query(GuestCpu*);
extern "C" void metadata_syscall(GuestCpu*);
extern "C" void kernel_packet_setup(GuestCpu*);
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(){try{
 constexpr uint64_t codeBase=0x400000,stackBase=0x700000,paramBase=0x900000;
 std::array<std::byte,0x4000> code{};std::array<std::byte,256> stack{};std::array<std::byte,32> parameters{};
 uint64_t size=24;std::memcpy(parameters.data(),&size,8);
 uint32_t version=0x07000000;std::memcpy(parameters.data()+20,&version,4);
 GuestMemory memory;memory.AddAt(codeBase,code,false,true);memory.AddAt(stackBase,stack,true);memory.AddAt(paramBase,parameters,false);
 BootstrapServices services(memory);services.RegisterProcessParameters(2,paramBase);
 unsigned rejected=0;
 for (auto [module,address]: {std::pair<uint32_t,uint64_t>{2,paramBase},{3,0},{3,paramBase+31},{3,UINT64_MAX}}){
  try{services.RegisterProcessParameters(module,address);}catch(const std::exception&){++rejected;}
 }
 Require(rejected==4,"Invalid metadata registration accepted");
 for(uint64_t invalidSize:{uint64_t(0),uint64_t(23),uint64_t(33),uint64_t(65537),UINT64_MAX}) {
  std::memcpy(parameters.data(),&invalidSize,8);
  bool failed=false;try{services.RegisterProcessParameters(3,paramBase);}catch(const std::exception&){failed=true;}
  Require(failed,"Invalid or unmapped declared metadata extent accepted");
 }
 std::memcpy(parameters.data(),&size,8);
 services.RegisterProcessParameters(3,paramBase); // Rejections did not consume the handle.
 GuestCpu cpu;
 auto invoke=[&](uint64_t module,uint64_t member,uint64_t output){
  cpu={};cpu.memory=&memory;cpu.loadBias=codeBase;cpu.flags=0x247;cpu.nextRip=16;
  cpu.stop=CpuStop::Call;cpu.target=services.SyscallAddress();cpu.registers[4]=stackBase+200;
  cpu.registers[0]=649;cpu.registers[7]=module;cpu.registers[6]=member;cpu.registers[2]=output;cpu.registers[10]=0x123;
  Require(services.Dispatch(cpu) && !cpu.fault && cpu.rip==16 && cpu.registers[4]==stackBase+200,"Metadata syscall did not return");
  Require(cpu.registers[10]==0x123 && cpu.registers[11]==0x247 && cpu.registers[1]==services.SyscallAddress()+2,"Metadata syscall clobbers incorrect");
 };
 invoke(2,8,stackBase);Require(!cpu.registers[0] && !(cpu.flags&1) && memory.Load(stackBase,8)==paramBase,"Metadata pointer query failed");
 memory.Store(stackBase,0x1234,8);
 invoke(99,8,stackBase);Require(cpu.registers[0]==UINT64_MAX && services.ErrorValue()==3 && (cpu.flags&1) && memory.Load(stackBase,8)==0x1234,"Missing module falsely succeeded");
 invoke(2,7,stackBase);Require(cpu.registers[0]==UINT64_MAX && services.ErrorValue()==22 && (cpu.flags&1) && memory.Load(stackBase,8)==0x1234,"Unknown member falsely succeeded");
 for(auto address:{uint64_t(0),paramBase,stackBase+255}) {
  const auto before=parameters;
  invoke(2,8,address);Require(cpu.registers[0]==UINT64_MAX && services.ErrorValue()==14 && (cpu.flags&1) && parameters==before,"Bad output pointer changed metadata");
 }
 cpu={};cpu.memory=&memory;cpu.loadBias=codeBase;cpu.nextRip=16;cpu.target=services.SyscallAddress();
 cpu.stop=CpuStop::Call;cpu.registers[0]=649;cpu.registers[7]=2;cpu.registers[6]=8;
 cpu.registers[4]=stackBase+200;cpu.registers[2]=stackBase+192;
 Require(!services.Dispatch(cpu) && cpu.fault==6 && memory.Load(stackBase+192,8)==paramBase,"Metadata output alias did not validate actual guest return");
 GuestMemory foreign;
 cpu={};cpu.memory=&foreign;cpu.target=services.SyscallAddress();cpu.stop=CpuStop::Call;cpu.registers[0]=649;
 Require(!services.Dispatch(cpu) && cpu.fault==8,"Foreign guest address space accepted");
 // Execute the real binary's firmware-query instructions, including its CRT
 // syscall wrapper. This isolates a service dependency, not full startup.
 cpu={};cpu.memory=&memory;cpu.loadBias=codeBase;cpu.fuel=100;cpu.rip=0x212a;
 cpu.registers[4]=stackBase+160;cpu.registers[5]=stackBase+224;
 firmware_query(&cpu);
 Require(cpu.stop==CpuStop::Call && cpu.target==codeBase+0x380,"Real metadata query did not call CRT");
 Require(EnterGuestCall(cpu,codeBase+cpu.nextRip),"Real query call failed");
 // The CRT's indirect call reads its syscall thunk pointer through a RIP-relative slot.
 std::array<std::byte,8> slot{};memory.AddAt(codeBase+0x58f890,slot,true);memory.Store(codeBase+0x58f890,services.SyscallAddress(),8);
 metadata_syscall(&cpu);Require(services.Dispatch(cpu),"Real query syscall failed");
 metadata_syscall(&cpu);Require(cpu.stop==CpuStop::Return && CompleteGuestReturn(cpu),"Real query CRT return failed");
 firmware_query(&cpu);
 Require(!cpu.fault && cpu.rip==0x2167 && cpu.registers[14]==version && memory.Load(stackBase+192,8)==paramBase,"Real firmware read differs from registered metadata");
 std::array<std::byte,8> pair{};memory.AddAt(codeBase+0x530000,pair,true);memory.Store(codeBase+0x530000,42,4);
 cpu={};cpu.memory=&memory;cpu.loadBias=codeBase;cpu.fuel=100;cpu.rip=0x2b5c;cpu.registers[5]=stackBase+224;
 cpu.registers[0]=0xffff800000100000;cpu.registers[7]=stackBase;cpu.registers[6]=0xffff800000200000;cpu.registers[2]=8;
 cpu.vectors[0].fill(std::byte(0xff));kernel_packet_setup(&cpu);
 Require(!cpu.fault && cpu.stop==CpuStop::Call && cpu.target==codeBase+0x380,"Kernel control setup did not reach syscall wrapper");
 Require(cpu.registers[7]==105 && cpu.registers[6]==42 && cpu.registers[2]==41 && cpu.registers[1]==46 && cpu.registers[9]==20,"Kernel packet option arguments incorrect");
 Require(memory.Load(cpu.registers[8],8)==0xffff800000100000,"Kernel packet target lost");
 for(size_t i=8;i<24;++i)Require(memory.Load(cpu.registers[8]+i,1)==0,"Kernel packet control padding not zeroed");
 for(auto byte:cpu.vectors[0])Require(byte==std::byte(0),"Real VXORPS did not clear vector register");
 std::cout<<"PASS: real kernel-copy control preparation reaches setsockopt call with verified descriptor, address and zero padding\n";
 std::cout<<"PASS: registered module metadata, error returns, library syscall ABI and real homebrew firmware-query instructions\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
