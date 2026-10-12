#include "import_registry.hpp"
#ifdef NATIVE_PS5_RUST_LOADER
#include "rust/registry_abi.h"
#else
#include <nid/NidCompute.hpp>
#endif
#include <stdexcept>

namespace NativePS5 {
#ifdef NATIVE_PS5_RUST_LOADER
namespace {
RustLoaderBytes RustBytes(std::string_view text) {return {reinterpret_cast<const unsigned char*>(text.data()),text.size()};}
}
ImportRegistry::ImportRegistry():registry(native_ps5_registry_new_v1()) {if(!registry)throw std::runtime_error("Cannot create Rust export registry");}
ImportRegistry::~ImportRegistry(){native_ps5_registry_delete_v1(registry);}
void ImportRegistry::Register(std::string module,std::string name,uint64_t address){
    if(native_ps5_registry_register_v1(registry,RustBytes(module),RustBytes(name),address))throw std::runtime_error("Rust export registration failed");
}
std::optional<uint64_t> ImportRegistry::Resolve(const ImportIdentity& identity) const {
    const auto result=native_ps5_registry_resolve_v2(registry,{RustBytes(identity.name),RustBytes(identity.symbol),RustBytes(identity.module),RustBytes(identity.libraryToken),identity.moduleId?uint64_t(*identity.moduleId):65536,
        RustBytes(identity.library),identity.libraryVersion?uint64_t(*identity.libraryVersion):65536,identity.moduleVersion?uint64_t(*identity.moduleVersion):65536});
    return result.status?std::nullopt:std::optional<uint64_t>(result.value);
}
#else
void ImportRegistry::Register(std::string module,std::string name,uint64_t address) {
    if (module.empty() || name.empty() || !address || name.find('#')!=std::string::npos
        || module.find_first_of("/\\:$\r\n")!=std::string::npos)
        throw std::invalid_argument("Invalid native import export");
    const auto nid=Nid::ComputeNid(name,"");
    auto planned=exports;
    for (const auto& token:{name,nid}) {
        const auto [entry,inserted]=planned.emplace(std::pair(module,token),address);
        if (!inserted && entry->second!=address) throw std::runtime_error("Conflicting native import export");
    }
    exports=std::move(planned);
}
std::optional<uint64_t> ImportRegistry::Resolve(const ImportIdentity& identity) const {
    if (identity.moduleId && identity.module.empty()) return {};
    if (!identity.module.empty()) {
        const auto found=exports.find({identity.module,identity.symbol});
        return found==exports.end()?std::nullopt:std::optional(found->second);
    }
    std::optional<uint64_t> result;
    for (const auto& [key,address]:exports) {
        if (key.second!=identity.symbol) continue;
        // Even equal addresses from two modules are ambiguous ownership.
        if (result) return {};
        result=address;
    }
    return result;
}
#endif
}
