#include "guest_submission.hpp"
#include <stdexcept>

namespace NativePS5 {
GuestSubmission PrepareGuestDcb(const GuestMemory& memory,uint64_t descriptorAddress,const Pm4State& initial) {
    if (!descriptorAddress || descriptorAddress%8 || descriptorAddress>UINT64_MAX-16)
        throw std::invalid_argument("Invalid guest DCB descriptor address");
    const auto address=memory.Load(descriptorAddress,8);
    const auto tail=memory.Load(descriptorAddress+8,8);
    const auto count=uint32_t(tail);
    if ((tail>>32)&255) throw std::invalid_argument("Nonzero DCB submission flags unsupported");
    if (count>65536) throw std::invalid_argument("Guest DCB exceeds 256 KiB limit");
    if (count && (!address || address%4 || uint64_t(count)*4>UINT64_MAX-address))
        throw std::invalid_argument("Invalid guest command buffer address");
    GuestSubmission result{address,{},{}};
    result.words.reserve(count);
    for (uint32_t i=0;i<count;++i) result.words.push_back(uint32_t(memory.Load(address+uint64_t(i)*4,4)));
    result.decoded=DecodePm4(result.words,initial);
    return result;
}
}
