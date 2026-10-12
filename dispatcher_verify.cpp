#include "dispatcher.hpp"
#include "elf_image.hpp"
#include "dispatch-registry.hpp"
#include "discovered.hpp"
#include "homebrew-nid.hpp"
#include <cstring>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>

namespace {
void Require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
uint64_t Address(auto& buffer) { return reinterpret_cast<uintptr_t>(buffer.data()); }
void CheckDispatcher() {
    NativePS5::GuestMemory memory;
    NativePS5::BootstrapServices services(memory);
    std::vector<std::byte> code(0x100),stack(256);
    memory.Add(code,false,true); memory.Add(stack,true);
    NativePS5::Dispatcher dispatcher;
    RegisterSynthetic(dispatcher);
    NativePS5::GuestCpu cpu;
    cpu.memory=&memory; cpu.loadBias=Address(code); cpu.rip=0x10; cpu.fuel=100;
    cpu.registers[4]=Address(stack)+stack.size();
    const auto initialStack=cpu.registers[4];
    // Caller -> callee -> caller -> conditional external branch -> missing block.
    auto result=dispatcher.Run(cpu,services,100);
    Require(result.reason==NativePS5::DispatchStop::MissingCode && result.pc==0x80, "Taken external branch dispatch failed");
    Require(cpu.registers[0]==42 && cpu.registers[4]==initialStack && !cpu.fault, "Nested call/return corrupted state");
    cpu.rip=0x50; cpu.stop=NativePS5::CpuStop::None; cpu.flags=2; cpu.fuel=10;
    result=dispatcher.Run(cpu,services,100);
    Require(result.reason==NativePS5::DispatchStop::MissingCode && result.pc==0x52, "Untaken external branch fallthrough failed");
    cpu.rip=0x60; cpu.fuel=3; cpu.stop=NativePS5::CpuStop::None;
    result=dispatcher.Run(cpu,services,100);
    Require(result.reason==NativePS5::DispatchStop::Fuel && cpu.rip==0x60 && cpu.fuel==0, "Native loop did not exhaust fuel");
    cpu.rip=0x70; cpu.fuel=10; cpu.stop=NativePS5::CpuStop::None;
    result=dispatcher.Run(cpu,services,100);
    Require(result.reason==NativePS5::DispatchStop::MissingCode && result.pc==0x90 && cpu.registers[4]==initialStack && cpu.stop==NativePS5::CpuStop::Call, "Missing callee mutated stack or lost pending call");
    cpu.rip=0x10; cpu.stop=NativePS5::CpuStop::None;
    result=dispatcher.Run(cpu,services,0);
    Require(result.reason==NativePS5::DispatchStop::TransitionLimit && cpu.rip==0x10, "Zero transition budget changed state");
    cpu.rip=0x40; cpu.stop=NativePS5::CpuStop::Return;
    cpu.registers[4]=Address(stack);
    memory.Store(cpu.registers[4],Address(stack),8);
    result=dispatcher.Run(cpu,services,10);
    Require(result.reason==NativePS5::DispatchStop::Fault && cpu.fault==6 && cpu.registers[4]==Address(stack), "Invalid return failed imprecisely");
    bool rejected=false;
    try { RegisterSynthetic(dispatcher); } catch (const std::exception&) { rejected=true; }
    Require(rejected,"Duplicate code registration accepted");
    std::cout << "PASS: dispatcher calls, returns, taken/untaken external branches, missing code, fuel and invalid-return state\n";
}
}

int main(int argc,char** argv) {
    try {
        CheckDispatcher();
        if (argc!=2) throw std::runtime_error("Usage: dispatcher-verify crispy-doom.elf");
        std::ifstream stream(argv[1],std::ios::binary);
        if (!stream) throw std::runtime_error("Cannot open ELF");
        std::vector<char> file((std::istreambuf_iterator<char>(stream)),{});
        NativePS5::ElfImage image(std::as_bytes(std::span(file)));
        NativePS5::GuestMemory memory;
        NativePS5::BootstrapServices services(memory);
        image.Relocate([&](std::string_view name) { return services.Resolve(1,name); });
        for (auto segment:image.Segments()) {
            auto bytes=image.Read(segment.address,segment.memorySize);
            memory.Add({const_cast<std::byte*>(bytes.data()),bytes.size()},segment.flags&2,segment.flags&1);
        }
        std::vector<std::byte> stack(65536),args(64),descriptors(16),resultBuffer(8);
        memory.Add(stack,true); memory.Add(args,true); memory.Add(descriptors,true); memory.Add(resultBuffer,true);
        // Explicitly unavailable PS5 descriptors: the real payload must reject them.
        memory.Store(Address(descriptors),UINT64_MAX,8);
        memory.Store(Address(descriptors)+8,UINT64_MAX,8);
        memory.Store(Address(args),services.DlsymAddress(),8);
        memory.Store(Address(args)+8,Address(descriptors),8);
        memory.Store(Address(args)+16,Address(descriptors)+8,8);
        memory.Store(Address(args)+40,Address(resultBuffer),8);
        NativePS5::GuestCpu cpu;
        cpu.memory=&memory; cpu.loadBias=image.LoadBias(); cpu.rip=0x40; cpu.fuel=20000000;
        cpu.registers[4]=Address(stack)+stack.size()-8; cpu.registers[7]=Address(args);
        const auto initialStack=cpu.registers[4];
        NativePS5::Dispatcher dispatcher;
        Register_discovered(dispatcher,image);
        bool checkedNid=false;
        for (size_t step=0;step<2000000;++step) {
            const auto result=dispatcher.Run(cpu,services,1);
            Require(result.reason==NativePS5::DispatchStop::TransitionLimit,"Homebrew stopped before NID completion");
            if (cpu.stop==NativePS5::CpuStop::Return && cpu.rip==0xc10) {
                for (size_t i=0;i<sizeof(expectedHomebrewNid);++i)
                    Require(memory.Load(cpu.registers[0]+i,1)==static_cast<unsigned char>(expectedHomebrewNid[i]),"Real SHA1/NID result differs from Python hashlib reference");
                checkedNid=true;
                break;
            }
        }
        Require(checkedNid,"Homebrew NID routine exceeded transition budget");
        auto result=dispatcher.Run(cpu,services,2000000);
        Require(result.reason==NativePS5::DispatchStop::MissingCode && result.pc==0x373 && !cpu.fault,"Unexpected homebrew dispatch boundary");
        Require(cpu.rip==0x373 && cpu.nextRip==0x316b,"Unsupported instruction lost guest state");
        Require(cpu.registers[4]==initialStack-56,"Discovered guest call frames corrupted");
        Require(memory.Load(Address(resultBuffer),4)==0xfffffff7,"Real kernel initialization failed to return EBADF");
        Require(cpu.registers[0]==0,"Failed exit-symbol lookup unexpectedly succeeded");
        std::cout << "PASS: real homebrew SHA1 and NID encoding match independent Python hashlib reference; NID="<<expectedHomebrewNid<<"\n"
                  << "STOP: deliberate guest UD2 at 0x373 after failed exit-symbol lookup; PS5 kernel environment absent; gameplay not reached\n";
        // Runtime code identity validation must precede registration. This image
        // is an independent mapping, so the executed input above stays untouched.
        NativePS5::ElfImage changed(std::as_bytes(std::span(file)));
        auto code=changed.Read(0x40,1);
        *const_cast<std::byte*>(code.data())^=std::byte(1);
        NativePS5::Dispatcher rejectedRegistry;
        bool rejected=false;
        try { Register_discovered(rejectedRegistry,changed); } catch (const std::runtime_error&) { rejected=true; }
        Require(rejected,"Changed executable code accepted by generated registry");
        return 0;
    } catch (const std::exception& error) { std::cerr << "FAIL: " << error.what() << '\n'; return 1; }
}
