// Temporary C ABI transport for Rust-owned runtime migration tests.
// Behavior remains in existing runtime methods; no guest service semantics here.
#include "../bootstrap_services.hpp"
#include <string_view>
using namespace NativePS5;
static_assert(sizeof(GuestCpu)==720 && alignof(GuestCpu)==16);
extern "C" void* native_ps5_legacy_memory_new() noexcept {try{return new GuestMemory;}catch(...){return nullptr;}}
extern "C" void native_ps5_legacy_memory_delete(void* p) noexcept {delete static_cast<GuestMemory*>(p);}
extern "C" uint64_t native_ps5_legacy_memory_add(void* p,uint64_t address,unsigned char* bytes,uint64_t size,uint64_t writable,uint64_t executable) noexcept {
    try{static_cast<GuestMemory*>(p)->AddAt(address,{reinterpret_cast<std::byte*>(bytes),size},writable!=0,executable!=0);return 0;}catch(...){return 1;}
}
extern "C" void* native_ps5_legacy_services_new(void* p) noexcept {try{return new BootstrapServices(*static_cast<GuestMemory*>(p));}catch(...){return nullptr;}}
extern "C" void native_ps5_legacy_services_delete(void* p) noexcept {delete static_cast<BootstrapServices*>(p);}
extern "C" uint64_t native_ps5_legacy_resolve(void* p,uint64_t module,const char* name,uint64_t length) noexcept {
    try{return static_cast<BootstrapServices*>(p)->Resolve(module,std::string_view(name,length)).value_or(0);}catch(...){return 0;}
}
extern "C" uint64_t native_ps5_legacy_dispatch(void* p,void* cpu) noexcept {
    return static_cast<BootstrapServices*>(p)->Dispatch(*static_cast<GuestCpu*>(cpu));
}
