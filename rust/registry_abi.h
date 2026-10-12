#pragma once
#include "loader_runtime_abi.h"
extern "C" void* native_ps5_registry_new_v1() noexcept;
extern "C" void native_ps5_registry_delete_v1(void*) noexcept;
extern "C" uint64_t native_ps5_registry_register_v1(void*,RustLoaderBytes,RustLoaderBytes,uint64_t) noexcept;
extern "C" RustLoaderResolution native_ps5_registry_resolve_v2(const void*,RustLoaderIdentity) noexcept;
