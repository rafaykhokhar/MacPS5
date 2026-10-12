#pragma once
#include "pm4.hpp"
#include "rdna_metal.hpp"
#include <memory>

namespace NativePS5 {
struct ShaderImage {
    uint64_t address;
    RdnaRenderStage stage;
    std::vector<uint32_t> words;
};
struct ShaderEntry {
    std::shared_ptr<const ShaderImage> image;
    size_t wordOffset;
    std::span<const uint32_t> Code() const { return std::span(image->words).subspan(wordOffset); }
};
class ShaderRegistry {
public:
    void Register(uint64_t address,RdnaRenderStage stage,std::span<const uint32_t> words);
    ShaderEntry Resolve(const Pm4State& state,RdnaRenderStage stage) const;
private:
    std::map<uint64_t,std::shared_ptr<const ShaderImage>> images;
};
}
