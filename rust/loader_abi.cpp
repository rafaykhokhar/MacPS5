// Temporary transport to the retained loader oracle. No loader policy lives here.
#include "../elf_image.hpp"
#include <cstdio>
extern "C" void* native_ps5_loader_oracle_new(const unsigned char* data,uint64_t size) noexcept {
    try {return new NativePS5::ElfImage({reinterpret_cast<const std::byte*>(data),size});}
    catch(const std::exception& e){std::fprintf(stderr,"Loader oracle: %s\n",e.what());return nullptr;}
}
extern "C" void native_ps5_loader_oracle_delete(void* image) noexcept {delete static_cast<NativePS5::ElfImage*>(image);}
extern "C" uint64_t native_ps5_loader_oracle_bias(void* image) noexcept {return static_cast<NativePS5::ElfImage*>(image)->LoadBias();}
extern "C" uint64_t native_ps5_loader_oracle_relocate(void* image) noexcept {
    try{static_cast<NativePS5::ElfImage*>(image)->Relocate([](std::string_view name)->std::optional<uint64_t>{if(name=="sceKernelDlsym")return 0x1234567800ULL;return {};});return 0;}catch(...){return 1;}
}
extern "C" uint64_t native_ps5_loader_oracle_read(void* image,uint64_t address,unsigned char* output,uint64_t size) noexcept {
    try{const auto bytes=static_cast<NativePS5::ElfImage*>(image)->Read(address,size);std::memcpy(output,bytes.data(),size);return 0;}catch(...){return 1;}
}
extern "C" uint64_t native_ps5_loader_oracle_applied(void* image) noexcept {return static_cast<NativePS5::ElfImage*>(image)->AppliedRelocations();}
extern "C" uint64_t native_ps5_loader_oracle_pending(void* image) noexcept {return static_cast<NativePS5::ElfImage*>(image)->Unresolved().size();}
