#include "dispatcher.hpp"
#include "elf_image.hpp"
#include "discovered.hpp"
#include "payload_environment.hpp"
#include "guest_format.hpp"
#include <fstream>
#include <iostream>
#include <iterator>
using namespace NativePS5;
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(int argc,char** argv){try{
 Require(argc==2,"ELF input required");std::ifstream stream(argv[1],std::ios::binary);
 std::vector<char> file((std::istreambuf_iterator<char>(stream)),{});ElfImage image(std::as_bytes(std::span(file)));
 GuestMemory memory;BootstrapServices services(memory);
 image.Relocate([&](std::string_view name){return services.Resolve(1,name);});
 for(auto segment:image.Segments()){auto data=image.Read(segment.address,segment.memorySize);memory.Add({const_cast<std::byte*>(data.data()),data.size()},segment.flags&2,segment.flags&1);}
 PayloadEnvironment environment(memory,services);
 std::vector<std::byte> stack(65536);memory.Add(stack,true);
 GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=image.LoadBias();cpu.rip=0x40;cpu.fuel=20000000;
 cpu.registers[4]=reinterpret_cast<uintptr_t>(stack.data())+stack.size()-8;cpu.registers[7]=environment.Arguments();
 Dispatcher dispatcher;Register_discovered(dispatcher,image);
 bool initialized=false;
 for(size_t step=0;step<2000000;++step){
  auto result=dispatcher.Run(cpu,services,1);
  if(cpu.stop==CpuStop::Return && cpu.rip==0x29ee){
   Require(cpu.registers[0]==0,"Full kernel initialization returned failure");
   Require(memory.Load(image.LoadBias()+0x58f8d0,8)==PayloadEnvironment::Allproc,"Firmware path calculated wrong process root");
   Require(memory.Load(image.LoadBias()+0x58f8e8,8)==PayloadEnvironment::Prison,"Kernel initialization did not read modeled prison");
   Require(memory.Load(image.LoadBias()+0x58f8b8,8)==services.ErrorAddress(),"Kernel initialization did not resolve __error");
   Require(services.AllocatedBytes()==0,"Kernel initialization leaked resolver scratch space");
   initialized=true;break;
  }
  if(result.reason!=DispatchStop::TransitionLimit){
   std::cerr<<"Initialization boundary pc="<<std::hex<<result.pc<<" fault="<<cpu.fault<<" rax="<<cpu.registers[0]<<std::dec<<'\n';
   throw std::runtime_error("Whole-entry execution stopped before kernel initialization completed");
  }
 }
 Require(initialized,"Kernel initialization exceeded transition budget");
 std::cout<<"PASS: real ELF entry -> syscall bootstrap -> full __kernel_init -> guest NID/symbol resolver, with owned payload environment\n";
 bool checkedLogging=false;
 for(size_t step=0;step<2000000;++step){
  auto boundary=dispatcher.Run(cpu,services,1);
  if(cpu.stop==CpuStop::Return && cpu.rip==0x878){
   Require(cpu.registers[0]==0 && memory.Load(image.LoadBias()+0x58f8a0,8)==services.SnprintfAddress() &&
           memory.Load(image.LoadBias()+0x58f8a8,8)==services.StrerrorAddress() &&
           memory.Load(image.LoadBias()+0x58f898,8)==services.VsnprintfAddress() &&
           memory.Load(image.LoadBias()+0x58f8b0,8)==services.ErrorAddress(),
           "Unexpected logging initialization outcome");
   checkedLogging=true;break;
  }
  Require(boundary.reason==DispatchStop::TransitionLimit,"Stopped before logging initialization returned");
 }
 Require(checkedLogging,"Logging initialization exceeded transition budget");
 auto result=dispatcher.Run(cpu,services,2000000);
 Require(memory.Load(services.IsThreadedAddress(),4)==1 && !memory.Executable(services.IsThreadedAddress()),
         "Startup did not resolve and write the guest __isthreaded data export");
 Require(services.AllocatedBytes()==0,"Logging initialization leaked resolver memory");
 std::cout<<"PASS: whole-entry __klog_init resolves all formatting/error exports and returns success\n";
 std::cout<<"PASS: guest startup resolves writable __isthreaded and stores 1\n";
 std::cout<<"NEXT: pc="<<std::hex<<result.pc<<" fault="<<cpu.fault<<" rax="<<cpu.registers[0]<<std::dec<<" reason="<<unsigned(result.reason)<<"; gameplay not reached\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
