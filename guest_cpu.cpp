#include "guest_cpu.hpp"
#include <bit>
#include <cstring>
#include <stdexcept>

namespace NativePS5 {
void GuestMemory::Add(std::span<std::byte> bytes, bool writable, bool executable) {
    AddAt(reinterpret_cast<uintptr_t>(bytes.data()),bytes,writable,executable);
}
void GuestMemory::AddAt(uint64_t address, std::span<std::byte> bytes, bool writable, bool executable) {
    if (bytes.empty()) throw std::runtime_error("Empty guest memory region");
    const auto backing=reinterpret_cast<uintptr_t>(bytes.data());
    if (!backing || bytes.size()>UINT64_MAX-address || bytes.size()>UINTPTR_MAX-backing)
        throw std::runtime_error("Guest memory overflow or invalid backing");
    for (const auto& old : regions) {
        if (address < old.address + old.bytes.size() && old.address < address + bytes.size())
            throw std::runtime_error("Overlapping guest memory regions");
        // Until shared mappings have coordinated permissions/invalidation,
        // prohibit aliases that could write immutable or translated code bytes.
        const auto prior=reinterpret_cast<uintptr_t>(old.bytes.data());
        if (backing<prior+old.bytes.size() && prior<backing+bytes.size())
            throw std::runtime_error("Aliased guest backing storage");
    }
    regions.push_back({address, bytes, writable, executable});
}
bool GuestMemory::RemoveData(uint64_t address,std::span<std::byte> bytes) noexcept {
    for(auto it=regions.begin();it!=regions.end();++it)
        if(it->address==address && it->bytes.data()==bytes.data() && it->bytes.size()==bytes.size() && !it->executable) {
            regions.erase(it);return true;
        }
    return false;
}
const MemoryRegion& GuestMemory::Find(uint64_t address, uint64_t size) const {
    if (!size) throw std::runtime_error("Invalid guest access size");
    for (const auto& region : regions) {
        if (address >= region.address && address - region.address < region.bytes.size()
            && size <= region.bytes.size() - (address - region.address)) return region;
    }
    throw std::runtime_error("Guest memory access outside mapped regions");
}
uint64_t GuestMemory::Load(uint64_t address, uint64_t size) const {
    if (size!=1 && size!=2 && size!=4 && size!=8) throw std::runtime_error("Invalid scalar load size");
    const auto& region = Find(address, size);
    uint64_t value = 0;
    std::memcpy(&value, region.bytes.data() + address - region.address, size);
    return value;
}
void GuestMemory::Store(uint64_t address, uint64_t value, uint64_t size) {
    if (size!=1 && size!=2 && size!=4 && size!=8) throw std::runtime_error("Invalid scalar store size");
    const auto& region = Find(address, size);
    if (!region.writable || region.executable) throw std::runtime_error("Guest write permission or code-invalidation fault");
    std::memcpy(region.bytes.data() + address - region.address, &value, size);
}
void GuestMemory::ReadVector(uint64_t address,std::span<std::byte> output) const {
    if (output.size()!=16 && output.size()!=32) throw std::runtime_error("Invalid vector load size");
    const auto& region=Find(address,output.size());
    std::memcpy(output.data(),region.bytes.data()+address-region.address,output.size());
}
void GuestMemory::WriteVector(uint64_t address,std::span<const std::byte> input) {
    if (input.size()!=16 && input.size()!=32) throw std::runtime_error("Invalid vector store size");
    const auto& region=Find(address,input.size());
    if (!region.writable || region.executable) throw std::runtime_error("Guest vector write permission or code-invalidation fault");
    std::memcpy(region.bytes.data()+address-region.address,input.data(),input.size());
}
void GuestMemory::ValidateAccess(uint64_t address,uint64_t size,bool writable) const {
    if(!size) return;
    const auto& region=Find(address,size);
    if(writable && (!region.writable || region.executable)) throw std::runtime_error("Guest write permission fault");
}
void GuestMemory::ReadBytes(uint64_t address,std::span<std::byte> output) const {
    if(output.empty()) return;
    const auto& region=Find(address,output.size());
    std::memmove(output.data(),region.bytes.data()+address-region.address,output.size());
}
void GuestMemory::WriteBytes(uint64_t address,std::span<const std::byte> input) {
    if(input.empty()) return;
    const auto& region=Find(address,input.size());
    if(!region.writable || region.executable) throw std::runtime_error("Guest byte write permission fault");
    std::memmove(region.bytes.data()+address-region.address,input.data(),input.size());
}
bool GuestMemory::Executable(uint64_t address) const { return Find(address, 1).executable; }
bool EnterGuestCall(GuestCpu& cpu, uint64_t returnAddress) noexcept {
    if (cpu.fault || cpu.stop != CpuStop::Call || !cpu.memory) return false;
    try {
        if (!cpu.memory->Executable(cpu.target) || !cpu.memory->Executable(returnAddress) || cpu.target < cpu.loadBias)
            throw std::runtime_error("Invalid guest call target or return address");
        if (cpu.registers[4] < 8) throw std::runtime_error("Guest stack underflow");
        const auto nextStack = cpu.registers[4] - 8;
        cpu.memory->Store(nextStack, returnAddress, 8);
        cpu.registers[4] = nextStack;
        cpu.rip = cpu.target - cpu.loadBias;
        cpu.target = 0;
        cpu.stop = CpuStop::None;
        return true;
    } catch (...) { cpu.fault = 5; return false; }
}
bool CompleteGuestReturn(GuestCpu& cpu) noexcept {
    if (cpu.fault || cpu.stop != CpuStop::Return || !cpu.memory) return false;
    try {
        const auto target = cpu.memory->Load(cpu.registers[4], 8);
        if (target < cpu.loadBias || !cpu.memory->Executable(target)) throw std::runtime_error("Invalid return target");
        cpu.registers[4] += 8;
        cpu.rip = target - cpu.loadBias;
        cpu.target = 0;
        cpu.stop = CpuStop::None;
        return true;
    } catch (...) { cpu.fault = 6; return false; }
}
}

extern "C" uint64_t guest_load(NativePS5::GuestCpu* cpu, uint64_t address, uint64_t size) noexcept {
    try {
        if (!cpu->memory) throw std::runtime_error("Missing memory");
        return cpu->memory->Load(address, size);
    } catch (...) { cpu->fault = 1; return 0; }
}
extern "C" void guest_store(NativePS5::GuestCpu* cpu, uint64_t address, uint64_t value, uint64_t size) noexcept {
    try {
        if (!cpu->memory) throw std::runtime_error("Missing memory");
        cpu->memory->Store(address, value, size);
    } catch (...) { cpu->fault = 2; }
}
extern "C" void guest_vector_load(NativePS5::GuestCpu* cpu,uint64_t address,uint64_t reg,uint64_t size,uint64_t aligned) noexcept {
    if (cpu->fault) return;
    try {
        if (!cpu->memory || reg>=16 || (size!=16 && size!=32) || aligned>1 || (aligned && address%size))
            throw std::runtime_error("Invalid vector load");
        std::array<std::byte,32> value{};
        cpu->memory->ReadVector(address,std::span(value).first(size));
        // VEX.128 clears the upper YMM half; failures leave the whole register intact.
        cpu->vectors[reg]=value;
    } catch (...) { cpu->fault=1; }
}
extern "C" void guest_vector_store(NativePS5::GuestCpu* cpu,uint64_t address,uint64_t reg,uint64_t size,uint64_t aligned) noexcept {
    if (cpu->fault) return;
    try {
        if (!cpu->memory || reg>=16 || (size!=16 && size!=32) || aligned>1 || (aligned && address%size))
            throw std::runtime_error("Invalid vector store");
        // Validate the complete access before writing any byte.
        cpu->memory->WriteVector(address,std::span(cpu->vectors[reg]).first(size));
    } catch (...) { cpu->fault=2; }
}
extern "C" uint64_t guest_flags(NativePS5::GuestCpu* cpu, uint64_t a, uint64_t b, uint64_t result, uint64_t bits, uint64_t operation) noexcept {
    if ((bits != 8 && bits != 16 && bits != 32 && bits != 64) || operation > 3) { cpu->fault = 3; return cpu->flags; }
    const uint64_t mask = bits == 64 ? UINT64_MAX : (uint64_t(1) << bits) - 1;
    const uint64_t sign = uint64_t(1) << (bits - 1);
    a &= mask; b &= mask; result &= mask;
    uint64_t flags = (cpu->flags & ~uint64_t(0x8d5)) | 2;
    if (result == 0) flags |= 0x40;
    if (result & sign) flags |= 0x80;
    if ((std::popcount(static_cast<unsigned>(result & 0xff)) & 1) == 0) flags |= 4;
    if (operation == 2) return flags;
    if ((a ^ b ^ result) & 0x10) flags |= 0x10;
    bool overflow = operation == 0 ? ((a ^ b) & (a ^ result) & sign) != 0
                                  : ((~(a ^ b)) & (a ^ result) & sign) != 0;
    if (overflow) flags |= 0x800;
    if (operation == 3) flags |= cpu->flags & 1;
    else if (operation == 0 ? a < b : result < a) flags |= 1;
    return flags;
}
extern "C" uint64_t guest_condition(NativePS5::GuestCpu* cpu, uint64_t condition) noexcept {
    bool cf = cpu->flags & 1, pf = cpu->flags & 4, zf = cpu->flags & 0x40;
    bool sf = cpu->flags & 0x80, of = cpu->flags & 0x800;
    switch (condition) {
        case 0: return of; case 1: return !of;
        case 2: return cf; case 3: return !cf;
        case 4: return zf; case 5: return !zf;
        case 6: return cf || zf; case 7: return !cf && !zf;
        case 8: return sf; case 9: return !sf;
        case 10: return pf; case 11: return !pf;
        case 12: return sf != of; case 13: return sf == of;
        case 14: return zf || sf != of; case 15: return !zf && sf == of;
        default: cpu->fault = 4; return 0;
    }
}

extern "C" uint64_t guest_shift_flags(NativePS5::GuestCpu* cpu,uint64_t original,uint64_t count,uint64_t result,uint64_t bits,uint64_t operation) noexcept {
    // Operations: SHL, SHR, SAR, ROL, ROR. Preserve architecturally undefined
    // AF/OF values rather than claiming a particular CPU's undefined result.
    if ((bits!=32 && bits!=64) || operation>4) { cpu->fault=3; return cpu->flags; }
    count&=bits-1;
    if (!count) return cpu->flags;
    const auto mask=bits==64?UINT64_MAX:uint64_t(UINT32_MAX);
    original&=mask; result&=mask;
    bool carry;
    if (operation==0) carry=(original>>(bits-count))&1;
    else if (operation<3) carry=(original>>(count-1))&1;
    else if (operation==3) carry=result&1;
    else carry=(result>>(bits-1))&1;
    uint64_t flags=(cpu->flags&~uint64_t(1))|uint64_t(carry);
    if (operation<3) {
        flags&=~uint64_t(0xc4);
        if (!result) flags|=0x40;
        if ((result>>(bits-1))&1) flags|=0x80;
        if (!(std::popcount(static_cast<unsigned>(result&255))&1)) flags|=4;
    }
    if (count==1) {
        bool overflow=false;
        if (operation==0 || operation==3) overflow=bool((result>>(bits-1))&1)!=carry;
        else if (operation==1) overflow=(original>>(bits-1))&1;
        else if (operation==4) overflow=((result>>(bits-1))^(result>>(bits-2)))&1;
        flags=(flags&~uint64_t(0x800))|(overflow?0x800:0);
    }
    return flags;
}

extern "C" void guest_vector_xor(NativePS5::GuestCpu* cpu,uint64_t address,uint64_t destination,uint64_t first,uint64_t size) noexcept {
    if(cpu->fault) return;
    try {
        if(!cpu->memory || destination>=16 || first>=16 || (size!=16 && size!=32))
            throw std::runtime_error("Invalid vector XOR operand");
        std::array<std::byte,32> result{};
        cpu->memory->ReadVector(address,std::span(result).first(size));
        for(size_t i=0;i<size;++i) result[i]^=cpu->vectors[first][i];
        cpu->vectors[destination]=result;
    }catch(...){cpu->fault=1;}
}
