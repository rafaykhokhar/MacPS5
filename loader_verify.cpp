#include "elf_image.hpp"
#include <fstream>
#include <iostream>
#include <iterator>
#include <map>
#include <stdexcept>
#include <unistd.h>

namespace {
template<class T> T Get(std::span<const std::byte> data, size_t offset) {
    if (offset > data.size() || sizeof(T) > data.size() - offset) throw std::runtime_error("Test ELF bounds");
    T value;
    std::memcpy(&value, data.data() + offset, sizeof(T));
    return value;
}
template<class T> void Put(std::vector<std::byte>& data, size_t offset, T value) {
    if (offset > data.size() || sizeof(T) > data.size() - offset) throw std::runtime_error("Fixture bounds");
    std::memcpy(data.data() + offset, &value, sizeof(T));
}
void Require(bool value, const char* error) { if (!value) throw std::runtime_error(error); }
std::vector<std::byte> Fixture() {
    std::vector<std::byte> data(4096);
    const unsigned char identity[] = {0x7f, 'E', 'L', 'F', 2, 1, 1};
    std::memcpy(data.data(), identity, sizeof(identity));
    Put<uint16_t>(data, 16, 3); Put<uint16_t>(data, 18, 62); Put<uint32_t>(data, 20, 1);
    Put<uint64_t>(data, 24, 0x100); Put<uint64_t>(data, 32, 64);
    Put<uint16_t>(data, 52, 64); Put<uint16_t>(data, 54, 56); Put<uint16_t>(data, 56, 2);
    Put<uint32_t>(data, 64, 1); Put<uint32_t>(data, 68, 7);
    Put<uint64_t>(data, 96, data.size()); Put<uint64_t>(data, 104, 8192); Put<uint64_t>(data, 112, 4096);
    Put<uint32_t>(data, 120, 2); Put<uint64_t>(data, 128, 0x180); Put<uint64_t>(data, 136, 0x180);
    Put<uint64_t>(data, 152, 128); Put<uint64_t>(data, 160, 128);
    Put<uint8_t>(data, 0x100, 0xc3);
    std::pair<int64_t, uint64_t> tags[] = {{7, 0x340}, {8, 72}, {9, 24}, {6, 0x240}, {11, 24}, {5, 0x300}, {10, 5}, {0,0}};
    for (size_t i = 0; i < 8; ++i) {
        Put<int64_t>(data, 0x180 + i * 16, tags[i].first);
        Put<uint64_t>(data, 0x188 + i * 16, tags[i].second);
    }
    std::memcpy(data.data() + 0x300, "\0foo", 5);
    Put<uint32_t>(data, 0x258, 1); Put<uint8_t>(data, 0x25c, 0x12);
    Put<uint8_t>(data, 0x274, 0x12); Put<uint16_t>(data, 0x276, 1); Put<uint64_t>(data, 0x278, 0x100);
    Put<uint64_t>(data, 0x340, 0x400); Put<uint64_t>(data, 0x348, 8); Put<int64_t>(data, 0x350, 0x100);
    Put<uint64_t>(data, 0x358, 0x408); Put<uint64_t>(data, 0x360, (uint64_t(1)<<32) | 6);
    Put<uint64_t>(data, 0x370, 0x410); Put<uint64_t>(data, 0x378, (uint64_t(2)<<32) | 1); Put<int64_t>(data, 0x380, -4);
    return data;
}

void SelfTest() {
    auto missing = [](std::string_view) -> std::optional<uint64_t> { return {}; };
    auto data = Fixture();
    NativePS5::ElfImage image(data);
    Require(!image.ImportsResolved(), "Fresh image cannot be resolved");
    for (auto byte : image.Read(4096, 4096)) Require(byte == std::byte(0), "BSS not zero");
    image.Relocate(missing);
    Require(image.AppliedRelocations() == 2 && image.Unresolved().size() == 1 && !image.ImportsResolved(), "Missing import was not retained");
    Require(image.Unresolved()[0].name == "foo", "Wrong symbol name");
    Require(Get<uint64_t>(image.Read(0x400, 8), 0) == image.LoadBias() + 0x100, "RELATIVE mismatch");
    Require(Get<uint64_t>(image.Read(0x410, 8), 0) == image.LoadBias() + 0xfc, "64-bit symbol/addend mismatch");
    Require(Get<uint64_t>(image.Read(0x408, 8), 0) == 0, "Missing import patched");
    NativePS5::ElfImage resolved(data);
    resolved.Relocate([](std::string_view name) -> std::optional<uint64_t> { if (name == "foo") return 0x1234567890; return {}; });
    Require(resolved.ImportsResolved() && resolved.AppliedRelocations() == 3, "Resolver failed");
    Require(Get<uint64_t>(resolved.Read(0x408, 8), 0) == 0x1234567890, "GLOB_DAT mismatch");
    size_t rejected = 0;
    auto invalid = [&](std::vector<std::byte> bytes, size_t budget = 256*1024*1024) {
        try { NativePS5::ElfImage bad(bytes, budget); bad.Relocate(missing); }
        catch (const std::exception&) { ++rejected; return; }
        throw std::runtime_error("Malformed fixture accepted");
    };
    invalid({});
    invalid(data, 1024);
    auto corrupt = [&](size_t offset, uint64_t value) { auto b = data; Put<uint64_t>(b, offset, value); invalid(b); };
    corrupt(96, UINT64_MAX);
    corrupt(104, UINT64_MAX);
    corrupt(112, 3);
    corrupt(24, 0x5000);
    corrupt(0x348, 37);
    corrupt(0x340, 8190);
    corrupt(0x188, UINT64_MAX);
    corrupt(0x1e8, 1);
    corrupt(0x360, (uint64_t(100000)<<32) | 6);
    corrupt(32, 1);
    corrupt(112, uint64_t(getpagesize()) * 2);
    try { image.Relocate(missing); throw std::logic_error("Repeated relocation accepted"); }
    catch (const std::runtime_error&) { ++rejected; }
    std::cout << "PASS: native ELF mapping, BSS, RELATIVE/GLOB_DAT/64 relocations, unresolved-import gate; " << rejected << " rejection checks\n";
}

void RealFile(const char* path) {
    std::ifstream stream(path, std::ios::binary);
    if (!stream) throw std::runtime_error("Cannot open ELF");
    std::vector<char> chars((std::istreambuf_iterator<char>(stream)), {});
    auto bytes = std::as_bytes(std::span(chars));
    NativePS5::ElfImage image(bytes);
    for (const auto& segment : image.Segments()) {
        Require(std::memcmp(image.Read(segment.address, segment.fileSize).data(), bytes.data()+segment.fileOffset, segment.fileSize) == 0, "Segment copy mismatch");
        for (auto byte : image.Read(segment.address+segment.fileSize, segment.memorySize-segment.fileSize)) Require(byte == std::byte(0), "Real BSS not zero");
    }
    image.Relocate([](std::string_view) -> std::optional<uint64_t> { return {}; });
    const auto sectionOffset = Get<uint64_t>(bytes, 40);
    const auto sectionCount = Get<uint16_t>(bytes, 60);
    Require(Get<uint16_t>(bytes, 58) == 64, "Reference checker needs section headers");
    size_t verified = 0, unresolved = 0;
    for (size_t i = 0; i < sectionCount; ++i) {
        auto offset = sectionOffset + i*64;
        if (Get<uint32_t>(bytes, offset+4) != 4) continue;
        const auto start = Get<uint64_t>(bytes, offset+24), size = Get<uint64_t>(bytes, offset+32);
        Require(Get<uint64_t>(bytes, offset+56) == 24 && size%24 == 0, "Reference RELA shape");
        for (size_t n = 0; n < size; n += 24) {
            auto target = Get<uint64_t>(bytes, start+n), info = Get<uint64_t>(bytes, start+n+8);
            auto addend = Get<int64_t>(bytes, start+n+16);
            if (uint32_t(info) == 8) {
                Require(Get<uint64_t>(image.Read(target,8),0) == image.LoadBias()+uint64_t(addend), "Real relocation mismatch");
                ++verified;
            } else if (uint32_t(info) == 6) ++unresolved;
            else throw std::runtime_error("Unexpected relocation in homebrew reference fixture");
        }
    }
    Require(verified == image.AppliedRelocations() && unresolved == image.Unresolved().size(), "Reference counts mismatch");
    Require(!image.ImportsResolved(), "Homebrew must not be marked ready with missing services");
    std::cout << "PASS: real ELF file bytes and BSS verified; " << verified << " relocations verified against section tables\n"
              << "Mapped bytes: " << image.MappedBytes() << "; unresolved imports: " << image.Unresolved().size() << "\n";
    for (const auto& dependency : image.Dependencies()) std::cout << "Dependency: " << dependency << "\n";
    std::cout << "Guest code has not executed; mapped x86 pages are non-executable.\n";
}
}

int main(int argc, char** argv) {
    try {
        SelfTest();
        if (argc == 2) RealFile(argv[1]);
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "FAIL: " << error.what() << "\n";
        return 1;
    }
}
