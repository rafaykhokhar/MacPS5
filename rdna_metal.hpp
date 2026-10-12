#pragma once
#include <cstdint>
#include <span>
#include <string>

namespace NativePS5 {
// Register-state ABI for isolated straight-line shader blocks, not an AGC ABI.
// Buffers: input VGPR[lanes][256], SGPR[106], output VGPR[lanes][256],
// uint laneCount, uint active[lanes]. All register values are raw 32-bit bits.
std::string TranslateRdnaIntegerBlock(std::span<const uint32_t> words);
enum class RdnaRenderStage { Vertex, Fragment };
// One full-width POS0 or MRT0 export. Test ABI: buffer0 holds 256 initial
// VGPRs per vertex (or one shared fragment seed); buffer1 holds 106 SGPRs.
std::string TranslateRdnaRenderBlock(std::span<const uint32_t> words, RdnaRenderStage stage);
}
