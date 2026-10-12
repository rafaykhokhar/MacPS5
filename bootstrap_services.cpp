#include "bootstrap_services.hpp"
#include "guest_format.hpp"
#include "guest_error_text.hpp"
#include <cstring>
#include <string>
#include <stdexcept>
#include <unistd.h>
#ifdef NATIVE_PS5_RUST_PAYLOAD
#include "rust/string_abi.h"
namespace {
uint64_t RustMap(void* context,unsigned char* data,uint64_t size) noexcept {
    try {auto address=reinterpret_cast<uintptr_t>(data);static_cast<NativePS5::GuestMemory*>(context)->AddAt(address,{reinterpret_cast<std::byte*>(data),size},true);return address;}catch(...){return 0;}
}
uint64_t RustUnmap(void* context,uint64_t address,unsigned char* data,uint64_t size) noexcept {
    return static_cast<NativePS5::GuestMemory*>(context)->RemoveData(address,{reinterpret_cast<std::byte*>(data),size})?0:1;
}
uint64_t RustRead(void* context,uint64_t address,unsigned char* out,uint64_t size) noexcept {
    try {static_cast<NativePS5::GuestMemory*>(context)->ReadBytes(address,{reinterpret_cast<std::byte*>(out),size});return 0;}
    catch(...){return 1;}
}
uint64_t RustWrite(void* context,uint64_t address,const unsigned char* data,uint64_t size) noexcept {
    try {static_cast<NativePS5::GuestMemory*>(context)->WriteBytes(address,{reinterpret_cast<const std::byte*>(data),size});return 0;}
    catch(...){return 2;}
}
}
#endif

namespace NativePS5 {
BootstrapServices::BootstrapServices(GuestMemory& memory):memory(memory),allocations(memory) {
    thunks[0] = std::byte(0x0f); thunks[1] = std::byte(0x0b);
    const unsigned char getpid[] = {0x48,0xc7,0xc0,0x14,0,0,0,0x49,0x89,0xca,0x0f,0x05,0xc3};
    std::memcpy(thunks.data()+16, getpid, sizeof(getpid));
    thunks[32]=std::byte(0x0f); thunks[33]=std::byte(0x0b);
    thunks[48]=std::byte(0x0f); thunks[49]=std::byte(0x0b);
    thunks[64]=std::byte(0x0f); thunks[65]=std::byte(0x0b);
    thunks[80]=std::byte(0x0f); thunks[81]=std::byte(0x0b);
#ifdef NATIVE_PS5_RUST_PAYLOAD
    for(size_t i=96;i<thunks.size();i+=16){thunks[i]=std::byte(0x0f);thunks[i+1]=std::byte(0x0b);}
#endif
    memory.Add(thunks, false, true);
    memory.Add(errnoStorage,true);
    memory.Add(errorText,false);
    memory.Add(isThreaded,true);
#ifdef NATIVE_PS5_RUST_PAYLOAD
    memory.Add(rustEnvironment,false);
    rustHeap=native_ps5_heap_new_v1({&memory,RustMap,RustUnmap});
    if(!rustHeap)throw std::bad_alloc();
#endif
}
BootstrapServices::~BootstrapServices() {
#ifdef NATIVE_PS5_RUST_PAYLOAD
    native_ps5_heap_delete_v1(rustHeap);
#endif
}
uint64_t BootstrapServices::DlsymAddress() const { return reinterpret_cast<uintptr_t>(thunks.data()); }
uint64_t BootstrapServices::GetpidAddress() const { return DlsymAddress()+16; }
uint64_t BootstrapServices::SyscallAddress() const { return GetpidAddress()+10; }
uint64_t BootstrapServices::ErrorAddress() const { return DlsymAddress()+32; }
uint64_t BootstrapServices::SnprintfAddress() const { return DlsymAddress()+48; }
uint64_t BootstrapServices::VsnprintfAddress() const { return DlsymAddress()+64; }
uint64_t BootstrapServices::StrerrorAddress() const { return DlsymAddress()+80; }
uint64_t BootstrapServices::IsThreadedAddress() const { return reinterpret_cast<uintptr_t>(isThreaded.data()); }
uint64_t BootstrapServices::ErrnoAddress() const { return reinterpret_cast<uintptr_t>(errnoStorage.data()); }
uint32_t BootstrapServices::ErrorValue() const { return static_cast<uint32_t>(memory.Load(ErrnoAddress(),4)); }
std::optional<uint64_t> BootstrapServices::Resolve(uint64_t module, std::string_view name) const {
#ifdef NATIVE_PS5_RUST_PAYLOAD
    if(uint32_t(module)==2)for(uint64_t i=0;i<10;++i){const auto entry=native_ps5_string_export_v1(i);if(name==entry.name.data())return RustStringAddress(i);}
#endif
    if(uint32_t(module)==2 && name=="snprintf")return SnprintfAddress();
    if(uint32_t(module)==2 && name=="vsnprintf")return VsnprintfAddress();
    if(uint32_t(module)==2 && name=="strerror")return StrerrorAddress();
    if(uint32_t(module)==2 && name=="__isthreaded")return IsThreadedAddress();
    if (uint32_t(module) != 1 && uint32_t(module) != 0x2001) return {};
    if (name == "sceKernelDlsym") return DlsymAddress();
    if (name == "getpid") return GetpidAddress();
    if (name == "__error") return ErrorAddress();
    return {};
}
uint64_t BootstrapServices::Dlsym(GuestCpu& cpu) const noexcept {
    constexpr uint64_t badAddress = 0x8002000e, missing = 0x80020003;
    if (!cpu.memory || !cpu.registers[6] || !cpu.registers[2]) return badAddress;
    try {
        std::string name;
        for (size_t i = 0; i < 4096; ++i) {
            if (cpu.registers[6] > UINT64_MAX-i) return badAddress;
            auto byte = cpu.memory->Load(cpu.registers[6]+i,1);
            if (!byte) {
                auto result = Resolve(cpu.registers[7], name);
                if (!result) return missing;
                cpu.memory->Store(cpu.registers[2],*result,8);
                return 0;
            }
            name.push_back(static_cast<char>(byte));
        }
    } catch (...) { return badAddress; }
    return badAddress;
}
void BootstrapServices::RegisterProcessParameters(uint32_t module,uint64_t guestAddress) {
    if (!guestAddress || processParameters.contains(module) || processParameters.size()>=256)
        throw std::invalid_argument("Invalid or duplicate module metadata registration");
    const auto size=memory.Load(guestAddress,8);
    if (size<24 || size>65536 || size>UINT64_MAX-guestAddress)
        throw std::invalid_argument("Invalid process parameter size");
    for (uint64_t offset=0;offset<size;++offset) (void)memory.Load(guestAddress+offset,1);
    processParameters.emplace(module,guestAddress);
}
uint64_t BootstrapServices::ModuleMember(GuestCpu& cpu) const noexcept {
    // Initial supported member: process parameters (8). Return a guest errno
    // code internally; Dispatch applies the callable library error convention.
    if (cpu.registers[6]!=8) return 22;
    const auto member=processParameters.find(static_cast<uint32_t>(cpu.registers[7]));
    if (member==processParameters.end()) return 3;
    try {
        if (!cpu.registers[2]) return 14;
        memory.Store(cpu.registers[2],member->second,8);
        return 0;
    } catch (...) { return 14; }
}
bool BootstrapServices::Dispatch(GuestCpu& cpu) const noexcept {
    if (cpu.fault || cpu.stop != CpuStop::Call || !cpu.memory) return false;
    bool rustString=false;
#ifdef NATIVE_PS5_RUST_PAYLOAD
    rustString=cpu.target>=RustStringAddress(0) && cpu.target<=RustStringAddress(9) && (cpu.target-RustStringAddress(0))%16==0;
#endif
    if (!rustString && cpu.target != DlsymAddress() && cpu.target != GetpidAddress() && cpu.target != SyscallAddress() && cpu.target != ErrorAddress() && cpu.target!=SnprintfAddress() && cpu.target!=VsnprintfAddress() && cpu.target!=StrerrorAddress()) return false;
    if (cpu.target == SyscallAddress() && cpu.registers[0] != 20 && cpu.registers[0] != 649 && cpu.registers[0] != 3 && cpu.registers[0] != 4 && cpu.registers[0] != 6 && cpu.registers[0] != 105 && cpu.registers[0]!=477 && cpu.registers[0]!=73) { cpu.fault = 8; return false; }
    try {
        if (cpu.memory!=&memory || cpu.nextRip > UINT64_MAX-cpu.loadBias) { cpu.fault = 8; return false; }
        const auto returnAddress = cpu.loadBias+cpu.nextRip;
        if (!cpu.memory->Executable(returnAddress)) { cpu.fault = 8; return false; }
        auto oldStack = cpu.registers[4];
        if (oldStack < 8) { cpu.fault = 8; return false; }
        cpu.memory->Store(oldStack-8,returnAddress,8);
        cpu.registers[4] = oldStack-8;
#ifdef NATIVE_PS5_RUST_PAYLOAD
        if(rustString){
            const auto operation=(cpu.target-RustStringAddress(0))/16;
            if(operation==8){
                const auto result=native_ps5_getenv_v1({&memory,RustRead,RustWrite},reinterpret_cast<uintptr_t>(rustEnvironment.data()),cpu.registers[7]);
                if(result.fault){cpu.fault=result.fault;return false;}cpu.registers[0]=result.value;
            } else if(operation>=6){

                const auto result=native_ps5_heap_call_v1(rustHeap,operation,cpu.registers[7],cpu.registers[6]);
                if(result.fault==12){memory.Store(ErrnoAddress(),12,4);cpu.registers[0]=0;}
                else if(result.fault){cpu.fault=result.fault;return false;}
                else cpu.registers[0]=result.value;
            } else {
            const auto result=native_ps5_libc_call_v1({&memory,RustRead,RustWrite},(cpu.target-RustStringAddress(0))/16,{{cpu.registers[7],cpu.registers[6],cpu.registers[2],cpu.registers[1],cpu.registers[8],cpu.registers[9]},cpu.registers[4]});
            if(result.fault){cpu.fault=result.fault;return false;}cpu.registers[0]=result.value;
            }
        } else
#endif
        if (cpu.target == DlsymAddress()) {
            cpu.registers[0] = Dlsym(cpu);
        } else if(cpu.target==ErrorAddress()) {
            cpu.registers[0]=ErrnoAddress();
        } else if(cpu.target==StrerrorAddress()) {
            const auto [text,known]=GuestErrorText(static_cast<int32_t>(cpu.registers[7]));
            if(text.size()>=errorText.size())throw std::runtime_error("Guest error text exceeds buffer");
            if(!known)memory.Store(ErrnoAddress(),22,4);
            std::memcpy(errorText.data(),text.c_str(),text.size()+1);
            cpu.registers[0]=reinterpret_cast<uintptr_t>(errorText.data());
        } else if(cpu.target==SnprintfAddress() || cpu.target==VsnprintfAddress()) {
#ifdef NATIVE_PS5_RUST_PAYLOAD
            const auto result=native_ps5_format_call_v1({&memory,RustRead,RustWrite},cpu.target==SnprintfAddress()?1:2,{{cpu.registers[7],cpu.registers[6],cpu.registers[2],cpu.registers[1],cpu.registers[8],cpu.registers[9]},cpu.registers[4]});
            if(result.fault)memory.Store(ErrnoAddress(),result.fault,4);
            cpu.registers[0]=result.value;
#else
            uint32_t error=0;
            try {
                if(cpu.target==VsnprintfAddress()){
                    FormatVaArguments arguments(memory,cpu.registers[1]);
                    cpu.registers[0]=GuestSnprintf(memory,cpu.registers[7],cpu.registers[6],cpu.registers[2],[&](){return arguments.Next();});
                }else{
                    FormatArguments arguments(cpu);
                    cpu.registers[0]=GuestSnprintf(memory,cpu.registers[7],cpu.registers[6],cpu.registers[2],[&](){return arguments.Next();});
                }
            }catch(const FormatUnsupported&){error=22;}
             catch(const std::bad_alloc&){error=12;}
             catch(const std::runtime_error&){error=14;}
            if(error){memory.Store(ErrnoAddress(),error,4);cpu.registers[0]=UINT32_MAX;}
#endif
        } else {
            if (cpu.target == GetpidAddress()) cpu.registers[10] = cpu.registers[1];
            cpu.registers[11] = cpu.flags;
            cpu.registers[1] = SyscallAddress()+2;
            IoResult result;
            const auto number=cpu.target==GetpidAddress()?20:cpu.registers[0];
            if(number==649) result.error=ModuleMember(cpu);
            else if(number==477) result=allocations.Map(cpu.registers[7],cpu.registers[6],cpu.registers[2],cpu.registers[10],cpu.registers[8],cpu.registers[9]);
            else if(number==73) result=allocations.Unmap(cpu.registers[7],cpu.registers[6]);
            else if(number==3) result=descriptors.Read(memory,cpu.registers[7],cpu.registers[6],cpu.registers[2]);
            else if(number==4) result=descriptors.Write(memory,cpu.registers[7],cpu.registers[6],cpu.registers[2]);
            else if(number==105) result=descriptors.SetSocketOption(memory,cpu.registers[7],cpu.registers[6],cpu.registers[2],cpu.registers[10],cpu.registers[8]);
            else if(number==6) result=descriptors.Close(cpu.registers[7]);
            else result.value=static_cast<uint32_t>(getpid());
            // This entry models the callable library syscall stub, including
            // its error continuation, not just the CPU SYSCALL instruction.
            if(result.error) memory.Store(ErrnoAddress(),result.error,4);
            cpu.registers[0]=result.error?UINT64_MAX:result.value;
            cpu.flags=(cpu.flags&~uint64_t(1))|(result.error!=0);
        }
        // A guest output pointer can alias the saved return address. Observe the
        // resulting guest RET instead of silently forcing the original caller PC.
        cpu.stop = CpuStop::Return;
        return CompleteGuestReturn(cpu);
    } catch (...) { cpu.fault = 8; return false; }
}
}
