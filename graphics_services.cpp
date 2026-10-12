#include "graphics_services.hpp"
#include <cstdio>
#include <stdexcept>

namespace NativePS5 {
GraphicsServices::GraphicsServices(GuestMemory& memory,Submitter submitter):memory(memory),submit(std::move(submitter)) {
    if (!submit) throw std::invalid_argument("Graphics submission backend is required");
    memory.Add(thunk,false,true);
}
std::optional<uint64_t> GraphicsServices::Resolve(std::string_view name) const {
    if (name=="sceAgcDriverSubmitDcb") return reinterpret_cast<uintptr_t>(thunk.data());
    return {};
}
bool GraphicsServices::Dispatch(GuestCpu& cpu) noexcept {
    if (cpu.fault || cpu.stop!=CpuStop::Call || cpu.target!=reinterpret_cast<uintptr_t>(thunk.data())) return false;
    error[0]=0;
    try {
        if (cpu.memory!=&memory || cpu.nextRip>UINT64_MAX-cpu.loadBias || cpu.registers[4]<8)
            throw std::invalid_argument("Invalid graphics bridge CPU state");
        const auto caller=cpu.loadBias+cpu.nextRip;
        if (!memory.Executable(caller)) throw std::invalid_argument("Invalid graphics bridge return address");
        memory.Store(cpu.registers[4]-8,caller,8);
        cpu.registers[4]-=8;
        auto prepared=PrepareGuestDcb(memory,cpu.registers[7],state);
        // The backend must throw on unsupported state or failed completion.
        submit(prepared);
        state=std::move(prepared.decoded.finalState);
        cpu.registers[0]=0;
        cpu.stop=CpuStop::Return;
        if (!CompleteGuestReturn(cpu)) {
            std::snprintf(error.data(),error.size(),"Graphics completed but guest return validation failed");
            return false;
        }
        return true;
    } catch (const std::exception& failure) {
        std::snprintf(error.data(),error.size(),"%s",failure.what());
    } catch (...) {
        std::snprintf(error.data(),error.size(),"Unknown graphics submission failure");
    }
    cpu.fault=9;
    return false;
}
}
