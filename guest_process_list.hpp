#pragma once
#include "guest_cpu.hpp"
#include <cstring>
#include <memory>
#include <stdexcept>

namespace NativePS5 {
// Initial process-header layout observed in the selected payload binary.
// Credentials, descriptor tables and loaded-module records are not supplied yet.
// Keep this owner alive while its guest mappings can be accessed.
class GuestProcessList {
    static constexpr size_t RecordSize=0x400,Limit=256;
    struct Process { uint32_t pid; std::array<std::byte,RecordSize> bytes{}; };
    GuestMemory& kernel;
    uint64_t rootAddress,recordBase;
    std::array<std::byte,8> root{};
    std::vector<std::unique_ptr<Process>> processes;
public:
    GuestProcessList(GuestMemory& memory,uint64_t rootAddress,uint64_t recordBase)
        :kernel(memory),rootAddress(rootAddress),recordBase(recordBase) {
        if(!rootAddress || !recordBase || rootAddress>UINT64_MAX-root.size() ||
           recordBase>UINT64_MAX-RecordSize*Limit || rootAddress%8 || recordBase%16 ||
           (rootAddress<recordBase+RecordSize*Limit && recordBase<rootAddress+root.size()))
            throw std::invalid_argument("Invalid guest process-list address range");
        processes.reserve(Limit);
        kernel.AddAt(rootAddress,root,true);
    }
    GuestProcessList(const GuestProcessList&)=delete;
    GuestProcessList& operator=(const GuestProcessList&)=delete;
    uint64_t Add(uint32_t pid) {
        if(!pid || pid>INT32_MAX || processes.size()==Limit)
            throw std::invalid_argument("Invalid PID or process limit reached");
        for(const auto& process:processes) if(process->pid==pid)
            throw std::invalid_argument("Duplicate guest PID");
        auto process=std::make_unique<Process>();process->pid=pid;
        const auto head=kernel.Load(rootAddress,8);
        std::memcpy(process->bytes.data(),&head,8);
        std::memcpy(process->bytes.data()+0xbc,&pid,4);
        const auto address=recordBase+processes.size()*RecordSize;
        kernel.ValidateAccess(rootAddress,8,true);
        kernel.AddAt(address,process->bytes,true);
        processes.push_back(std::move(process)); // Reserved capacity; no allocation.
        kernel.Store(rootAddress,address,8);
        return address;
    }
    uint64_t RootAddress() const { return rootAddress; }
};
}
