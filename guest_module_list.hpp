#pragma once
#include "guest_cpu.hpp"
#include <cstring>
#include <memory>
#include <string>
#include <stdexcept>
#ifdef NATIVE_PS5_RUST_PAYLOAD
#include "rust/payload_abi.h"
#endif

namespace NativePS5 {
// Layout pinned to the selected payload's kernel_dynlib_obj/resolve accesses.
// Owns mappings; keep this object and both address spaces alive during use.
class GuestModuleList {
    static constexpr size_t Limit=128,Stride=0x10000,RecordSize=0x180,DynamicSize=0x120;
    struct Module { uint32_t handle; std::vector<std::byte> bytes; };
    GuestMemory& kernel;
    const GuestMemory& user;
    uint64_t rootAddress,recordBase;
#ifdef NATIVE_PS5_RUST_PAYLOAD
    RustModuleContainer rootRecord=native_ps5_fixture_module_container_v1();
    std::span<std::byte> root=rootRecord.bytes;
#else
    std::array<std::byte,8> root{};
#endif
    std::vector<std::unique_ptr<Module>> modules;
public:
    enum class ExportKind { Function, Object };
    struct Export {
        std::string nid; uint64_t address;
        ExportKind kind=ExportKind::Function;
        uint64_t size=0;
        bool writable=false;
    };
    GuestModuleList(GuestMemory& kernel,const GuestMemory& user,uint64_t rootAddress,uint64_t recordBase)
        :kernel(kernel),user(user),rootAddress(rootAddress),recordBase(recordBase) {
        if(&kernel==&user || !rootAddress || !recordBase || rootAddress%8 || recordBase%16 ||
           rootAddress>UINT64_MAX-root.size() || recordBase>UINT64_MAX-Stride*Limit ||
           (rootAddress<recordBase+Stride*Limit && recordBase<rootAddress+root.size()))
            throw std::invalid_argument("Invalid guest module-list address space or range");
        modules.reserve(Limit);
        kernel.AddAt(rootAddress,root,true);
    }
    GuestModuleList(const GuestModuleList&)=delete;
    GuestModuleList& operator=(const GuestModuleList&)=delete;
    uint64_t RootAddress() const { return rootAddress; }
    uint64_t Add(uint32_t handle,uint64_t base,std::span<const Export> exports) {
        if(!handle || modules.size()==Limit || exports.empty() || exports.size()>128)
            throw std::invalid_argument("Invalid guest module handle, exports or capacity");
        for(const auto& module:modules) if(module->handle==handle)
            throw std::invalid_argument("Duplicate guest module handle");
        constexpr std::string_view alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
        for(size_t i=0;i<exports.size();++i) {
            const auto& entry=exports[i];
            if(entry.nid.size()!=11 || entry.nid.find_first_not_of(alphabet)!=std::string::npos ||
               entry.address<=base)
                throw std::invalid_argument("Invalid native guest module export");
            if(entry.kind==ExportKind::Function){
                if(entry.size || entry.writable || !user.Executable(entry.address))
                    throw std::invalid_argument("Invalid callable guest export");
            }else if(entry.kind==ExportKind::Object){
                if(!entry.size || entry.size>16*1024*1024 || user.Executable(entry.address))
                    throw std::invalid_argument("Invalid guest data export");
                user.ValidateAccess(entry.address,entry.size,entry.writable);
            }else throw std::invalid_argument("Unknown guest export kind");
            for(size_t j=0;j<i;++j) if(entry.nid==exports[j].nid)
                throw std::invalid_argument("Duplicate guest module export");
        }
        const size_t symbols=RecordSize+DynamicSize,symbolBytes=(exports.size()+1)*24;
        const size_t strings=symbols+symbolBytes,stringBytes=1+exports.size()*12;
        auto module=std::make_unique<Module>();module->handle=handle;module->bytes.resize(strings+stringBytes);
        const auto address=recordBase+modules.size()*Stride;
        auto put=[&](size_t offset,uint64_t value,size_t size=8){std::memcpy(module->bytes.data()+offset,&value,size);};
        put(0,kernel.Load(rootAddress,8));put(0x28,handle);put(0x30,base);put(0x148,address+RecordSize);
        put(RecordSize+0x28,address+symbols);put(RecordSize+0x30,symbolBytes);
        put(RecordSize+0x38,address+strings);put(RecordSize+0x40,stringBytes);
        for(size_t i=0;i<exports.size();++i) {
            const auto symbol=symbols+(i+1)*24,name=1+i*12;
            put(symbol,name,4);put(symbol+4,exports[i].kind==ExportKind::Function?0x12:0x11,1);put(symbol+6,1,2);
            put(symbol+8,exports[i].address-base);
            put(symbol+16,exports[i].size);
            std::memcpy(module->bytes.data()+strings+name,exports[i].nid.data(),11);
        }
        kernel.ValidateAccess(rootAddress,8,true);
        kernel.AddAt(address,module->bytes,true);
        modules.push_back(std::move(module));
        kernel.Store(rootAddress,address,8);
        return address;
    }
};
}
