#pragma once
#include "bootstrap_services.hpp"
#include <map>
#include <functional>

namespace NativePS5 {
enum class DispatchStop { MissingCode, Fault, Fuel, TransitionLimit };
struct DispatchResult {
    DispatchStop reason;
    uint64_t pc;
    uint64_t transitions;
};

// Code entries must be instruction boundaries validated by the AOT decoder.
// All functions operate on GuestCpu; no guest pointer is invoked as host code.
class Dispatcher {
public:
    using Block = void (*)(GuestCpu*);
    void Add(uint64_t pc, Block block);
    DispatchResult Run(GuestCpu& cpu, const BootstrapServices& services, uint64_t transitionLimit,
                       const std::function<bool(GuestCpu&)>& additionalServices={}) const;
private:
    std::map<uint64_t, Block> blocks;
};
}
