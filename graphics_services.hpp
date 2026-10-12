#pragma once
#include "guest_submission.hpp"
#include <functional>
#include <optional>
#include <string_view>

namespace NativePS5 {
class GraphicsServices {
public:
    using Submitter=std::function<void(const GuestSubmission&)>;
    GraphicsServices(GuestMemory& memory,Submitter submitter);
    GraphicsServices(const GraphicsServices&)=delete;
    GraphicsServices& operator=(const GraphicsServices&)=delete;
    std::optional<uint64_t> Resolve(std::string_view name) const;
    bool Dispatch(GuestCpu& cpu) noexcept;
    std::string_view LastError() const { return error.data(); }
    const Pm4State& QueueState() const { return state; }
private:
    GuestMemory& memory;
    Submitter submit;
    std::array<std::byte,2> thunk{std::byte(0x0f),std::byte(0x0b)};
    std::array<char,512> error{};
    Pm4State state;
};
}
