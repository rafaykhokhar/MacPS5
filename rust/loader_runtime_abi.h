#pragma once
#include <cstdint>
struct RustLoaderBytes {const unsigned char* data;uint64_t size;};
struct RustLoaderResolution {uint64_t value,status;};
struct RustLoaderIdentity {RustLoaderBytes name,symbol,module,libraryToken;uint64_t moduleId;RustLoaderBytes library;uint64_t libraryVersion,moduleVersion;};
struct RustLoaderRecord {RustLoaderIdentity identity;uint64_t address,kind;};
struct RustLoaderPatch {uint64_t address,value;};
static_assert(sizeof(RustLoaderBytes)==16 && sizeof(RustLoaderResolution)==16 && sizeof(RustLoaderIdentity)==104 && sizeof(RustLoaderRecord)==120 && sizeof(RustLoaderPatch)==16);
extern "C" void* native_ps5_image_build_v3(const unsigned char*,uint64_t,uint64_t,uint64_t,void*,RustLoaderResolution(*)(void*,RustLoaderIdentity) noexcept) noexcept;
extern "C" void native_ps5_image_delete_v3(void*) noexcept;
extern "C" uint64_t native_ps5_image_count_v3(const void*,uint64_t) noexcept;
extern "C" RustLoaderPatch native_ps5_image_patch_v3(const void*,uint64_t) noexcept;
extern "C" RustLoaderRecord native_ps5_image_import_v3(const void*,uint64_t) noexcept;
extern "C" RustLoaderBytes native_ps5_image_dependency_v3(const void*,uint64_t) noexcept;
