#include "guest_cpu.hpp"
#include <algorithm>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <tuple>

using namespace NativePS5;
extern "C" void vector_load128(GuestCpu*);
extern "C" void vector_store128(GuestCpu*);
extern "C" void vector_aligned_load128(GuestCpu*);
extern "C" void vector_aligned_store128(GuestCpu*);
extern "C" void vector_load256(GuestCpu*);
extern "C" void vector_store256(GuestCpu*);
extern "C" void vector_aligned_load256(GuestCpu*);
extern "C" void vector_aligned_store256(GuestCpu*);
extern "C" void vector_reg128(GuestCpu*);
extern "C" void vector_regstore128(GuestCpu*);
extern "C" void vector_reg256(GuestCpu*);
extern "C" void vector_self128(GuestCpu*);
extern "C" void vector_sib256(GuestCpu*);
extern "C" void vector_rip128(GuestCpu*);
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
using Block=void(*)(GuestCpu*);
GuestCpu Seed(GuestMemory& memory,uint64_t address) {
    GuestCpu cpu; cpu.memory=&memory; cpu.fuel=10; cpu.flags=0x8d7;
    for (size_t r=0;r<16;++r) {
        cpu.registers[r]=0x1234567890+r;
        for (size_t b=0;b<32;++b) cpu.vectors[r][b]=std::byte((r*13+b*7+1)&255);
    }
    cpu.registers[0]=address;
    return cpu;
}
void CheckStop(const GuestCpu& cpu,const GuestCpu& before) {
    Require(!cpu.fault && cpu.stop==CpuStop::Return,"Vector instruction failed");
    Require(cpu.flags==before.flags && cpu.registers==before.registers,"Vector move changed GPRs or flags");
}
}
int main() {
    try {
        alignas(32) std::array<std::byte,128> storage{};
        for (size_t i=0;i<storage.size();++i) storage[i]=std::byte((i*29+3)&255);
        const auto original=storage;
        const auto address=reinterpret_cast<uintptr_t>(storage.data());
        GuestMemory memory; memory.Add(storage,true);
        unsigned checks=0;
        for (unsigned size:{16u,32u}) for (unsigned offset=0;offset<32;++offset) {
            storage=original;
            auto cpu=Seed(memory,address+offset); const auto before=cpu;
            (size==16?vector_load128:vector_load256)(&cpu);
            CheckStop(cpu,before);
            auto expected=before.vectors;
            std::copy_n(storage.begin()+offset,size,expected[0].begin());
            if (size==16) std::fill(expected[0].begin()+16,expected[0].end(),std::byte(0));
            Require(cpu.vectors==expected,"Unaligned vector load or upper clearing mismatch");
            cpu=Seed(memory,address+offset); const auto storeBefore=cpu;
            (size==16?vector_store128:vector_store256)(&cpu);
            CheckStop(cpu,storeBefore);
            auto expectedMemory=original;
            std::copy_n(storeBefore.vectors[0].begin(),size,expectedMemory.begin()+offset);
            Require(storage==expectedMemory && cpu.vectors==storeBefore.vectors,"Vector store changed wrong bytes/registers");
            checks+=2;
        }
        for (auto [block,dst,src,size]:{
                std::tuple<Block,unsigned,unsigned,unsigned>{vector_reg128,8,1,16},
                {vector_regstore128,1,8,16},{vector_reg256,8,9,32},{vector_self128,0,0,16}}) {
            auto cpu=Seed(memory,address); const auto before=cpu;
            block(&cpu); CheckStop(cpu,before);
            auto expected=before.vectors;
            std::copy_n(before.vectors[src].begin(),size,expected[dst].begin());
            if (size==16) std::fill(expected[dst].begin()+16,expected[dst].end(),std::byte(0));
            Require(cpu.vectors==expected,"Native register vector copy mismatch"); ++checks;
        }
        storage=original;
        auto sib=Seed(memory,address); sib.registers[10]=address; sib.registers[1]=2;
        const auto sibBefore=sib; vector_sib256(&sib); CheckStop(sib,sibBefore);
        Require(!std::memcmp(sib.vectors[15].data(),storage.data()+24,32),"VEX SIB/high register mismatch");
        auto rip=Seed(memory,address); rip.loadBias=address;
        const auto ripBefore=rip; vector_rip128(&rip); CheckStop(rip,ripBefore);
        Require(!std::memcmp(rip.vectors[0].data(),storage.data()+24,16),"VEX RIP address mismatch");
        checks+=2;
        for (auto [load,store,size]:{
                std::tuple<Block,Block,unsigned>{vector_aligned_load128,vector_aligned_store128,16},
                {vector_aligned_load256,vector_aligned_store256,32}}) {
            auto good=Seed(memory,address); const auto before=good;
            load(&good); CheckStop(good,before);
            good=Seed(memory,address); const auto storeBefore=good;
            store(&good); CheckStop(good,storeBefore);
            for (unsigned offset=1;offset<size;++offset) {
                const auto snapshot=storage;
                auto bad=Seed(memory,address+offset); const auto previous=bad;
                load(&bad);
                Require(bad.fault==1 && bad.vectors==previous.vectors && bad.flags==previous.flags,"Misaligned vector load changed state");
                bad=previous; store(&bad);
                Require(bad.fault==2 && storage==snapshot && bad.vectors==previous.vectors,"Misaligned vector store changed state");
                checks+=2;
            }
        }
        for (unsigned size:{16u,32u}) for (unsigned available=1;available<size;++available) {
            const auto snapshot=storage;
            auto cpu=Seed(memory,address+storage.size()-available); const auto before=cpu;
            (size==16?vector_load128:vector_load256)(&cpu);
            Require(cpu.fault==1 && cpu.vectors==before.vectors,"Faulting wide load partially updated register");
            cpu=before; (size==16?vector_store128:vector_store256)(&cpu);
            Require(cpu.fault==2 && storage==snapshot,"Faulting wide store partially committed");
            checks+=2;
        }
        for (bool executable:{false,true}) {
            GuestMemory guarded; guarded.Add(storage,executable,executable);
            auto cpu=Seed(guarded,address); const auto snapshot=storage;
            vector_store256(&cpu);
            Require(cpu.fault==2 && storage==snapshot,"Read-only or executable vector write permitted"); ++checks;
        }
        auto scalar=Seed(memory,address); guest_load(&scalar,address,16);
        Require(scalar.fault==1,"Wide scalar load accepted");
        scalar.fault=0; const auto snapshot=storage; guest_store(&scalar,address,1,32);
        Require(scalar.fault==2 && storage==snapshot,"Wide scalar store accepted");
        std::cout<<"PASS: native VEX 128/256-bit moves, NEON register copies, alignment, upper clearing, guarded wide memory; "<<checks<<" checks\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
