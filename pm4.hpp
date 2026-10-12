#pragma once
#include <cstdint>
#include <map>
#include <optional>
#include <span>
#include <vector>

namespace NativePS5 {
struct Pm4State {
    std::map<uint32_t,uint32_t> shader,context,userConfig;
    std::optional<uint32_t> instances;
};
struct Pm4Draw {
    uint32_t firstVertex,count,instances,flags;
    Pm4State state;
};
struct Pm4Submission {
    Pm4State finalState;
    std::vector<Pm4Draw> draws;
};
// Decoding has no guest-memory or GPU side effects. Register contents remain
// raw state: a backend must validate their meaning before executing a draw.
Pm4Submission DecodePm4(std::span<const uint32_t> words,const Pm4State& initial={});
}
