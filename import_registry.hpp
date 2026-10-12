#pragma once
#include "elf_image.hpp"
#include <map>

namespace NativePS5 {
// Explicit native bridge exports, scoped by module. This does not load guest
// libraries or implement library-version matching / symbol interposition.
class ImportRegistry {
public:
#ifdef NATIVE_PS5_RUST_LOADER
    ImportRegistry();
    ~ImportRegistry();
    ImportRegistry(const ImportRegistry&)=delete;
    ImportRegistry& operator=(const ImportRegistry&)=delete;
#endif
    void Register(std::string module,std::string name,uint64_t address);
    std::optional<uint64_t> Resolve(const ImportIdentity& identity) const;
private:
#ifdef NATIVE_PS5_RUST_LOADER
    void* registry=nullptr;
#else
    std::map<std::pair<std::string,std::string>,uint64_t> exports;
#endif
};
}
