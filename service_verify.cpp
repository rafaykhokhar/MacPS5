#include "bootstrap_services.hpp"
#include "elf_image.hpp"
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>
#include <unistd.h>

extern "C" void translated_start(NativePS5::GuestCpu*);
extern "C" void translated_crt_region(NativePS5::GuestCpu*);

namespace {
void Require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
uint64_t Read64(std::span<const std::byte> bytes) { uint64_t value; std::memcpy(&value,bytes.data(),8); return value; }
}

int main(int argc, char** argv) {
    try {
        if (argc != 2) throw std::runtime_error("Usage: service-verify crispy-doom.elf");
        std::ifstream stream(argv[1],std::ios::binary);
        if (!stream) throw std::runtime_error("Cannot open ELF");
        std::vector<char> file((std::istreambuf_iterator<char>(stream)),{});
        NativePS5::GuestMemory memory;
        NativePS5::BootstrapServices services(memory);
        NativePS5::ElfImage image(std::as_bytes(std::span(file)));
        image.Relocate([&](std::string_view name) { return services.Resolve(1,name); });
        for (auto segment : image.Segments()) {
            auto bytes = image.Read(segment.address,segment.memorySize);
            memory.Add({const_cast<std::byte*>(bytes.data()),bytes.size()},segment.flags&2,segment.flags&1);
        }
        std::vector<std::byte> stack(65536), arguments(64), names(64);
        memory.Add(stack,true); memory.Add(arguments,true); memory.Add(names,true);
        auto address = [](auto& bytes) { return reinterpret_cast<uintptr_t>(bytes.data()); };
        memory.Store(address(arguments),services.DlsymAddress(),8);
        NativePS5::GuestCpu cpu;
        cpu.memory = &memory; cpu.loadBias = image.LoadBias(); cpu.fuel = 20000000;
        cpu.registers[4] = address(stack)+stack.size()-8;
        cpu.registers[7] = address(arguments);
        translated_start(&cpu);
        Require(cpu.stop == NativePS5::CpuStop::Call && cpu.fault == 0 && cpu.nextRip == 0x8c, "Startup did not stop at runtime init");
        const auto callerStack = cpu.registers[4], callerRbp = cpu.registers[5], callerRbx = cpu.registers[3];
        Require(NativePS5::EnterGuestCall(cpu,cpu.loadBias+cpu.nextRip), "Failed guest call entry");
        unsigned calls = 0;
        for (unsigned steps = 0; steps < 16; ++steps) {
            translated_crt_region(&cpu);
            Require(cpu.fault == 0, "Runtime-init translation fault");
            if (cpu.stop == NativePS5::CpuStop::Return) break;
            Require(cpu.stop == NativePS5::CpuStop::Call && services.Dispatch(cpu), "Unimplemented runtime-init service");
            ++calls;
        }
        Require(cpu.stop == NativePS5::CpuStop::Return && cpu.registers[0] == 0 && calls == 2, "Runtime init did not complete through both real lookups");
        Require(Read64(image.Read(0x58f890,8)) == services.SyscallAddress(), "Runtime syscall pointer mismatch");
        Require(NativePS5::CompleteGuestReturn(cpu), "Guest return failed");
        Require(cpu.rip == 0x8c && cpu.registers[4] == callerStack && cpu.registers[5] == callerRbp && cpu.registers[3] == callerRbx, "Guest ABI return state mismatch");
        std::cout << "PASS: real translated __crt_syscall_init completed; sceKernelDlsym/getpid lookups, syscall thunk, call/return and callee-saved registers verified\n";
        auto invoke = [&](std::string_view name, uint64_t module, uint64_t output) {
            std::fill(names.begin(),names.end(),std::byte(0));
            std::memcpy(names.data(),name.data(),name.size());
            cpu.registers[7] = module; cpu.registers[6] = address(names); cpu.registers[2] = output;
            cpu.target = services.DlsymAddress(); cpu.stop = NativePS5::CpuStop::Call; cpu.nextRip = 0x8c;
            Require(services.Dispatch(cpu), "Dlsym dispatch failed");
        };
        memory.Store(address(arguments)+8,0x1234,8);
        invoke("not_implemented",1,address(arguments)+8);
        Require(cpu.registers[0] == 0x80020003 && memory.Load(address(arguments)+8,8) == 0x1234, "Unknown symbol falsely resolved");
        invoke("getpid",99,address(arguments)+8);
        Require(cpu.registers[0] == 0x80020003, "Unknown module accepted");
        invoke("getpid",0x2001,address(arguments)+8);
        Require(cpu.registers[0] == 0 && memory.Load(address(arguments)+8,8) == services.GetpidAddress(), "Kernel alias lookup failed");
        invoke("getpid",1,0);
        Require(cpu.registers[0] == 0x8002000e, "Invalid output pointer not rejected");
        invoke("__error",1,address(arguments)+8);
        Require(cpu.registers[0]==0 && memory.Load(address(arguments)+8,8)==services.ErrorAddress(),"Guest errno export failed to resolve");
        const auto errnoStack=cpu.registers[4];const auto errnoFlags=cpu.flags;
        cpu.target=services.ErrorAddress();cpu.stop=NativePS5::CpuStop::Call;
        Require(services.Dispatch(cpu) && cpu.registers[0]==services.ErrnoAddress() && cpu.registers[4]==errnoStack && cpu.flags==errnoFlags,"Guest errno pointer ABI failed");
        memory.Store(cpu.registers[0],123,4);
        Require(services.ErrorValue()==123,"Guest errno pointer does not address owned state");
        {
            NativePS5::GuestMemory otherMemory;
            NativePS5::BootstrapServices otherServices(otherMemory);
            Require(otherServices.ErrorValue()==0 && services.ErrorValue()==123,"Guest runtimes share errno state");
        }
        const auto bridgeStack = cpu.registers[4];
        cpu.registers[1] = 0x12345678; cpu.flags = 0x247;
        cpu.target = services.GetpidAddress(); cpu.stop = NativePS5::CpuStop::Call;
        Require(services.Dispatch(cpu) && cpu.registers[0] == uint64_t(getpid()), "getpid bridge returned wrong process id");
        Require(cpu.registers[10] == 0x12345678 && cpu.registers[11] == 0x247 && cpu.registers[1] == services.SyscallAddress()+2
                && cpu.flags == 0x246 && cpu.registers[4] == bridgeStack, "getpid syscall clobbers or stack incorrect");
        Require(services.ErrorValue()==123,"Successful getpid cleared errno");
        cpu.target = services.SyscallAddress(); cpu.stop = NativePS5::CpuStop::Call; cpu.registers[0] = 20; cpu.flags |= 1;
        Require(services.Dispatch(cpu) && cpu.registers[0] == uint64_t(getpid()) && !(cpu.flags&1), "Raw getpid syscall bridge failed");
        cpu.target = services.SyscallAddress(); cpu.stop = NativePS5::CpuStop::Call; cpu.registers[0] = 0xffff;
        Require(!services.Dispatch(cpu) && cpu.fault == 8, "Unsupported syscall returned fake success");
        cpu.fault = 0; cpu.target = services.GetpidAddress(); cpu.stop = NativePS5::CpuStop::Call;
        cpu.nextRip = UINT64_MAX; auto beforeRegisters = cpu.registers;
        Require(!services.Dispatch(cpu) && cpu.fault == 8 && cpu.registers == beforeRegisters, "Invalid bridge return modified registers");
        cpu.fault = 0; cpu.nextRip = 0x8c; cpu.registers[4] = 0;
        Require(!services.Dispatch(cpu) && cpu.fault == 8 && cpu.registers[4] == 0, "Invalid bridge stack was accepted");
        cpu.fault = 0; cpu.registers[4] = bridgeStack; cpu.target = services.DlsymAddress();
        std::fill(names.begin(),names.end(),std::byte(0));
        std::memcpy(names.data(),"getpid",6);
        cpu.registers[7] = 1; cpu.registers[6] = address(names); cpu.registers[2] = bridgeStack-8;
        const bool aliasedReturn = services.Dispatch(cpu);
        // Depending on allocation order, the thunk may precede the ELF load bias.
        // Either the actual overwritten target is observed or return validation faults.
        Require((aliasedReturn && cpu.loadBias+cpu.rip == services.GetpidAddress())
                || (!aliasedReturn && cpu.fault == 6 && cpu.registers[4] == bridgeStack-8), "Aliased return slot was silently ignored");
        std::cout << "PASS: missing symbol/module errors, invalid pointers, native getpid, syscall clobbers, aliased return and unsupported-syscall trap\n";
        std::cout << "Remaining unresolved ELF imports: " << image.Unresolved().size() << "; gameplay not reached\n";
        return 0;
    } catch (const std::exception& error) { std::cerr << "FAIL: " << error.what() << "\n"; return 1; }
}
