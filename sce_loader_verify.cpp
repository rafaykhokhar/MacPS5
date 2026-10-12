#include "elf_image.hpp"
#include <fstream>
#include <iostream>
#include <iterator>
#include <stdexcept>

using namespace NativePS5;
namespace {
void Require(bool value,const char* message) { if (!value) throw std::runtime_error(message); }
template<class T> void Put(std::vector<std::byte>& data,size_t offset,T value) {
    if (offset>data.size() || sizeof(T)>data.size()-offset) throw std::logic_error("Fixture bounds");
    std::memcpy(data.data()+offset,&value,sizeof(T));
}
uint64_t Slot(const ElfImage& image) {
    uint64_t value=0; std::memcpy(&value,image.Read(0x4000,8).data(),8); return value;
}
auto Resolve(const ImportIdentity& identity)->std::optional<uint64_t> {
    if (identity.module=="libSceAgcDriver.prx" && identity.moduleId==1) return 0x123456;
    return {};
}
}
int main(int argc,char** argv) {
    try {
        if (argc!=2) throw std::runtime_error("Usage: sce-loader-verify sce-graphics-call.elf");
        std::ifstream stream(argv[1],std::ios::binary);
        if (!stream) throw std::runtime_error("Cannot read SCE fixture");
        std::vector<char> chars((std::istreambuf_iterator<char>(stream)),{});
        auto bytes=std::as_bytes(std::span(chars));
        std::vector<std::byte> data(bytes.begin(),bytes.end());
        ElfImage linked(data);
        linked.RelocateScoped(Resolve);
        Require(linked.ImportsResolved() && linked.AppliedRelocations()==1 && Slot(linked)==0x123456,
                "SCE-relative PLT relocation failed");
        Require(linked.Dependencies()==std::vector<std::string>{"libSceAgcDriver.prx"},"Wrong SCE strings");
        size_t rejected=0;
        auto reject=[&](auto operation) {
            try { operation(); } catch (const std::runtime_error&) { ++rejected; return; }
            throw std::logic_error("Malformed SCE metadata accepted");
        };
        reject([&] { linked.Read(0x1800,1); }); // Metadata is not a load segment.
        auto invalid=[&](std::vector<std::byte> copy) {
            reject([&] { ElfImage bad(copy); bad.RelocateScoped(Resolve); });
        };
        auto corrupt=[&](size_t offset,uint64_t value) { auto copy=data; Put(copy,offset,value); invalid(copy); };
        constexpr size_t dynlib=0x500+3*56;
        corrupt(dynlib,0); // Missing PT_SCE_DYNLIBDATA.
        corrupt(dynlib+8,UINT64_MAX); // File bounds.
        corrupt(dynlib+32,0x110); // Relocation table crosses segment boundary, but stays inside file.
        corrupt(0x188,0x1f0); // PLT offset crosses the declared metadata segment.
        corrupt(0x188,UINT64_MAX); // Offset arithmetic.
        corrupt(0x198,25); // Bad PLT byte count.
        corrupt(0x1a8,17); // DT_REL unsupported for PLT.
        corrupt(0x1b8,0x1e0); // Symbol table crosses metadata boundary.
        corrupt(0x1c8,16); // Symbol record size.
        corrupt(0x1d8,0); // Missing/zero symbol table bound.
        corrupt(0x1d8,25); // Nonintegral symbol count.
        corrupt(0x1d8,0x1000); // Whole symbol table must fit metadata segment.
        corrupt(0x1908,(uint64_t(2)<<32)|7); // Index is inside file but beyond declared symbols.
        corrupt(0x1e8,UINT64_MAX); // String table overflow.
        corrupt(0x1f8,0x300); // Oversized string table.
        corrupt(0x208,0x1000); // DT_NEEDED string outside table.
        corrupt(0x500+2*56+32,175); // Partial dynamic record.
        // Standard/SCE aliases cannot coexist, even with equal or zero values.
        for (const auto& pair:{std::pair<int64_t,uint64_t>{5,0x40},{6,0},{23,0x100},{2,24},{20,7},{10,0},{11,24}}) {
            auto copy=data;
            Put(copy,0x220,pair.first); Put(copy,0x228,pair.second);
            Put<uint64_t>(copy,0x500+2*56+32,192);
            Put<uint64_t>(copy,0x500+2*56+40,192);
            invalid(copy);
        }
        // All metadata is selected per tag: standard VA strings + SCE-relative symbols/PLT.
        auto mixed=data;
        uint64_t strSize=0; std::memcpy(&strSize,data.data()+0x1f8,8);
        std::memcpy(mixed.data()+0x380,data.data()+0x1840,strSize);
        Put<int64_t>(mixed,0x1e0,5); Put<uint64_t>(mixed,0x1e8,0x380);
        Put<int64_t>(mixed,0x1f0,10);
        ElfImage mix(mixed); mix.RelocateScoped(Resolve);
        Require(Slot(mix)==0x123456,"Mixed standard/SCE table selection failed");
        // Test the non-PLT SCE RELA family independently.
        auto rela=data;
        Put<int64_t>(rela,0x180,0x6100002f); Put<int64_t>(rela,0x190,0x61000031);
        Put<int64_t>(rela,0x1a0,0x61000033); Put<uint64_t>(rela,0x1a8,24);
        ElfImage regular(rela); regular.RelocateScoped(Resolve);
        Require(Slot(regular)==0x123456,"SCE RELA family failed");
        ElfImage missing(data);
        missing.RelocateScoped([](const auto&)->std::optional<uint64_t> { return {}; });
        Require(!missing.ImportsResolved() && missing.Unresolved().size()==1 && !Slot(missing),"SCE missing import patched");
        std::cout<<"PASS: unmapped SCE dynamic data, bounded symbols/strings, SCE PLT/RELA, mixed tables; "
                 <<rejected<<" rejection checks\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
