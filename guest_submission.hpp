#pragma once
#include "guest_cpu.hpp"
#include "pm4.hpp"

namespace NativePS5 {
struct GuestSubmission {
    uint64_t commandAddress;
    std::vector<uint32_t> words;
    Pm4Submission decoded;
};
// Reads the 16-byte Packet descriptor used by sceAgcDriverSubmitDcb.
// Preparation is not submission completion and performs no GPU work.
GuestSubmission PrepareGuestDcb(const GuestMemory& memory,uint64_t descriptorAddress,const Pm4State& initial={});
}
