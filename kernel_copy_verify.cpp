#include "dispatcher.hpp"
#include "elf_image.hpp"
#include "kernel-copy.hpp"
#include "guest_process_list.hpp"
#include "guest_module_list.hpp"
#include <unistd.h>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
using namespace NativePS5;
void Require(bool b,const char* s){if(!b)throw std::runtime_error(s);}
int main(int argc,char** argv){try{
 Require(argc==2,"ELF input required");std::ifstream stream(argv[1],std::ios::binary);
 std::vector<char> file((std::istreambuf_iterator<char>(stream)),{});ElfImage image(std::as_bytes(std::span(file)));
 GuestMemory memory,kernel;BootstrapServices services(memory);
 image.Relocate([&](std::string_view name){return services.Resolve(1,name);});
 for(auto segment:image.Segments()){auto data=image.Read(segment.address,segment.memorySize);memory.Add({const_cast<std::byte*>(data.data()),data.size()},segment.flags&2,segment.flags&1);}
 constexpr uint64_t control=0xffff800000100000,target=0xffff800000200000;
 std::array<std::byte,64> fields{};std::array<std::byte,512> data{},user{};std::array<std::byte,4096> stack{};
 kernel.AddAt(control,fields,true);kernel.AddAt(target,data,true);memory.Add(user,true);memory.Add(stack,true);
 auto [selector,writer]=services.CreatePacketControlPair(kernel);auto [readPipe,writePipe]=services.CreateKernelPipe(kernel,control);
 memory.Store(image.LoadBias()+0x530000,selector,4);memory.Store(image.LoadBias()+0x530004,writer,4);
 memory.Store(image.LoadBias()+0x530008,readPipe,4);memory.Store(image.LoadBias()+0x53000c,writePipe,4);
 memory.Store(image.LoadBias()+0x58f8b8,services.ErrorAddress(),8);
 memory.Store(image.LoadBias()+0x58f918,control,8);memory.Store(image.LoadBias()+0x58f890,services.SyscallAddress(),8);
 Dispatcher dispatcher;Register_kernel_copy(dispatcher,image);
 const auto userAddress=reinterpret_cast<uintptr_t>(user.data()),stackTop=reinterpret_cast<uintptr_t>(stack.data())+stack.size()-8;
 auto run=[&](bool input,size_t count,uint32_t error=0,uint64_t userPointer=0,uint64_t kernelPointer=0){
  if(!userPointer) userPointer=userAddress;
  if(!kernelPointer) kernelPointer=target;
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=image.LoadBias();cpu.fuel=10000;cpu.rip=input?0x2ad0:0x2ca0;cpu.registers[4]=stackTop;
  cpu.registers[7]=input?userPointer:kernelPointer;cpu.registers[6]=input?kernelPointer:userPointer;cpu.registers[2]=count;
  memory.Store(stackTop,image.LoadBias()+0x373,8);
  auto result=dispatcher.Run(cpu,services,1000);
  Require(result.reason==DispatchStop::MissingCode && result.pc==0x373 && !cpu.fault && uint32_t(cpu.registers[0])==(error?UINT32_MAX:0) && cpu.registers[4]==stackTop+8,"Real kernel-copy helper did not complete with expected result");
  if(error) Require(services.ErrorValue()==error,"Real kernel-copy errno differs from failure");
 };
 for(size_t count:{size_t(1),size_t(7),size_t(20),size_t(255),size_t(512)}){
  for(size_t i=0;i<data.size();++i)data[i]=std::byte(i*17+count);user.fill(std::byte(0));
  auto before=data;run(false,count);
  for(size_t i=0;i<user.size();++i)Require(user[i]==(i<count?before[i]:std::byte(0)),"Real copyout byte mismatch");
  Require(data==before && kernel.Load(control,4)==0x40000000-count && kernel.Load(control+8,4)==count,"Copyout control accounting failed");
  for(size_t i=0;i<user.size();++i)user[i]=std::byte(i*29+count);data.fill(std::byte(0));
  run(true,count);
  for(size_t i=0;i<data.size();++i)Require(data[i]==(i<count?user[i]:std::byte(0)),"Real copyin byte mismatch");
  Require(kernel.Load(control,4)==count && kernel.Load(control+4,4)==count,"Copyin control accounting failed");
 }
 const auto failedData=data,failedUser=user;
 run(false,8,14,userAddress+511);
 Require(data==failedData && user==failedUser && kernel.Load(control,4)==0x40000000 && kernel.Load(control+8,4)==0,"Failed copyout consumed data or changed bytes");
 run(true,8,14,userAddress+511);
 Require(data==failedData && user==failedUser && kernel.Load(control,4)==0 && kernel.Load(control+4,4)==0,"Failed copyin changed data or counters");
 run(false,8,14,0,target+508);
 Require(data==failedData && user==failedUser,"Truncated kernel source changed user data");
 run(true,8,14,0,target+508);
 Require(data==failedData && user==failedUser,"Truncated kernel destination changed data");
 run(false,0,22); // The actual helper calls __error and stores EINVAL.
 run(true,1);
 Require(services.ErrorValue()==22,"Successful syscall unexpectedly cleared errno");
 // Also exercise malformed ring metadata directly.
 GuestDescriptors descriptors;auto [r,w]=descriptors.CreateKernelPipe(kernel,control);
 kernel.Store(control,8,4);kernel.Store(control+4,8,4);kernel.Store(control+8,0,4);kernel.Store(control+12,512,4);kernel.Store(control+16,target,8);
 auto fieldsBefore=fields;auto dataBefore=data;
 Require(descriptors.Read(memory,r,UINT64_MAX,8).error==14 && fields==fieldsBefore && data==dataBefore,"Failed kernel read changed state");
 Require(descriptors.Write(memory,w,UINT64_MAX,8).error==14 && fields==fieldsBefore && data==dataBefore,"Failed kernel write changed state");
 kernel.Store(control+16,target+508,8);fieldsBefore=fields;
 Require(descriptors.Read(memory,r,userAddress,8).error==14 && fields==fieldsBefore,"Partial mapped source advanced counters");
 kernel.Store(control,8,4);kernel.Store(control+4,4,4);kernel.Store(control+8,12,4);kernel.Store(control+12,16,4);kernel.Store(control+16,target,8);
 for(size_t i=0;i<16;++i)data[i]=std::byte(i);
 auto result=descriptors.Read(memory,r,userAddress,8);
 Require(!result.error && result.value==8 && kernel.Load(control,4)==0 && kernel.Load(control+8,4)==4,"Kernel ring read accounting failed");
 for(size_t i=0;i<8;++i)Require(user[i]==std::byte((i+12)%16),"Kernel ring read order failed");
 for(size_t i=0;i<8;++i)user[i]=std::byte(100+i);
 result=descriptors.Write(memory,w,userAddress,8);
 Require(!result.error && result.value==8 && kernel.Load(control,4)==8 && kernel.Load(control+4,4)==12,"Kernel ring write accounting failed");
 for(size_t i=0;i<8;++i)Require(data[4+i]==user[i],"Kernel ring write order failed");
 kernel.Store(control+16,UINT64_MAX-8,8);fieldsBefore=fields;
 Require(descriptors.Read(memory,r,userAddress,8).error==22 && fields==fieldsBefore,"Overflowing kernel ring accepted");
 GuestProcessList processes(kernel,0xffff800000300000,0xffff800000400000);
 const auto currentPid=static_cast<uint32_t>(getpid());
 const auto currentProcess=processes.Add(currentPid);
 const auto otherPid=currentPid==100?101:100;
 const auto otherProcess=processes.Add(otherPid);
 memory.Store(image.LoadBias()+0x58f8d0,processes.RootAddress(),8);
 auto lookup=[&](int32_t pid,uint64_t expected,uint32_t error=0){
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=image.LoadBias();cpu.fuel=100000;cpu.rip=0x30e0;
  cpu.registers[4]=stackTop;cpu.registers[7]=static_cast<uint32_t>(pid);
  memory.Store(stackTop,image.LoadBias()+0x373,8);
  auto result=dispatcher.Run(cpu,services,10000);
  Require(result.reason==DispatchStop::MissingCode && result.pc==0x373 && !cpu.fault && cpu.registers[0]==expected && cpu.registers[4]==stackTop+8,"Real process lookup failed");
  if(error)Require(services.ErrorValue()==error,"Process lookup errno mismatch");
 };
 lookup(0,currentProcess); // Calls getpid, traverses the linked list and fills guest cache.
 lookup(otherPid,otherProcess);
 lookup(INT32_MAX,0,3);
 // Check that the real lookup uses its cache on the repeated PID path.
 memory.Store(image.LoadBias()+0x58f8d0,0,8);
 lookup(currentPid,currentProcess);
 memory.Store(image.LoadBias()+0x58f8d0,processes.RootAddress(),8);
 const auto head=kernel.Load(processes.RootAddress(),8);
 for(uint32_t invalid:{uint32_t(0),currentPid,UINT32_MAX}){
  bool rejected=false;try{processes.Add(invalid);}catch(const std::invalid_argument&){rejected=true;}
  Require(rejected && kernel.Load(processes.RootAddress(),8)==head,"Invalid PID changed process list");
 }
 Require(kernel.Load(otherProcess,8)==currentProcess && kernel.Load(currentProcess,8)==0,"Process link structure corrupted");
 std::array<std::byte,64> collision{};
 kernel.AddAt(0xffff800000400800,collision,true);
 bool mappingRejected=false;
 try{processes.Add(currentPid==102?103:102);}catch(const std::runtime_error&){mappingRejected=true;}
 Require(mappingRejected && kernel.Load(processes.RootAddress(),8)==head,"Failed process mapping changed list head");
 {
  GuestMemory boundedMemory;GuestProcessList bounded(boundedMemory,0x1000,0x2000);
  for(uint32_t pid=1;pid<=256;++pid)bounded.Add(pid);
  const auto finalHead=boundedMemory.Load(0x1000,8);
  bool full=false;try{bounded.Add(257);}catch(const std::invalid_argument&){full=true;}
  Require(full && boundedMemory.Load(0x1000,8)==finalHead,"Process limit not enforced atomically");
 }
 GuestModuleList modules(kernel,memory,0xffff800000500000,0xffff800000600000);
 const std::array<GuestModuleList::Export,2> exports{{{"9BcDykPmo1I",services.ErrorAddress()},{"HoLVWNanBBc",services.GetpidAddress()}}};
 const auto module1=modules.Add(1,0,exports);
 const auto module2=modules.Add(2001,0,exports);
 kernel.Store(currentProcess+0x3e8,modules.RootAddress(),8);
 auto findModule=[&](int32_t pid,uint32_t handle,uint64_t expected,uint32_t error=0,uint64_t output=0){
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=image.LoadBias();cpu.fuel=100000;cpu.rip=0x3320;
  cpu.registers[4]=stackTop;cpu.registers[7]=static_cast<uint32_t>(pid);cpu.registers[6]=handle;
  cpu.registers[2]=output?output:userAddress;memory.Store(stackTop,image.LoadBias()+0x373,8);
  auto result=dispatcher.Run(cpu,services,10000);
  Require(result.reason==DispatchStop::MissingCode && result.pc==0x373 && !cpu.fault &&
          uint32_t(cpu.registers[0])==(error?UINT32_MAX:0) && cpu.registers[4]==stackTop+8,"Real module lookup failed");
  if(error)Require(services.ErrorValue()==error,"Module lookup errno mismatch");
  else {
   std::array<std::byte,0x180> record{};kernel.ReadBytes(expected,record);
   Require(std::equal(record.begin(),record.end(),user.begin()),"Module lookup did not copy complete record");
  }
 };
 findModule(0,1,module1);findModule(currentPid,2001,module2);findModule(0,999,0,22);
 findModule(INT32_MAX,1,0,3);findModule(0,1,0,14,userAddress+500);
 const auto moduleHead=kernel.Load(modules.RootAddress(),8);
 for(uint32_t bad:{uint32_t(0),uint32_t(1),uint32_t(2001)}){
  bool rejected=false;try{modules.Add(bad,0,exports);}catch(const std::invalid_argument&){rejected=true;}
  Require(rejected && kernel.Load(modules.RootAddress(),8)==moduleHead,"Invalid module changed head");
 }
 auto invalidExports=exports;
 invalidExports[0].address=userAddress;
 bool badExport=false;try{modules.Add(3,0,invalidExports);}catch(const std::invalid_argument&){badExport=true;}
 Require(badExport && kernel.Load(modules.RootAddress(),8)==moduleHead,"Nonexecutable export accepted");
 invalidExports=exports;invalidExports[1].nid=invalidExports[0].nid;
 badExport=false;try{modules.Add(3,0,invalidExports);}catch(const std::invalid_argument&){badExport=true;}
 Require(badExport && kernel.Load(modules.RootAddress(),8)==moduleHead,"Duplicate export accepted");
 invalidExports=exports;invalidExports[0].nid="bad";
 badExport=false;try{modules.Add(3,0,invalidExports);}catch(const std::invalid_argument&){badExport=true;}
 Require(badExport && kernel.Load(modules.RootAddress(),8)==moduleHead,"Malformed export NID accepted");
 badExport=false;try{modules.Add(3,exports[0].address,exports);}catch(const std::invalid_argument&){badExport=true;}
 Require(badExport && kernel.Load(modules.RootAddress(),8)==moduleHead,"Zero-valued export accepted");
 std::array<std::byte,16> moduleCollision{};kernel.AddAt(0xffff800000620000,moduleCollision,true);
 bool collisionRejected=false;try{modules.Add(3,0,exports);}catch(const std::runtime_error&){collisionRejected=true;}
 Require(collisionRejected && kernel.Load(modules.RootAddress(),8)==moduleHead,"Module mapping failure changed head");
 const auto dynamic=kernel.Load(module1+0x148,8),symbols=kernel.Load(dynamic+0x28,8),strings=kernel.Load(dynamic+0x38,8);
 Require(kernel.Load(dynamic+0x30,8)==72 && kernel.Load(dynamic+0x40,8)==25,"Module table sizes incorrect");
 for(size_t i=0;i<exports.size();++i){
  const auto symbol=symbols+(i+1)*24;
  Require(kernel.Load(symbol+8,8)+kernel.Load(module1+0x30,8)==exports[i].address,"Symbol address differs from callable export");
  std::array<std::byte,12> name{};kernel.ReadBytes(strings+kernel.Load(symbol,4),name);
  Require(std::memcmp(name.data(),exports[i].nid.data(),11)==0 && name[11]==std::byte(0),"Export NID differs from registered symbol");
 }
 {
  GuestMemory boundedKernel;GuestModuleList bounded(boundedKernel,memory,0x1000,0x10000);
  for(uint32_t handle=1;handle<=128;++handle)bounded.Add(handle,0,exports);
  const auto head=boundedKernel.Load(bounded.RootAddress(),8);
  bool full=false;try{bounded.Add(129,0,exports);}catch(const std::invalid_argument&){full=true;}
  Require(full && boundedKernel.Load(bounded.RootAddress(),8)==head,"Module capacity failure changed head");
 }
 auto resolve=[&](uint32_t handle,std::string_view nid,uint64_t expected){
  user.fill(std::byte(0));std::memcpy(user.data(),nid.data(),nid.size());
  GuestCpu cpu;cpu.memory=&memory;cpu.loadBias=image.LoadBias();cpu.fuel=200000;cpu.rip=0x3810;
  cpu.registers[4]=stackTop;cpu.registers[7]=0;cpu.registers[6]=handle;cpu.registers[2]=userAddress;
  memory.Store(stackTop,image.LoadBias()+0x373,8);
  auto result=dispatcher.Run(cpu,services,20000);
  if(result.pc!=0x373 || cpu.fault || cpu.registers[0]!=expected)
   std::cerr<<"Resolver diagnostic pc="<<std::hex<<result.pc<<" fault="<<cpu.fault<<" rax="<<cpu.registers[0]<<" expected="<<expected<<" errno="<<services.ErrorValue()<<std::dec<<'\n';
  Require(result.reason==DispatchStop::MissingCode && result.pc==0x373 && !cpu.fault &&
          cpu.registers[0]==expected && cpu.registers[4]==stackTop+8,"Complete real symbol resolver failed");
  Require(services.AllocatedBytes()==0,"Symbol resolver leaked temporary mapping");
 };
 for(const auto& entry:exports){resolve(1,entry.nid,entry.address);resolve(2001,entry.nid,entry.address);}
 resolve(1,"AAAAAAAAAAA",0);resolve(999,exports[0].nid,0);
 // A truncated symbol table must fail copying and still release its allocation.
 const auto originalSymbols=kernel.Load(dynamic+0x28,8);
 kernel.Store(dynamic+0x28,0xffff800020000000,8);resolve(1,exports[0].nid,0);
 Require(services.ErrorValue()==14,"Resolver copy failure did not preserve EFAULT");
 kernel.Store(dynamic+0x28,originalSymbols,8);
 std::cout<<"PASS: complete real kernel_dynlib_resolve finds native exports and frees temporary mappings on success/failure\n";
 std::cout<<"PASS: real kernel_dynlib_obj traverses owned module records; callable export tables and missing/invalid lookup paths\n";
 std::cout<<"PASS: real kernel_get_proc traverses modeled records, reports ESRCH and reuses its guest cache\n";
 std::cout<<"PASS: complete real kernel_copyin/kernel_copyout helpers at five lengths; guest errno failures, counters and failed-transfer preservation\n";
}catch(const std::exception& e){std::cerr<<"FAIL: "<<e.what()<<'\n';return 1;}}
