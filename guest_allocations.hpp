#pragma once
#include "guest_descriptors.hpp"
#include <cstdlib>
#include <cstring>
#include <memory>

namespace NativePS5 {
// Initial anonymous data mappings only. GuestMemory must outlive this owner.
class GuestAllocations {
    static constexpr uint64_t Page=16384,MaxMapping=16*1024*1024,MaxTotal=256*1024*1024;
    struct Allocation {
        std::unique_ptr<std::byte,decltype(&std::free)> data{nullptr,&std::free};
        size_t size=0;
        uint64_t Address() const {return reinterpret_cast<uintptr_t>(data.get());}
        std::span<std::byte> Bytes() const {return {data.get(),size};}
    };
    GuestMemory& memory;
    std::vector<std::unique_ptr<Allocation>> allocations;
    uint64_t total=0;
public:
    explicit GuestAllocations(GuestMemory& memory):memory(memory){allocations.reserve(256);}
    GuestAllocations(const GuestAllocations&)=delete;
    GuestAllocations& operator=(const GuestAllocations&)=delete;
    ~GuestAllocations(){for(auto& a:allocations)memory.RemoveData(a->Address(),a->Bytes());}
    uint64_t BytesAllocated() const {return total;}
    IoResult Map(uint64_t hint,uint64_t length,uint64_t protection,uint64_t flags,uint64_t fd,uint64_t offset) {
        if(hint || !length || length>UINT64_MAX-(Page-1) || (protection!=1 && protection!=3) ||
           flags!=0x1002 || uint32_t(fd)!=UINT32_MAX || offset) return {0,22};
        const auto size=(length+Page-1)&~(Page-1);
        if(size>MaxMapping || size>MaxTotal-total || allocations.size()==256) return {0,12};
        try {
            auto allocation=std::make_unique<Allocation>();void* backing=nullptr;
            if(posix_memalign(&backing,Page,size)) return {0,12};
            allocation->data.reset(static_cast<std::byte*>(backing));allocation->size=size;
            std::memset(backing,0,size);
            const auto address=allocation->Address();
            memory.AddAt(address,allocation->Bytes(),protection==3);
            allocations.push_back(std::move(allocation)); // Pre-reserved capacity.
            total+=size;return {address,0};
        }catch(const std::bad_alloc&){return {0,12};}
    }
    IoResult Unmap(uint64_t address,uint64_t length) {
        if(address%Page || !length || length>UINT64_MAX-(Page-1)) return {0,22};
        const auto size=(length+Page-1)&~(Page-1);
        for(auto it=allocations.begin();it!=allocations.end();++it) {
            auto& a=**it;
            if(a.Address()!=address || a.size!=size)continue;
            if(!memory.RemoveData(address,a.Bytes()))return {0,22};
            total-=a.size;allocations.erase(it);return {0,0};
        }
        // Partial unmaps and unowned mappings are deliberately unsupported.
        return {0,22};
    }
};
}
