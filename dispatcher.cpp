#include "dispatcher.hpp"
#include <stdexcept>

namespace NativePS5 {
void Dispatcher::Add(uint64_t pc, Block block) {
    if (!block || !blocks.emplace(pc,block).second) throw std::runtime_error("Invalid or duplicate translated entry");
}
DispatchResult Dispatcher::Run(GuestCpu& cpu, const BootstrapServices& services, uint64_t limit,
                               const std::function<bool(GuestCpu&)>& additionalServices) const {
    for (uint64_t steps = 0; steps < limit; ++steps) {
        if (cpu.fault) return {DispatchStop::Fault,cpu.rip,steps};
        if (cpu.stop == CpuStop::Fuel || !cpu.fuel) return {DispatchStop::Fuel,cpu.rip,steps};
        if (cpu.stop == CpuStop::Call) {
            if (services.Dispatch(cpu)) continue;
            if (cpu.fault) return {DispatchStop::Fault,cpu.rip,steps};
            if (additionalServices && additionalServices(cpu)) continue;
            if (cpu.fault) return {DispatchStop::Fault,cpu.rip,steps};
            if (cpu.target < cpu.loadBias) { cpu.fault = 5; return {DispatchStop::Fault,cpu.rip,steps}; }
            auto target = cpu.target-cpu.loadBias;
            // Leave the pending call and stack untouched until code exists.
            if (!blocks.contains(target)) return {DispatchStop::MissingCode,target,steps};
            if (cpu.nextRip > UINT64_MAX-cpu.loadBias || !EnterGuestCall(cpu,cpu.loadBias+cpu.nextRip)) {
                if (!cpu.fault) cpu.fault = 5;
                return {DispatchStop::Fault,cpu.rip,steps};
            }
        } else if (cpu.stop == CpuStop::Return) {
            if (!CompleteGuestReturn(cpu)) return {DispatchStop::Fault,cpu.rip,steps};
        } else if (cpu.stop == CpuStop::Branch) {
            cpu.stop = CpuStop::None;
        }
        auto entry = blocks.find(cpu.rip);
        if (entry == blocks.end()) return {DispatchStop::MissingCode,cpu.rip,steps};
        try {
            if (cpu.rip > UINT64_MAX-cpu.loadBias || !cpu.memory || !cpu.memory->Executable(cpu.loadBias+cpu.rip)) {
                cpu.fault = 7; return {DispatchStop::Fault,cpu.rip,steps};
            }
        } catch (...) { cpu.fault = 7; return {DispatchStop::Fault,cpu.rip,steps}; }
        cpu.stop = CpuStop::None;
        cpu.target = 0;
        entry->second(&cpu);
        if (cpu.stop == CpuStop::None && !cpu.fault) cpu.fault = 7;
    }
    if (cpu.fault) return {DispatchStop::Fault,cpu.rip,limit};
    if (cpu.stop == CpuStop::Fuel || !cpu.fuel) return {DispatchStop::Fuel,cpu.rip,limit};
    return {DispatchStop::TransitionLimit,cpu.rip,limit};
}
}
