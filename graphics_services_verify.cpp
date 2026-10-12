#include "graphics_services.hpp"
#include <iostream>
#include <stdexcept>

int main() {
    try {
        using namespace NativePS5;
        GuestMemory memory;
        std::array<std::byte,32> code{},stack{};
        std::array<uint64_t,2> emptyPacket{};
        memory.Add(code,false,true); memory.Add(stack,true);
        memory.Add(std::as_writable_bytes(std::span(emptyPacket)),false);
        unsigned calls=0;
        GraphicsServices bridge(memory,[&](const GuestSubmission&) { ++calls; throw std::runtime_error("backend failure probe"); });
        GuestCpu cpu;
        cpu.memory=&memory; cpu.loadBias=reinterpret_cast<uintptr_t>(code.data()); cpu.nextRip=1;
        cpu.registers[4]=reinterpret_cast<uintptr_t>(stack.data()+stack.size());
        cpu.registers[7]=reinterpret_cast<uintptr_t>(emptyPacket.data()); cpu.registers[0]=0xdeadbeef;
        cpu.target=*bridge.Resolve("sceAgcDriverSubmitDcb"); cpu.stop=CpuStop::Call;
        if (bridge.Dispatch(cpu) || cpu.fault!=9 || calls!=1 || cpu.registers[0]!=0xdeadbeef
            || bridge.LastError()!="backend failure probe" || !bridge.QueueState().userConfig.empty())
            throw std::runtime_error("Backend failure was acknowledged or lost");
        cpu.fault=0; cpu.registers[4]=0; cpu.stop=CpuStop::Call;
        if (bridge.Dispatch(cpu) || cpu.fault!=9 || calls!=1) throw std::runtime_error("Invalid stack reached backend");
        cpu.fault=0; cpu.registers[4]=reinterpret_cast<uintptr_t>(stack.data()+stack.size()); cpu.registers[7]=0;
        if (bridge.Dispatch(cpu) || cpu.fault!=9 || calls!=1) throw std::runtime_error("Invalid descriptor reached backend");
        cpu.fault=0; cpu.target=0;
        if (bridge.Dispatch(cpu) || cpu.fault || bridge.Resolve("unknown")) throw std::runtime_error("Unknown graphics service claimed");
        std::cout<<"PASS: graphics backend failure propagation, invalid stack/descriptor gating and unknown-service rejection\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
