#pragma once
#include <array>
#include <cstddef>
#include <cstdint>
// Transport only. Layout construction and fixture state live in Rust payload.rs.
struct RustCredentialRecord { std::array<std::byte,0x8b> bytes; };
struct RustModuleContainer { std::array<std::byte,0x100> bytes; };
static_assert(sizeof(RustCredentialRecord)==0x8b && sizeof(RustModuleContainer)==0x100);
extern "C" RustCredentialRecord native_ps5_fixture_credentials_v1(uint64_t prison) noexcept;
extern "C" RustModuleContainer native_ps5_fixture_module_container_v1() noexcept;
