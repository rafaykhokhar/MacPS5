#include "elf_image.hpp"
#include "guest_cpu.hpp"
#include <algorithm>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>

extern "C" void translated_start(NativePS5::GuestCpu*);
extern "C" void translated_crt(NativePS5::GuestCpu*);

namespace {
void Require(bool condition, const char* message) { if (!condition) throw std::runtime_error(message); }
uint64_t Read64(std::span<const std::byte> bytes) {
    uint64_t value; std::memcpy(&value, bytes.data(), 8); return value;
}

void HelperTests() {
    NativePS5::GuestCpu cpu;
    for (uint64_t a = 0; a < 256; ++a) for (uint64_t b = 0; b < 256; ++b) {
        for (uint64_t operation = 0; operation <= 1; ++operation) {
            uint8_t result = operation == 0 ? uint8_t(a-b) : uint8_t(a+b);
            cpu.flags = guest_flags(&cpu, a, b, result, 8, operation);
            bool carry = operation == 0 ? a < b : a+b > 255;
            const int lhs = a < 128 ? int(a) : int(a)-256;
            const int rhs = b < 128 ? int(b) : int(b)-256;
            const int signedResult = operation == 0 ? lhs-rhs : lhs+rhs;
            Require(bool(cpu.flags&1) == carry, "Carry mismatch");
            Require(bool(cpu.flags&0x800) == (signedResult < -128 || signedResult > 127), "Overflow mismatch");
            Require(bool(cpu.flags&0x40) == (result == 0), "Zero mismatch");
            Require(bool(cpu.flags&0x80) == (result >= 128), "Sign mismatch");
            unsigned ones = 0;
            for (unsigned bit = 0; bit < 8; ++bit) ones += (result >> bit)&1;
            Require(bool(cpu.flags&4) == (ones%2 == 0), "Parity mismatch");
            Require(bool(cpu.flags&0x10) == (operation == 0 ? (a&15)<(b&15) : (a&15)+(b&15)>15), "Auxiliary carry mismatch");
            if (operation == 0) {
                Require(guest_condition(&cpu, 2) == (a<b), "Unsigned comparison mismatch");
                Require(guest_condition(&cpu, 12) == (lhs<rhs), "Signed comparison mismatch");
            }
        }
    }
    cpu.flags = 3;
    Require((guest_flags(&cpu, 255, 1, 0, 8, 3)&1) == 1, "INC must preserve carry");
    std::vector<std::byte> bytes(16);
    NativePS5::GuestMemory memory;
    memory.Add(bytes, true);
    cpu.memory = &memory;
    uint64_t start = reinterpret_cast<uintptr_t>(bytes.data());
    guest_store(&cpu, start+1, 0x0123456789abcdef, 8);
    Require(cpu.fault == 0 && guest_load(&cpu, start+1, 8) == 0x0123456789abcdef, "Unaligned memory access mismatch");
    guest_store(&cpu, start+12, 0, 8);
    Require(cpu.fault == 2, "Cross-boundary write accepted");
    cpu.fault = 0;
    guest_load(&cpu, UINT64_MAX, 8);
    Require(cpu.fault == 1, "Invalid read accepted");
    std::cout << "PASS: arithmetic flags checked over all 8-bit operand pairs; guarded memory checks\n";
}

void Startup(const char* path) {
    std::ifstream input(path, std::ios::binary);
    if (!input) throw std::runtime_error("Cannot open homebrew ELF");
    std::vector<char> file((std::istreambuf_iterator<char>(input)), {});
    NativePS5::ElfImage image(std::as_bytes(std::span(file)));
    image.Relocate([](std::string_view) -> std::optional<uint64_t> { return {}; });
    Require(!image.ImportsResolved(), "Test expects unresolved homebrew imports");
    NativePS5::GuestMemory memory;
    for (const auto& segment : image.Segments()) {
        auto bytes = image.Read(segment.address, segment.memorySize);
        memory.Add({const_cast<std::byte*>(bytes.data()), bytes.size()}, segment.flags & 2, segment.flags & 1);
    }
    std::vector<std::byte> stack(65536, std::byte(0xa5)), arguments(64);
    memory.Add(stack, true);
    memory.Add(arguments, true);
    constexpr uint64_t unresolvedCallback = 0xffff000000000001;
    std::memcpy(arguments.data(), &unresolvedCallback, 8);
    NativePS5::GuestCpu cpu;
    for (size_t i = 0; i < 16; ++i) cpu.registers[i] = 0x12340000 + i;
    cpu.registers[4] = reinterpret_cast<uintptr_t>(stack.data()+stack.size()-8);
    cpu.registers[7] = reinterpret_cast<uintptr_t>(arguments.data());
    cpu.memory = &memory;
    cpu.loadBias = image.LoadBias();
    cpu.fuel = 20000000;
    auto initial = cpu.registers;
    constexpr uint64_t bssBegin = 0x58f780, bssEnd = 0x80b2c0;
    auto bss = image.Read(bssBegin, bssEnd-bssBegin);
    std::memset(const_cast<std::byte*>(bss.data()), 0xa5, bss.size());
    translated_start(&cpu);
    Require(cpu.fault == 0, "Startup memory fault");
    Require(cpu.stop == NativePS5::CpuStop::Call && cpu.rip == 0x86 && cpu.target == image.LoadBias()+0x3a0, "Wrong startup stop boundary");
    Require(cpu.registers[4] == initial[4]-56 && cpu.registers[5] == initial[4]-8, "Stack frame mismatch");
    const unsigned pushed[] = {5,15,14,13,12,3,0};
    for (size_t i = 0; i < 7; ++i) Require(memory.Load(initial[4]-8*(i+1),8) == initial[pushed[i]], "Pushed register mismatch");
    Require(cpu.registers[0] == image.LoadBias()+bssBegin, "RIP-relative LEA mismatch");
    Require(cpu.registers[1] == bss.size() && cpu.registers[2] == bss.size(), "BSS loop counters mismatch");
    Require(cpu.registers[3] == initial[7] && cpu.registers[7] == initial[7], "Argument register mismatch");
    Require(Read64(bss.first(8)) == initial[7], "payload_args pointer not stored");
    Require(std::all_of(bss.begin()+8, bss.end(), [](std::byte value) { return value == std::byte(0); }), "Startup BSS loop failed to clear memory");
    Require(cpu.flags == 0x46, "Final comparison flags mismatch");
    Require(20000000-cpu.fuel == 4*bss.size()+19, "Unexpected instruction count");
    std::cout << "PASS: actual PS5 homebrew startup prefix executed as native ARM64; " << bss.size()
              << " BSS bytes initialized, stack/register/flags verified; stopped before call at 0x86\n";
    const uint64_t callerStack = cpu.registers[4];
    Require(NativePS5::EnterGuestCall(cpu, image.LoadBias()+0x8c), "Failed to enter translated guest call");
    Require(cpu.rip == 0x3a0 && memory.Load(callerStack-8,8) == image.LoadBias()+0x8c, "Guest call frame mismatch");
    translated_crt(&cpu);
    Require(cpu.fault == 0 && cpu.stop == NativePS5::CpuStop::Call && cpu.rip == 0x3c1 && cpu.target == unresolvedCallback, "Runtime-init callback boundary mismatch");
    Require(cpu.registers[7] == 1, "Runtime-init handle argument mismatch");
    std::string symbol;
    for (size_t i = 0; i < 128; ++i) {
        const auto c = memory.Load(cpu.registers[6]+i,1);
        if (!c) break;
        symbol.push_back(static_cast<char>(c));
    }
    Require(symbol == "sceKernelDlsym", "Runtime-init symbol argument mismatch");
    Require(cpu.registers[2] == cpu.registers[5]-16 && memory.Load(cpu.registers[2],8) == 0, "Runtime-init output slot mismatch");
    const auto callbackStack = cpu.registers[4];
    Require(!NativePS5::EnterGuestCall(cpu, image.LoadBias()+0x3c3) && cpu.fault == 5 && cpu.registers[4] == callbackStack, "Unresolved callback was dispatched");
    std::cout << "PASS: direct guest call and runtime-init prefix; stopped at unresolved sceKernelDlsym callback without invoking it\n";
    NativePS5::GuestCpu limited;
    limited.registers = initial; limited.loadBias = image.LoadBias(); limited.memory = &memory; limited.fuel = 1;
    translated_start(&limited);
    Require(limited.stop == NativePS5::CpuStop::Fuel && limited.rip == 0x41 && limited.registers[4] == initial[4]-8, "Fuel boundary incorrect");
    NativePS5::GuestCpu invalid;
    invalid.loadBias = image.LoadBias(); invalid.memory = &memory; invalid.fuel = 100;
    translated_start(&invalid);
    Require(invalid.fault == 2 && invalid.rip == 0x40 && invalid.registers[4] == 0, "Faulting push changed stack pointer");
    std::cout << "PASS: instruction-budget stop and precise failing stack write\n";
}
}

int main(int argc, char** argv) {
    try {
        if (argc != 2) throw std::runtime_error("Usage: startup-verify crispy-doom.elf");
        HelperTests();
        Startup(argv[1]);
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "FAIL: " << error.what() << "\n";
        return 1;
    }
}
