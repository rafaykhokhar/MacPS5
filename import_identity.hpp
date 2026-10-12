#pragma once
#include "elf_image.hpp"
#include <algorithm>
#include <map>
#include <stdexcept>

namespace NativePS5 {
// Module ID encoding and dependency aliases follow AnyPS5's
// core/relinker/domain/include/domain/ImportModule.hpp. Keep ownership even
// when there is no matching DT_NEEDED: never fall back to a different module.
inline ImportIdentity IdentifyImport(const std::string& name,
                                    const std::map<uint16_t,std::string>& modules,
                                    std::span<const std::string> dependencies) {
    ImportIdentity result{name,name,{},{},{}};
    const auto first=name.find('#');
    if (first==std::string::npos) return result;
    const auto second=name.find('#',first+1);
    if (!first || second==std::string::npos || second==first+1 || second+1==name.size())
        throw std::runtime_error("Invalid qualified import: "+name);
    constexpr std::string_view alphabet="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    result.symbol=name.substr(0,first);
    result.libraryToken=name.substr(first+1,second-first-1);
    if (result.libraryToken.find_first_not_of(alphabet)!=std::string::npos)
        throw std::runtime_error("Invalid import library token: "+name);
    uint32_t id=0;
    for (const char c:std::string_view(name).substr(second+1)) {
        const auto digit=alphabet.find(c);
        if (digit==std::string_view::npos || id>(65535u-digit)/64u)
            throw std::runtime_error("Invalid import module ID: "+name);
        id=id*64u+static_cast<uint32_t>(digit);
    }
    result.moduleId=static_cast<uint16_t>(id);
    const auto found=modules.find(*result.moduleId);
    if (found==modules.end()) throw std::runtime_error("Unknown import module ID: "+name);
    result.module=found->second;
    if (result.module.empty() || result.module.find_first_of("/\\:$\r\n")!=std::string::npos)
        throw std::runtime_error("Invalid import module name");
    if (!result.module.ends_with(".prx")) result.module+=".prx";
    if (std::find(dependencies.begin(),dependencies.end(),result.module)!=dependencies.end()) return result;
    auto normalize=[](std::string value) {
        if (value.ends_with(".prx")) value.resize(value.size()-4);
        if (value.ends_with("-module")) value.resize(value.size()-7);
        std::replace(value.begin(),value.end(),'.','_');
        return value;
    };
    std::string match;
    for (const auto& dependency:dependencies) {
        if (normalize(dependency)!=normalize(result.module)) continue;
        if (!match.empty() && match!=dependency) throw std::runtime_error("Ambiguous import module dependency");
        match=dependency;
    }
    if (!match.empty()) result.module=std::move(match);
    return result;
}
}
