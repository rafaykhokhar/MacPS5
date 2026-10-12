#pragma once
#include "guest_cpu.hpp"
#include "guest_descriptors.hpp"
#include "guest_allocations.hpp"
#include <optional>
#include <map>
#include <string_view>

namespace NativePS5 {
class BootstrapServices {
public:
    explicit BootstrapServices(GuestMemory& memory);
    ~BootstrapServices();
    BootstrapServices(const BootstrapServices&) = delete;
    BootstrapServices& operator=(const BootstrapServices&) = delete;
    uint64_t DlsymAddress() const;
    uint64_t GetpidAddress() const;
    uint64_t SyscallAddress() const;
    uint64_t ErrorAddress() const;
    uint64_t SnprintfAddress() const;
    uint64_t VsnprintfAddress() const;
    uint64_t StrerrorAddress() const;
    uint64_t IsThreadedAddress() const;
#ifdef NATIVE_PS5_RUST_PAYLOAD
    uint64_t RustStringAddress(uint64_t index) const { return index<10?DlsymAddress()+96+index*16:0; }
#endif
    uint64_t ErrnoAddress() const;
    uint32_t ErrorValue() const;
    uint64_t AllocatedBytes() const { return allocations.BytesAllocated(); }
    std::optional<uint64_t> Resolve(uint64_t module, std::string_view name) const;
    bool Dispatch(GuestCpu& cpu) const noexcept;
    // Loader-owned metadata must stay mapped for the service's lifetime.
    std::pair<uint32_t,uint32_t> CreatePipe(size_t capacity=16384) { return descriptors.CreatePipe(capacity); }
    std::pair<uint32_t,uint32_t> CreatePacketControlPair(GuestMemory& kernel) {
        if (&kernel==&memory) throw std::invalid_argument("Kernel control requires a separate address space");
        return descriptors.CreatePacketControlPair(kernel);
    }
    std::pair<uint32_t,uint32_t> CreateKernelPipe(GuestMemory& kernel,uint64_t address) {
        if (&kernel==&memory) throw std::invalid_argument("Kernel pipe requires a separate address space");
        return descriptors.CreateKernelPipe(kernel,address);
    }
    void RegisterProcessParameters(uint32_t module, uint64_t guestAddress);
private:
    uint64_t ModuleMember(GuestCpu& cpu) const noexcept;
    GuestMemory& memory;
    mutable GuestAllocations allocations;
    mutable GuestDescriptors descriptors;
    std::map<uint32_t,uint64_t> processParameters;
#ifdef NATIVE_PS5_RUST_PAYLOAD
    std::array<std::byte,8> rustEnvironment{};
    void* rustHeap=nullptr;
    std::array<std::byte, 256> thunks{};
#else
    std::array<std::byte, 96> thunks{};
#endif
    mutable std::array<std::byte,128> errorText{};
    std::array<std::byte, 4> errnoStorage{};
    std::array<std::byte, 4> isThreaded{};
    uint64_t Dlsym(GuestCpu& cpu) const noexcept;
};
}
