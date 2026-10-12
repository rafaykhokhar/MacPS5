#include "guest_cpu.hpp"
#include <bit>
#include <iostream>
#include <stdexcept>

extern "C" void cpu_add64(NativePS5::GuestCpu*);
extern "C" void cpu_sub64(NativePS5::GuestCpu*);
extern "C" void cpu_add32(NativePS5::GuestCpu*);
extern "C" void cpu_inc64(NativePS5::GuestCpu*);
extern "C" void cpu_high_registers(NativePS5::GuestCpu*);
extern "C" void cpu_indexed_load(NativePS5::GuestCpu*);
extern "C" void cpu_memory_add(NativePS5::GuestCpu*);
extern "C" void cpu_loop(NativePS5::GuestCpu*);

namespace {
void Require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
void Arithmetic(uint64_t a, uint64_t b) {
    const __int128 sa = std::bit_cast<int64_t>(a), sb = std::bit_cast<int64_t>(b);
    const __int128 low = INT64_MIN, high = INT64_MAX;
    for (unsigned operation = 0; operation < 4; ++operation) {
        NativePS5::GuestCpu cpu;
        cpu.registers[0] = a; cpu.registers[1] = b; cpu.flags = 3; cpu.fuel = 8;
        uint64_t expected, lhs = a, rhs = b;
        bool carry, overflow;
        if (operation == 0) {
            cpu_add64(&cpu); expected = a+b; carry = expected < a; overflow = sa+sb < low || sa+sb > high;
        } else if (operation == 1) {
            cpu_sub64(&cpu); expected = a-b; carry = a < b; overflow = sa-sb < low || sa-sb > high;
        } else if (operation == 2) {
            cpu_add32(&cpu); lhs = uint32_t(a); rhs = uint32_t(b); expected = uint32_t(lhs+rhs); carry = lhs+rhs > UINT32_MAX;
            int64_t value = int64_t(std::bit_cast<int32_t>(uint32_t(a))) + int64_t(std::bit_cast<int32_t>(uint32_t(b)));
            overflow = value < INT32_MIN || value > INT32_MAX;
        } else {
            cpu_inc64(&cpu); rhs = 1; expected = a+1; carry = true; overflow = a == uint64_t(INT64_MAX);
        }
        Require(cpu.fault == 0 && cpu.stop == NativePS5::CpuStop::Return && cpu.target == 0, "Arithmetic stop mismatch");
        Require(cpu.registers[0] == expected, "Arithmetic result mismatch");
        Require(bool(cpu.flags&1) == carry && bool(cpu.flags&0x800) == overflow, "Native carry/overflow mismatch");
        Require(bool(cpu.flags&0x40) == (expected == 0), "Native zero mismatch");
        Require(bool(cpu.flags&0x80) == bool(expected & (uint64_t(1) << (operation == 2 ? 31 : 63))), "Native sign mismatch");
        const bool auxiliary = operation == 1 ? (lhs&15)<(rhs&15) : (lhs&15)+(rhs&15)>15;
        Require(bool(cpu.flags&0x10) == auxiliary, "Native auxiliary carry mismatch");
    }
    NativePS5::GuestCpu highRegisters;
    highRegisters.registers[8] = a; highRegisters.registers[9] = b; highRegisters.fuel = 8;
    cpu_high_registers(&highRegisters);
    Require(highRegisters.registers[8] == a+b && highRegisters.registers[9] == b, "REX register extension mismatch");
}

void Memory() {
    std::vector<std::byte> storage(64);
    NativePS5::GuestMemory memory;
    memory.Add(storage, true);
    uint64_t address = reinterpret_cast<uintptr_t>(storage.data());
    memory.Store(address+4,0xfedcba9876543210,8);
    NativePS5::GuestCpu load;
    load.memory = &memory; load.registers[3] = address; load.registers[1] = 3; load.fuel = 8;
    cpu_indexed_load(&load);
    Require(load.fault == 0 && load.registers[0] == 0xfedcba9876543210, "SIB/negative displacement load mismatch");
    std::vector<std::byte> readOnly(8);
    NativePS5::GuestMemory protectedMemory;
    protectedMemory.Add(readOnly, false);
    NativePS5::GuestCpu store;
    store.memory = &protectedMemory; store.registers[3] = reinterpret_cast<uintptr_t>(readOnly.data());
    store.registers[0] = 1; store.flags = 0x847; store.fuel = 8;
    cpu_memory_add(&store);
    Require(store.fault == 2 && store.flags == 0x847 && protectedMemory.Load(store.registers[3],8) == 0, "Faulting arithmetic write changed architectural state");
    std::vector<std::byte> executable(8);
    NativePS5::GuestMemory codeMemory;
    codeMemory.Add(executable,true,true);
    NativePS5::GuestCpu code;
    code.memory = &codeMemory;
    guest_store(&code, reinterpret_cast<uintptr_t>(executable.data()), 0x90, 1);
    Require(code.fault == 2, "Self-modifying code write must trap until invalidation exists");
}
}

int main() {
    try {
        uint64_t boundaries[] = {0,1,15,16,0x7fffffff,0x80000000,0xffffffff,0x7fffffffffffffff,0x8000000000000000,UINT64_MAX};
        for (auto a : boundaries) for (auto b : boundaries) Arithmetic(a,b);
        uint64_t state = 0x13579bdf2468ace0;
        auto next = [&]() { state ^= state<<13; state ^= state>>7; state ^= state<<17; return state; };
        for (unsigned i = 0; i < 10000; ++i) { auto a=next(), b=next(); Arithmetic(a,b); }
        Memory();
        NativePS5::GuestCpu loop;
        loop.fuel = 100;
        cpu_loop(&loop);
        Require(loop.fault == 0 && loop.stop == NativePS5::CpuStop::Fuel && loop.fuel == 0 && loop.rip == 0, "Backward loop exceeded instruction budget");
        std::cout << "PASS: emitted ARM64 32/64-bit arithmetic, flags, extended registers, SIB addressing and guarded-write fault semantics\n";
        return 0;
    } catch (const std::exception& error) { std::cerr << "FAIL: " << error.what() << "\n"; return 1; }
}
