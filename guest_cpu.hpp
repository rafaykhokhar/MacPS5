#pragma once
#include <array>
#include <cstddef>
#include <cstdint>
#include <span>
#include <vector>

namespace NativePS5 {

struct MemoryRegion {
    uint64_t address;
    std::span<std::byte> bytes;
    bool writable;
    bool executable;
};

class GuestMemory {
public:
    void Add(std::span<std::byte> bytes, bool writable, bool executable = false);
    // The caller owns backing storage and must keep it alive and stationary.
    // Guest addresses are identifiers, never pointers to dereference on the host.
    void AddAt(uint64_t address, std::span<std::byte> bytes, bool writable, bool executable = false);
    // Owner-only removal: match the whole region and backing; executable regions
    // require translation invalidation and cannot be removed through this API.
    bool RemoveData(uint64_t address, std::span<std::byte> bytes) noexcept;
    uint64_t Load(uint64_t address, uint64_t size) const;
    void Store(uint64_t address, uint64_t value, uint64_t size);
    void ReadVector(uint64_t address, std::span<std::byte> output) const;
    void WriteVector(uint64_t address, std::span<const std::byte> input);
    void ValidateAccess(uint64_t address, uint64_t size, bool writable=false) const;
    void ReadBytes(uint64_t address, std::span<std::byte> output) const;
    void WriteBytes(uint64_t address, std::span<const std::byte> input);
    bool Executable(uint64_t address) const;
private:
    std::vector<MemoryRegion> regions;
    const MemoryRegion& Find(uint64_t address, uint64_t size) const;
};

enum class CpuStop : uint64_t { None, Call, Branch, Return, Fuel };
struct GuestCpu {
    std::array<uint64_t, 16> registers{};
    uint64_t rip = 0;
    uint64_t flags = 2;
    uint64_t loadBias = 0;
    uint64_t fault = 0;
    uint64_t fuel = 0;
    CpuStop stop = CpuStop::None;
    uint64_t target = 0;
    GuestMemory* memory = nullptr;
    uint64_t nextRip = 0;
    alignas(16) std::array<std::array<std::byte,32>,16> vectors{};
};
static_assert(offsetof(GuestCpu, rip) == 128 && offsetof(GuestCpu, flags) == 136);
static_assert(offsetof(GuestCpu, loadBias) == 144 && offsetof(GuestCpu, fault) == 152);
static_assert(offsetof(GuestCpu, fuel) == 160 && offsetof(GuestCpu, stop) == 168);
static_assert(offsetof(GuestCpu, target) == 176 && offsetof(GuestCpu, memory) == 184);
bool EnterGuestCall(GuestCpu& cpu, uint64_t returnAddress) noexcept;
bool CompleteGuestReturn(GuestCpu& cpu) noexcept;
static_assert(offsetof(GuestCpu, nextRip) == 192);
static_assert(offsetof(GuestCpu, vectors) == 208);

}

extern "C" uint64_t guest_load(NativePS5::GuestCpu*, uint64_t address, uint64_t size) noexcept;
extern "C" void guest_store(NativePS5::GuestCpu*, uint64_t address, uint64_t value, uint64_t size) noexcept;
extern "C" uint64_t guest_flags(NativePS5::GuestCpu*, uint64_t a, uint64_t b, uint64_t result, uint64_t bits, uint64_t operation) noexcept;
extern "C" uint64_t guest_condition(NativePS5::GuestCpu*, uint64_t condition) noexcept;
extern "C" uint64_t guest_shift_flags(NativePS5::GuestCpu*, uint64_t original, uint64_t count, uint64_t result, uint64_t bits, uint64_t operation) noexcept;
extern "C" void guest_vector_load(NativePS5::GuestCpu*, uint64_t address, uint64_t reg, uint64_t size, uint64_t aligned) noexcept;
extern "C" void guest_vector_store(NativePS5::GuestCpu*, uint64_t address, uint64_t reg, uint64_t size, uint64_t aligned) noexcept;

extern "C" void guest_vector_xor(NativePS5::GuestCpu*, uint64_t address, uint64_t destination, uint64_t first, uint64_t size) noexcept;
