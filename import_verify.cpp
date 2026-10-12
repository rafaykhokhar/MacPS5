#include "import_identity.hpp"
#include "import_registry.hpp"
#include <nid/NidCompute.hpp>
#include <fstream>
#include <iostream>
#include <iterator>

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
}
int main(int argc,char** argv) {
    try {
        if (argc!=2) throw std::runtime_error("Usage: import-verify graphics-call.elf");
        std::ifstream stream(argv[1],std::ios::binary);
        if (!stream) throw std::runtime_error("Cannot read import fixture");
        std::vector<char> chars((std::istreambuf_iterator<char>(stream)),{});
        auto bytes=std::as_bytes(std::span(chars));
        std::vector<std::byte> data(bytes.begin(),bytes.end());
        const auto nid=Nid::ComputeNid("sceAgcDriverSubmitDcb","");
        const std::map<uint16_t,std::string> modules={{1,"libSceAgcDriver"},{65535,"other"}};
        const std::vector<std::string> dependencies={"libSceAgcDriver.prx"};
        const auto identity=IdentifyImport(nid+"#A#B",modules,dependencies);
        Require(identity.symbol==nid && identity.module==dependencies[0] && identity.moduleId==1
                && identity.libraryToken=="A","Qualified identity mismatch");
        Require(IdentifyImport("foo#A#P--",modules,{}).moduleId==65535,"Maximum module ID mismatch");
        ImportRegistry registry;
        registry.Register(dependencies[0],"sceAgcDriverSubmitDcb",0x123400);
        registry.Register("wrong.prx","sceAgcDriverSubmitDcb",0x987600);
        Require(registry.Resolve(identity)==0x123400,"Wrong module export");
        Require(!registry.Resolve(IdentifyImport("sceAgcDriverSubmitDcb",{},{})),"Ambiguous plain import resolved");
        auto wrong=identity; wrong.module="absent.prx";
        Require(!registry.Resolve(wrong),"Qualified import escaped module scope");
        wrong=identity; wrong.symbol="unknown";
        Require(!registry.Resolve(wrong),"Unknown NID resolved");
        ElfImage linked(data);
        linked.RelocateScoped([&](const auto& item) { return registry.Resolve(item); });
        Require(linked.ImportsResolved() && linked.AppliedRelocations()==1 && Slot(linked)==0x123400,
                "Qualified NID relocation failed (Python/C++ hashes or metadata differ)");
        ElfImage legacy(data);
        bool sawQualified=false;
        legacy.Relocate([&](std::string_view name)->std::optional<uint64_t> {
            sawQualified=name==nid+"#A#B"; return {};
        });
        Require(sawQualified && !legacy.ImportsResolved() && !Slot(legacy)
                && legacy.Unresolved()[0].identity.moduleId==1,"Legacy resolver lost scope");
        const std::map<uint16_t,std::string> alias={{1,"foo_native"}};
        const std::vector<std::string> oneAlias={"foo.native.prx"};
        Require(IdentifyImport("foo#A#B",alias,oneAlias).module==oneAlias[0],"Module alias mismatch");
        Require(IdentifyImport("foo#A#B",alias,{}).module=="foo_native.prx","Unmatched owner discarded");
        const std::vector<std::string> exact={"foo_native.prx","foo.native.prx"};
        Require(IdentifyImport("foo#A#B",alias,exact).module==exact[0],"Exact dependency lost precedence");
        size_t rejected=0;
        auto reject=[&](auto operation) {
            try { operation(); } catch (const std::runtime_error&) { ++rejected; return; }
            throw std::logic_error("Invalid import metadata accepted");
        };
        for (const char* symbol:{"foo#A#C","foo#A#","foo#A#?","foo#A#QAA","foo#A#BBBBBBBB",
                                "foo#A","#A#B","foo##B","foo#?#B","foo#A#B#A"})
            reject([&] { IdentifyImport(symbol,modules,dependencies); });
        reject([&] { IdentifyImport("foo#A#B",{},dependencies); });
        const std::vector<std::string> ambiguous={"foo.native.prx","foo.native-module.prx"};
        reject([&] { IdentifyImport("foo#A#B",alias,ambiguous); });
        reject([&] { IdentifyImport("foo#A#B",{{1,"../bad"}},{}); });
        reject([&] { registry.Register(dependencies[0],"sceAgcDriverSubmitDcb",0x999999); });
        Require(registry.Resolve(identity)==0x123400,"Failed registration changed exports");
        // Duplicate DT_SCE_NEEDED_MODULE entries must be collected, then checked by ID.
        auto duplicate=data;
        Put<int64_t>(duplicate,0x210,0x61000045);
        uint64_t record=0; std::memcpy(&record,data.data()+0x208,8);
        Put<uint64_t>(duplicate,0x218,record);
        Put<uint64_t>(duplicate,176+32,176); Put<uint64_t>(duplicate,176+40,176);
        reject([&] { ElfImage image(duplicate); image.RelocateScoped([&](const auto& item) { return registry.Resolve(item); }); });
        // A late invalid relocation cannot leave an earlier valid patch installed.
        auto partial=data;
        Put<uint64_t>(partial,0x198,48); // DT_RELASZ
        Put<uint64_t>(partial,0x498,0x4008);
        Put<uint64_t>(partial,0x4a0,(uint64_t(1)<<32)|37);
        ElfImage atomic(partial);
        reject([&] { atomic.RelocateScoped([&](const auto& item) { return registry.Resolve(item); }); });
        Require(!Slot(atomic) && !atomic.ImportsResolved(),"Failed relocation partially committed");
        // Multiple different module records remain valid, unlike duplicate IDs.
        auto multiple=duplicate;
        Put<uint64_t>(multiple,0x218,(uint64_t(2)<<48)|(record&0xffffffffu));
        ElfImage multi(multiple);
        multi.RelocateScoped([&](const auto& item) { return registry.Resolve(item); });
        Require(Slot(multi)==0x123400,"Multiple module records rejected");
        std::cout<<"PASS: module-scoped NID linking, independent Python/C++ hash fixture, aliases, ambiguity, transactional patches; "
                 <<rejected<<" rejection checks\n";
    } catch (const std::exception& error) { std::cerr<<error.what()<<'\n'; return 1; }
}
