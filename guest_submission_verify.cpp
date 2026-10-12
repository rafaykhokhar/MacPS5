#include "guest_submission.hpp"
#include <array>
#include <iostream>
#include <stdexcept>

int main() {
    try {
        using namespace NativePS5;
        std::array<uint32_t,8> commands={0xc0017900,0x24a,2,0xc0002f00,1,0xc0012d00,3,2};
        std::array<uint64_t,2> packet={reinterpret_cast<uintptr_t>(commands.data()),commands.size()};
        GuestMemory memory;
        memory.Add(std::as_writable_bytes(std::span(commands)),false);
        memory.Add(std::as_writable_bytes(std::span(packet)),false);
        const auto descriptor=reinterpret_cast<uintptr_t>(packet.data());
        const auto original=packet;
        auto submission=PrepareGuestDcb(memory,descriptor);
        if (submission.decoded.draws.size()!=1 || submission.decoded.draws[0].firstVertex!=2 || submission.decoded.draws[0].count!=3)
            throw std::runtime_error("Guest DCB draw was decoded incorrectly");
        commands[6]=99;
        if (submission.words[6]!=3 || submission.decoded.draws[0].count!=3) throw std::runtime_error("Submission retained mutable commands");
        commands[6]=3;
        unsigned rejected=0;
        auto reject=[&](uint64_t address) { try { PrepareGuestDcb(memory,address); } catch (const std::exception&) { ++rejected; } };
        reject(0); reject(descriptor+1); reject(UINT64_MAX-7); reject(descriptor+8);
        packet[1]=65537; reject(descriptor);
        packet[1]=original[1]|(uint64_t(1)<<32); reject(descriptor);
        packet=original; packet[0]+=1; reject(descriptor);
        packet=original; packet[0]=0; reject(descriptor);
        packet=original; packet[1]+=1; reject(descriptor);
        packet=original; commands[7]=0; reject(descriptor); commands[7]=2;
        if (rejected!=10) throw std::runtime_error("Invalid guest DCB accepted");
        packet={0,0};
        auto empty=PrepareGuestDcb(memory,descriptor,submission.decoded.finalState);
        if (!empty.words.empty() || !empty.decoded.draws.empty() || empty.decoded.finalState.userConfig.at(0x24a)!=2)
            throw std::runtime_error("Empty DCB changed queue state");
        std::cout<<"PASS: guarded guest DCB descriptor/command reads, owned command snapshots, empty submissions and 10 rejection cases\n";
    } catch (const std::exception& error) { std::cerr<<"FAIL: "<<error.what()<<'\n'; return 1; }
}
