#pragma once
#include <array>
#include <cstdint>
#include <cstddef>
// C ABI transport. String semantics and export metadata are implemented in Rust.
struct RustStringExport {std::array<char,16> name;std::array<char,12> nid;};
struct RustStringOutcome {uint64_t value,fault;};
struct RustCallArguments {std::array<uint64_t,6> registers;uint64_t stack;};
struct RustMemoryApi {
    void* context;
    uint64_t (*read)(void*,uint64_t,unsigned char*,uint64_t) noexcept;
    uint64_t (*write)(void*,uint64_t,const unsigned char*,uint64_t) noexcept;
};
static_assert(sizeof(RustStringExport)==28 && sizeof(RustStringOutcome)==16 && sizeof(RustMemoryApi)==24 && sizeof(RustCallArguments)==56);
extern "C" RustStringExport native_ps5_string_export_v1(uint64_t) noexcept;
extern "C" RustStringExport native_ps5_dlsym_export_v1() noexcept;
extern "C" RustStringOutcome native_ps5_string_call_v1(RustMemoryApi,uint64_t,uint64_t,uint64_t,uint64_t) noexcept;
extern "C" RustStringOutcome native_ps5_libc_call_v1(RustMemoryApi,uint64_t,RustCallArguments) noexcept;
extern "C" RustStringOutcome native_ps5_format_call_v1(RustMemoryApi,uint64_t,RustCallArguments) noexcept;
struct RustHeapApi {
    void* context;
    uint64_t (*map)(void*,unsigned char*,uint64_t) noexcept;
    uint64_t (*unmap)(void*,uint64_t,unsigned char*,uint64_t) noexcept;
};
static_assert(sizeof(RustHeapApi)==24);
extern "C" void* native_ps5_heap_new_v1(RustHeapApi) noexcept;
extern "C" void native_ps5_heap_delete_v1(void*) noexcept;
extern "C" RustStringOutcome native_ps5_heap_call_v1(void*,uint64_t,uint64_t,uint64_t) noexcept;
extern "C" RustStringOutcome native_ps5_getenv_v1(RustMemoryApi,uint64_t,uint64_t) noexcept;
