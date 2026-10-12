#include "bootstrap_services.hpp"
#include "guest_format.hpp"
#include <cstdio>
#include <cstring>
#include <iostream>
using namespace NativePS5;
void Require(bool value,const char* message){if(!value)throw std::runtime_error(message);}
int main(){try{
 GuestMemory memory;BootstrapServices services(memory);
 std::array<std::byte,16> code{};std::array<std::byte,256> stack{};
 std::array<std::byte,1024> format{},output{},strings{};
 memory.AddAt(0x10000,code,false,true);memory.AddAt(0x20000,stack,true);
 memory.AddAt(0x30000,format,true);memory.AddAt(0x40000,output,true);memory.AddAt(0x50000,strings,true);
 std::memcpy(strings.data(),"hello",6);unsigned checks=0;
 auto call=[&](std::string_view text,std::initializer_list<uint64_t> values,size_t capacity=1024,uint64_t dest=0x40000){
  Require(text.size()<format.size(),"Test format too long");format.fill(std::byte(0));std::memcpy(format.data(),text.data(),text.size());
  output.fill(std::byte(0x55));GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=0x10000;cpu.nextRip=1;cpu.flags=0x246;
  cpu.target=services.SnprintfAddress();cpu.stop=CpuStop::Call;cpu.registers[4]=0x20080;
  cpu.registers[7]=dest;cpu.registers[6]=capacity;cpu.registers[2]=0x30000;
  size_t index=0;for(auto value:values){constexpr unsigned registers[]={1,8,9};if(index<3)cpu.registers[registers[index]]=value;else memory.Store(0x20080+(index-3)*8,value,8);++index;}
  const bool returned=services.Dispatch(cpu);
  if(!returned || cpu.fault || cpu.rip!=1 || cpu.registers[4]!=0x20080)
   std::cerr<<"Format diagnostic pattern="<<text<<" fault="<<cpu.fault<<" pc="<<cpu.rip<<" stack="<<cpu.registers[4]<<'\n';
  Require(returned && !cpu.fault && cpu.rip==1 && cpu.registers[4]==0x20080,"Formatting bridge call/return failed");
  Require(cpu.flags==0x246,"Formatting bridge corrupted preserved flags");++checks;return uint32_t(cpu.registers[0]);
 };
 auto expect=[&](std::string_view format,std::initializer_list<uint64_t> values,std::string_view text){
  Require(call(format,values)==text.size(),"Formatting result length mismatch");
  Require(std::memcmp(output.data(),text.data(),text.size())==0 && output[text.size()]==std::byte(0),"Formatting bytes mismatch");
  Require(output[text.size()+1]==std::byte(0x55),"Formatter overwrote trailing bytes");
 };
 expect("[%s] %d %#08x %%",{0x50000,uint64_t(-42),42},"[hello] -42 0x00002a %");
 expect("%d %d %d %d %d",{1,2,3,4,5},"1 2 3 4 5");
 expect("[%*.*s]",{uint64_t(-8),3,0x50000},"[hel     ]");
 expect("%hhd %hhu %hd %hu",{255,511,65535,131071},"-1 255 -1 65535");
 expect("%#.0o|%.0u|%+06d|% 4d",{0,0,12,12},"0||+00012|  12");
 expect("%p %llx %c",{0x123,UINT64_MAX,'Z'},"0x123 ffffffffffffffff Z");
 const char* patterns[]={"%lld","%+024lld","%#020llx","%.20llu","%-24llo"};
 for(auto pattern:patterns)for(uint64_t value:{uint64_t(0),uint64_t(1),UINT64_MAX,uint64_t(1)<<63,uint64_t(0xffffffff)}){
  char reference[128];int length;
  if(std::strchr(pattern,'d'))length=std::snprintf(reference,sizeof reference,pattern,static_cast<long long>(value));
  else length=std::snprintf(reference,sizeof reference,pattern,static_cast<unsigned long long>(value));
  Require(length>=0,"Host formatting reference failed");expect(pattern,{value},std::string_view(reference,length));
 }
 Require(call("abcdef",{},4)==6 && std::memcmp(output.data(),"abc\0",4)==0 && output[4]==std::byte(0x55),"Truncation failed");
 Require(call("abcdef",{},1)==6 && output[0]==std::byte(0) && output[1]==std::byte(0x55),"Capacity-one handling failed");
 Require(call("abcdef",{},0,UINT64_MAX)==6 && output[0]==std::byte(0x55),"Zero capacity touched output");
 Require(call("%s",{UINT64_MAX})==UINT32_MAX && services.ErrorValue()==14 && output[0]==std::byte(0x55),"Bad source did not fail atomically");
 Require(call("%.0s",{UINT64_MAX})==0,"Zero precision read source");
 for(auto unsupported:{"%f","%n","%ls","%2$d","%999999d","%"})
  Require(call(unsupported,{0x40000})==UINT32_MAX && services.ErrorValue()==22 && output[0]==std::byte(0x55),"Unsupported format silently accepted");
 Require(call("abcd",{},1024,0x403ff)==UINT32_MAX && services.ErrorValue()==14 && output[1023]==std::byte(0x55),"Truncated destination changed bytes");
 Require(services.Resolve(2,"snprintf")==services.SnprintfAddress() && !services.Resolve(1,"snprintf"),"Formatting export lost module ownership");
 auto invoke=[&](uint64_t target,uint64_t first,uint64_t fourth=0){
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=0x10000;cpu.nextRip=1;cpu.flags=0x246;
  cpu.target=target;cpu.stop=CpuStop::Call;cpu.registers[4]=0x20080;
  cpu.registers[7]=first;cpu.registers[6]=1024;cpu.registers[2]=0x30000;cpu.registers[1]=fourth;
  Require(services.Dispatch(cpu) && !cpu.fault && cpu.rip==1 && cpu.registers[4]==0x20080,"C runtime bridge return failed");
  Require(cpu.flags==0x246,"C runtime bridge changed flags");++checks;return cpu.registers[0];
 };
 constexpr uint64_t va=0x50100,save=0x50200,overflow=0x50300;
 const char* many="%llu %llu %llu %llu %llu %llu %llu %llu";
 format.fill(std::byte(0));std::memcpy(format.data(),many,std::strlen(many));
 for(unsigned offset=0;offset<=48;offset+=8){
  memory.Store(va,offset,4);memory.Store(va+4,48,4);memory.Store(va+8,overflow,8);memory.Store(va+16,save,8);
  unsigned value=1;for(unsigned slot=offset;slot<48;slot+=8)memory.Store(save+slot,value++,8);
  for(unsigned slot=0;value<=8;slot+=8)memory.Store(overflow+slot,value++,8);
  const auto before=strings;output.fill(std::byte(0x55));
  Require(invoke(services.VsnprintfAddress(),0x40000,va)==15 && std::memcmp(output.data(),"1 2 3 4 5 6 7 8\0",16)==0,"SysV va_list register/overflow transition failed");
  Require(strings==before,"vsnprintf modified caller va_list or argument storage");
 }
 for(uint64_t invalid:{uint64_t(1),uint64_t(49),uint64_t(UINT32_MAX)}){
  memory.Store(va,invalid,4);output.fill(std::byte(0x55));
  Require(uint32_t(invoke(services.VsnprintfAddress(),0x40000,va))==UINT32_MAX && services.ErrorValue()==22 && output[0]==std::byte(0x55),"Malformed va_list offset accepted");
 }
 memory.Store(va,0,4);memory.Store(va+16,UINT64_MAX-3,8);output.fill(std::byte(0x55));
 Require(uint32_t(invoke(services.VsnprintfAddress(),0x40000,va))==UINT32_MAX && services.ErrorValue()==14 && output[0]==std::byte(0x55),"Invalid register-save pointer accepted");
 Require(uint32_t(invoke(services.VsnprintfAddress(),0x40000,0x503ff))==UINT32_MAX && services.ErrorValue()==14,"Truncated va_list accepted");
 memory.Store(services.ErrnoAddress(),123,4);
 const auto errorPointer=invoke(services.StrerrorAddress(),14);
 Require(FormatString(memory,errorPointer,128)=="Bad address" && services.ErrorValue()==123,"Known strerror text or errno preservation failed");
 bool readonly=false;try{memory.Store(errorPointer,0,1);}catch(const std::runtime_error&){readonly=true;}
 Require(readonly,"Guest overwrote error-message storage");
 const auto unknown=invoke(services.StrerrorAddress(),uint32_t(INT32_MIN));
 Require(unknown==errorPointer && FormatString(memory,unknown,128)=="Unknown error: -2147483648" && services.ErrorValue()==22,"Unknown strerror conversion failed");
 Require(services.Resolve(2,"strerror")==services.StrerrorAddress() && services.Resolve(2,"vsnprintf")==services.VsnprintfAddress() && !services.Resolve(1,"strerror"),"New C runtime export ownership failed");
 std::cout<<"PASS: "<<checks<<" guest formatting/error calls, SysV registers/stack/va_list, host reference outputs, truncation and atomic failures\n";
}catch(const std::exception& error){std::cerr<<"FAIL: "<<error.what()<<'\n';return 1;}}
