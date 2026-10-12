#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <functional>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace NativePS5 {

struct ImportIdentity {
    std::string name; // Original symbol, including any qualification.
    std::string symbol; // Plain name or NID before the first '#'.
    std::string module;
    std::string libraryToken; // Consumer-local identifier; not a library name/version.
    std::optional<uint16_t> moduleId;
    std::string library = {};
    std::optional<uint16_t> libraryVersion = {};
    std::optional<uint16_t> moduleVersion = {};
};

struct LoadSegment {
    uint64_t address, fileOffset, fileSize, memorySize;
    uint32_t flags;
};

struct PendingImport {
    std::string name;
    uint64_t address;
    uint32_t relocationType;
    ImportIdentity identity;
};

class ElfImage {
public:
    using Resolver = std::function<std::optional<uint64_t>(std::string_view)>;
    using ScopedResolver = std::function<std::optional<uint64_t>(const ImportIdentity&)>;
    explicit ElfImage(std::span<const std::byte> file, size_t budget = 256 * 1024 * 1024);
    ~ElfImage();
    ElfImage(const ElfImage&) = delete;
    ElfImage& operator=(const ElfImage&) = delete;
    void Relocate(const Resolver& resolver);
    void RelocateScoped(const ScopedResolver& resolver);
    uint64_t LoadBias() const;
    uint64_t Entry() const;
    size_t MappedBytes() const;
    size_t AppliedRelocations() const;
    bool ImportsResolved() const;
    const std::vector<LoadSegment>& Segments() const;
    const std::vector<PendingImport>& Unresolved() const;
    const std::vector<std::string>& Dependencies() const;
    std::span<const std::byte> Read(uint64_t address, size_t size) const;

private:
    std::vector<std::byte> file;
    std::vector<LoadSegment> segments;
    std::vector<PendingImport> unresolved;
    std::vector<std::string> dependencies;
    void* mapping = nullptr;
    size_t mappedBytes = 0;
    uint64_t minimumAddress = 0;
    uint64_t entry = 0;
    uint64_t dynamicOffset = 0;
    uint64_t dynamicSize = 0;
    uint64_t dynlibOffset = 0;
    uint64_t dynlibSize = 0;
    bool hasDynlib = false;
    size_t applied = 0;
    size_t loaderBudget = 0;
    bool relocated = false;
    std::span<const std::byte> FileBytes(uint64_t offset, uint64_t size) const;
    std::span<const std::byte> GuestFileBytes(uint64_t address, uint64_t size) const;
    std::byte* Address(uint64_t address, uint64_t size) const;
};

}
