#include "elf_image.hpp"
#include "import_identity.hpp"
#include <algorithm>
#include <bit>
#include <limits>
#include <map>
#include <set>
#include <stdexcept>
#include <sys/mman.h>
#include <unistd.h>
#ifdef NATIVE_PS5_RUST_LOADER
#include "rust/loader_runtime_abi.h"
#include <memory>
#endif

namespace NativePS5 {
namespace {
#ifdef NATIVE_PS5_RUST_LOADER
ImportIdentity CopyRustIdentity(const RustLoaderIdentity& identity) {
    auto text=[](RustLoaderBytes bytes){return std::string(reinterpret_cast<const char*>(bytes.data),bytes.size);};
    return {text(identity.name),text(identity.symbol),text(identity.module),text(identity.libraryToken),
        identity.moduleId<65536?std::optional<uint16_t>(identity.moduleId):std::nullopt,
        text(identity.library),identity.libraryVersion<65536?std::optional<uint16_t>(identity.libraryVersion):std::nullopt,
        identity.moduleVersion<65536?std::optional<uint16_t>(identity.moduleVersion):std::nullopt};
}
#endif
struct Header {
    uint8_t ident[16];
    uint16_t type, machine;
    uint32_t version;
    uint64_t entry, programOffset, sectionOffset;
    uint32_t flags;
    uint16_t headerSize, programSize, programCount, sectionSize, sectionCount, sectionNames;
};
struct Program {
    uint32_t type, flags;
    uint64_t offset, address, physical, fileSize, memorySize, alignment;
};
struct Dynamic { int64_t tag; uint64_t value; };
struct Relocation { uint64_t address, info; int64_t addend; };
struct Symbol {
    uint32_t name;
    uint8_t info, other;
    uint16_t section;
    uint64_t value, size;
};
static_assert(sizeof(Header) == 64 && sizeof(Program) == 56);
static_assert(sizeof(Relocation) == 24 && sizeof(Symbol) == 24);
static_assert(std::endian::native == std::endian::little);

uint64_t Add(uint64_t a, uint64_t b) {
    if (b > std::numeric_limits<uint64_t>::max() - a) throw std::runtime_error("ELF address overflow");
    return a + b;
}

template<class T> T Decode(std::span<const std::byte> bytes) {
    if (bytes.size() != sizeof(T)) throw std::runtime_error("Incorrect ELF record size");
    T value;
    std::memcpy(&value, bytes.data(), sizeof(value));
    return value;
}
}

std::span<const std::byte> ElfImage::FileBytes(uint64_t offset, uint64_t size) const {
    if (offset > file.size() || size > file.size() - offset) throw std::runtime_error("ELF file range out of bounds");
    return std::span(file).subspan(offset, size);
}

std::span<const std::byte> ElfImage::GuestFileBytes(uint64_t address, uint64_t size) const {
    for (const auto& segment : segments) {
        if (address >= segment.address && address - segment.address <= segment.fileSize && size <= segment.fileSize - (address - segment.address))
            return FileBytes(segment.fileOffset + address - segment.address, size);
    }
    throw std::runtime_error("ELF metadata outside file-backed load segment");
}

std::byte* ElfImage::Address(uint64_t address, uint64_t size) const {
    for (const auto& segment : segments) {
        if (address >= segment.address && address - segment.address <= segment.memorySize && size <= segment.memorySize - (address - segment.address))
            return static_cast<std::byte*>(mapping) + address - minimumAddress;
    }
    throw std::runtime_error("Guest address outside mapped segment");
}

ElfImage::ElfImage(std::span<const std::byte> source, size_t budget) {
    loaderBudget = budget;
    if (source.size() > budget) throw std::runtime_error("ELF file exceeds loader budget");
    file.assign(source.begin(), source.end());
    const auto header = Decode<Header>(FileBytes(0, sizeof(Header)));
    const uint8_t prefix[] = {0x7f, 'E', 'L', 'F', 2, 1, 1};
    if (!std::equal(std::begin(prefix), std::end(prefix), header.ident) || header.machine != 62 || header.version != 1 || header.type != 3)
        throw std::runtime_error("Loader requires little-endian x86-64 ET_DYN ELF");
    if (header.headerSize != sizeof(Header) || header.programSize != sizeof(Program) || !header.programCount || header.programOffset < sizeof(Header) || header.flags)
        throw std::runtime_error("Unsupported ELF header layout");
    FileBytes(header.programOffset, uint64_t(header.programCount) * sizeof(Program));
    uint64_t maximumAddress = 0;
    minimumAddress = UINT64_MAX;
    bool hasDynamic = false;
    const uint64_t page = static_cast<uint64_t>(getpagesize());
    for (uint32_t i = 0; i < header.programCount; ++i) {
        const auto program = Decode<Program>(FileBytes(header.programOffset + i * sizeof(Program), sizeof(Program)));
        FileBytes(program.offset, program.fileSize);
        if (program.type == 3 || program.type == 7) throw std::runtime_error("ELF interpreter/TLS support is not implemented");
        if (program.type == 2) {
            if (hasDynamic || !program.fileSize || program.fileSize % sizeof(Dynamic))
                throw std::runtime_error("Duplicate or invalid dynamic segment");
            hasDynamic = true;
            dynamicOffset = program.offset;
            dynamicSize = program.fileSize;
        }
        if (program.type == 0x61000000) {
            if (hasDynlib) throw std::runtime_error("Multiple SCE dynamic data segments");
            hasDynlib = true;
            dynlibOffset = program.offset;
            dynlibSize = program.fileSize;
        }
        if (program.type != 1) continue;
        if (program.fileSize > program.memorySize || (program.flags & ~7u)) throw std::runtime_error("Invalid load segment");
        if (!program.memorySize) continue;
        if (program.alignment > 1 && ((program.alignment & (program.alignment - 1)) || program.address % program.alignment != program.offset % program.alignment))
            throw std::runtime_error("Invalid ELF segment alignment");
        if (program.alignment > page) throw std::runtime_error("ELF segment alignment exceeds native mapping alignment");
        const auto end = Add(program.address, program.memorySize);
        for (const auto& old : segments) {
            if (program.address < old.address + old.memorySize && old.address < end) throw std::runtime_error("Overlapping ELF segments");
        }
        segments.push_back({program.address, program.offset, program.fileSize, program.memorySize, program.flags});
        minimumAddress = std::min(minimumAddress, program.address & ~(page - 1));
        maximumAddress = std::max(maximumAddress, Add(end, page - 1) & ~(page - 1));
    }
    if (segments.empty() || maximumAddress - minimumAddress > budget - file.size()) throw std::runtime_error("ELF mapping exceeds loader budget");
    const bool validEntry = std::any_of(segments.begin(), segments.end(), [&](const auto& segment) {
        return (segment.flags & 1) && header.entry >= segment.address && header.entry - segment.address < segment.fileSize;
    });
    if (!validEntry) throw std::runtime_error("ELF entry is not file-backed executable code");
    mappedBytes = maximumAddress - minimumAddress;
    mapping = mmap(nullptr, mappedBytes, PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (mapping == MAP_FAILED) { mapping = nullptr; throw std::runtime_error("Cannot reserve ELF memory"); }
    try {
        if (reinterpret_cast<uintptr_t>(mapping) < minimumAddress) throw std::runtime_error("Unsupported guest address range");
        for (const auto& segment : segments) {
            const auto low = segment.address & ~(page - 1);
            const auto high = Add(segment.address + segment.memorySize, page - 1) & ~(page - 1);
            if (mprotect(static_cast<std::byte*>(mapping) + low - minimumAddress, high - low, PROT_READ | PROT_WRITE))
                throw std::runtime_error("Cannot map ELF segment");
            std::memcpy(Address(segment.address, segment.fileSize), FileBytes(segment.fileOffset, segment.fileSize).data(), segment.fileSize);
        }
        entry = header.entry;
    } catch (...) {
        munmap(mapping, mappedBytes);
        mapping = nullptr;
        throw;
    }
}

ElfImage::~ElfImage() { if (mapping) munmap(mapping, mappedBytes); }

void ElfImage::Relocate(const Resolver& resolver) {
    // Legacy resolvers receive the full name. Never silently discard scope.
    RelocateScoped([&](const ImportIdentity& identity) { return resolver(identity.name); });
}

void ElfImage::RelocateScoped(const ScopedResolver& resolver) {
    if (relocated) throw std::runtime_error("Image relocation may only be attempted once");
#ifdef NATIVE_PS5_RUST_LOADER
    auto callback=[](void* context,RustLoaderIdentity record) noexcept -> RustLoaderResolution {
        try {
            const auto identity=CopyRustIdentity(record);
            const auto value=(*static_cast<const ScopedResolver*>(context))(identity);
            return {value.value_or(0),value?0u:1u};
        } catch(...) {return {0,2};}
    };
    const std::unique_ptr<void,decltype(&native_ps5_image_delete_v3)> image(
        native_ps5_image_build_v3(reinterpret_cast<const unsigned char*>(file.data()),file.size(),LoadBias(),loaderBudget,
            const_cast<ScopedResolver*>(&resolver),callback),native_ps5_image_delete_v3);
    if(!image)throw std::runtime_error("Rust ELF relocation or resolver callback failed");
    std::vector<std::pair<std::byte*,uint64_t>> patches;
    std::vector<PendingImport> pending;
    std::vector<std::string> plannedDependencies;
    for(uint64_t i=0;i<native_ps5_image_count_v3(image.get(),0);++i){
        const auto patch=native_ps5_image_patch_v3(image.get(),i);
        if(patch.address<LoadBias())throw std::runtime_error("Rust relocation address below image bias");
        patches.emplace_back(Address(patch.address-LoadBias(),8),patch.value);
    }
    for(uint64_t i=0;i<native_ps5_image_count_v3(image.get(),1);++i){
        const auto item=native_ps5_image_import_v3(image.get(),i);
        const auto identity=CopyRustIdentity(item.identity);
        if(item.address<LoadBias())throw std::runtime_error("Rust import address below image bias");
        pending.push_back({identity.name,item.address-LoadBias(),static_cast<uint32_t>(item.kind),identity});
    }
    for(uint64_t i=0;i<native_ps5_image_count_v3(image.get(),2);++i){
        const auto name=native_ps5_image_dependency_v3(image.get(),i);
        plannedDependencies.emplace_back(reinterpret_cast<const char*>(name.data),name.size);
    }
    // All allocations and destination checks finish before any mapped guest write.
    for(const auto& [address,value]:patches)std::memcpy(address,&value,8);
    applied=patches.size();unresolved=std::move(pending);dependencies=std::move(plannedDependencies);relocated=true;
#else
    std::map<int64_t, uint64_t> tags;
    std::vector<uint64_t> needed;
    std::vector<uint64_t> moduleRecords;
    bool terminated = dynamicSize == 0;
    for (uint64_t offset = 0; offset + sizeof(Dynamic) <= dynamicSize; offset += sizeof(Dynamic)) {
        const auto item = Decode<Dynamic>(FileBytes(dynamicOffset + offset, sizeof(Dynamic)));
        if (item.tag == 0) { terminated = true; break; }
        if (item.tag == 1) { needed.push_back(item.value); continue; }
        if (item.tag == 0x61000045) { moduleRecords.push_back(item.value); continue; }
        if (!tags.emplace(item.tag, item.value).second) throw std::runtime_error("Duplicate ELF dynamic tag");
    }
    if (!terminated) throw std::runtime_error("Unterminated dynamic segment");
    auto get = [&](int64_t tag) { const auto it = tags.find(tag); return it == tags.end() ? uint64_t(0) : it->second; };
    if (get(17) || get(18) || get(35) || get(36)) throw std::runtime_error("REL/RELR relocations are not implemented");
    // SCE table pointers are offsets inside PT_SCE_DYNLIBDATA, not guest VAs.
    // Tag mappings follow AnyPS5 GuestImageReader.cpp. Reject ambiguous pairs
    // even when the table happens to be unused or its size is zero.
    for (const auto& pair : {std::pair<int64_t,int64_t>{5,0x61000035},{10,0x61000037},
            {6,0x61000039},{11,0x6100003b},{7,0x6100002f},{8,0x61000031},
            {9,0x61000033},{23,0x61000029},{2,0x6100002d},{20,0x6100002b},{3,0x61000027}})
        if (tags.contains(pair.first) && tags.contains(pair.second)) throw std::runtime_error("Ambiguous standard/SCE dynamic tags");
    auto value = [&](int64_t standard,int64_t sce) { return get(tags.contains(standard)?standard:sce); };
    auto present = [&](int64_t standard,int64_t sce) { return tags.contains(standard)||tags.contains(sce); };
    auto metadata = [&](int64_t standard,int64_t sce,uint64_t offset,uint64_t size) {
        if (!present(standard,sce)) throw std::runtime_error("Missing dynamic table pointer");
        const auto address=Add(value(standard,sce),offset);
        if (tags.contains(standard)) return GuestFileBytes(address,size);
        if (!hasDynlib || address>dynlibSize || size>dynlibSize-address)
            throw std::runtime_error("SCE table exceeds dynamic data segment");
        return FileBytes(Add(dynlibOffset,address),size);
    };
    const auto stringSize=value(10,0x61000037);
    const auto symbolSize=get(0x6100003f);
    if (tags.contains(0x61000039)) {
        if (!symbolSize || symbolSize%sizeof(Symbol) || value(11,0x6100003b)!=sizeof(Symbol))
            throw std::runtime_error("Invalid SCE dynamic symbol table size");
        metadata(6,0x61000039,0,symbolSize);
    }
    auto string = [&](uint64_t index) {
        if (index >= stringSize) throw std::runtime_error("Dynamic string offset outside table");
        auto bytes = metadata(5,0x61000035,index,stringSize-index);
        auto end = std::find(bytes.begin(), bytes.end(), std::byte(0));
        if (end == bytes.end()) throw std::runtime_error("Unterminated dynamic string");
        return std::string(reinterpret_cast<const char*>(bytes.data()), static_cast<size_t>(end - bytes.begin()));
    };
    std::vector<std::string> plannedDependencies;
    for (auto index : needed) plannedDependencies.push_back(string(index));
    std::map<uint16_t,std::string> modules;
    for (auto record:moduleRecords) {
        if (!modules.emplace(static_cast<uint16_t>(record>>48),string(record&0xffffffffu)).second)
            throw std::runtime_error("Duplicate import module ID");
    }
    std::vector<std::pair<uint64_t, uint64_t>> patches;
    std::vector<PendingImport> pending;
    std::set<uint64_t> targets;
    auto table = [&](int64_t addressTag,int64_t sceAddressTag,int64_t sizeTag,int64_t sceSizeTag) {
        if (!present(addressTag,sceAddressTag) && !present(sizeTag,sceSizeTag)) return;
        if (!present(addressTag,sceAddressTag) || !present(sizeTag,sceSizeTag))
            throw std::runtime_error("Incomplete relocation table metadata");
        const auto size=value(sizeTag,sceSizeTag);
        if (size % sizeof(Relocation)) throw std::runtime_error("Invalid relocation table size");
        const auto records=metadata(addressTag,sceAddressTag,0,size);
        for (uint64_t offset = 0; offset < size; offset += sizeof(Relocation)) {
            auto relocation = Decode<Relocation>(records.subspan(offset,sizeof(Relocation)));
            const uint32_t type = uint32_t(relocation.info);
            const uint32_t index = uint32_t(relocation.info >> 32);
            if (type == 0) continue;
            Address(relocation.address, 8);
            if (!targets.insert(relocation.address).second) throw std::runtime_error("Duplicate relocation target");
            if (type == 8) {
                if (index) throw std::runtime_error("RELATIVE relocation has a symbol");
                patches.emplace_back(relocation.address, LoadBias() + uint64_t(relocation.addend));
                continue;
            }
            if (type != 1 && type != 6 && type != 7) throw std::runtime_error("Unsupported x86 relocation type " + std::to_string(type));
            if (value(11,0x6100003b) != sizeof(Symbol)) throw std::runtime_error("Unsupported ELF symbol size");
            const auto symbolOffset=uint64_t(index)*sizeof(Symbol);
            if (tags.contains(0x6100003f) && (symbolOffset>symbolSize || sizeof(Symbol)>symbolSize-symbolOffset))
                throw std::runtime_error("Dynamic symbol index outside declared table");
            const auto symbol = Decode<Symbol>(metadata(6,0x61000039,symbolOffset,sizeof(Symbol)));
            if ((symbol.info & 15) == 10 || (symbol.info & 15) == 6) throw std::runtime_error("IFUNC/TLS symbol resolution is not implemented");
            std::optional<uint64_t> value;
            if (symbol.section == 0) {
                const auto name = string(symbol.name);
                auto identity=IdentifyImport(name,modules,plannedDependencies);
                value = resolver(identity);
                if (!value) {
                    pending.push_back({name, relocation.address, type,std::move(identity)});
                    continue;
                }
            } else if (symbol.section == 0xfff1) {
                value = symbol.value;
            } else if (symbol.section >= 0xff00) {
                throw std::runtime_error("Unsupported special symbol section");
            } else {
                Address(symbol.value, 0);
                value = LoadBias() + symbol.value;
            }
            patches.emplace_back(relocation.address, *value + (type == 1 ? uint64_t(relocation.addend) : 0));
        }
    };
    if (value(8,0x61000031) && value(9,0x61000033) != sizeof(Relocation)) throw std::runtime_error("Unsupported RELA entry size");
    table(7,0x6100002f,8,0x61000031);
    if (value(2,0x6100002d) && value(20,0x6100002b) != 7) throw std::runtime_error("PLT requires RELA relocations");
    table(23,0x61000029,2,0x6100002d);
    for (const auto& [address, value] : patches) std::memcpy(Address(address, 8), &value, 8);
    dependencies = std::move(plannedDependencies);
    unresolved = std::move(pending);
    applied = patches.size();
    relocated = true;
#endif
}

uint64_t ElfImage::LoadBias() const { return reinterpret_cast<uintptr_t>(mapping) - minimumAddress; }
uint64_t ElfImage::Entry() const { return LoadBias() + entry; }
size_t ElfImage::MappedBytes() const { return mappedBytes; }
size_t ElfImage::AppliedRelocations() const { return applied; }
bool ElfImage::ImportsResolved() const { return relocated && unresolved.empty(); }
const std::vector<LoadSegment>& ElfImage::Segments() const { return segments; }
const std::vector<PendingImport>& ElfImage::Unresolved() const { return unresolved; }
const std::vector<std::string>& ElfImage::Dependencies() const { return dependencies; }
std::span<const std::byte> ElfImage::Read(uint64_t address, size_t size) const { return {Address(address, size), size}; }

}
