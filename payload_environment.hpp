#pragma once
#include "bootstrap_services.hpp"
#include "guest_process_list.hpp"
#include "guest_module_list.hpp"
#include <unistd.h>
#ifdef NATIVE_PS5_RUST_PAYLOAD
#include "rust/payload_abi.h"
#include "rust/string_abi.h"
#endif

namespace NativePS5 {
// Explicit 7.00 layout model for the selected payload, not console firmware.
// Keep this owner alive throughout guest execution; user/services must outlive it.
class PayloadEnvironment {
    static constexpr uint64_t Base=0xffff800000000000;
    GuestMemory kernel;
    std::array<std::byte,64> control{},prison{},args{};
#ifdef NATIVE_PS5_RUST_PAYLOAD
    RustCredentialRecord credentialRecord=native_ps5_fixture_credentials_v1(0xffff800006200000);
    std::span<std::byte> credentials=credentialRecord.bytes;
#else
    std::array<std::byte,128> credentials{};
#endif
    std::array<std::byte,24> parameters{};
    std::array<std::byte,16> descriptors{};
    std::array<std::byte,8> result{};
    GuestProcessList processes;
    GuestModuleList modules;
    static uint64_t Address(std::span<std::byte> bytes){return reinterpret_cast<uintptr_t>(bytes.data());}
public:
    static constexpr uint64_t Allproc=Base+0x2859d50,Prison=Base+0x6200000;
    explicit PayloadEnvironment(GuestMemory& user,BootstrapServices& services)
        :processes(kernel,Allproc,Base+0x4000000),modules(kernel,user,Base+0x5000000,Base+0x5100000) {
        kernel.AddAt(Base+0x6100000,control,true);
        kernel.AddAt(Base+0x6000000,credentials,true);
        kernel.AddAt(Prison,prison,true);
        const auto process=processes.Add(static_cast<uint32_t>(getpid()));
        kernel.Store(process+0x40,Base+0x6000000,8);
        kernel.Store(Base+0x6000000+0x30,Prison,8);
        // Only these observed credential fields are modeled. They do not grant
        // host privileges or implement guest authorization/prison operations.
        kernel.Store(process+0x3e8,modules.RootAddress(),8);
#ifdef NATIVE_PS5_RUST_PAYLOAD
        const auto dlsymExport=native_ps5_dlsym_export_v1();
        const std::array<GuestModuleList::Export,3> exports{{{"9BcDykPmo1I",services.ErrorAddress()},{"HoLVWNanBBc",services.GetpidAddress()},{dlsymExport.nid.data(),services.DlsymAddress()}}};
#else
        const std::array<GuestModuleList::Export,2> exports{{{"9BcDykPmo1I",services.ErrorAddress()},{"HoLVWNanBBc",services.GetpidAddress()}}};
#endif
        modules.Add(1,0,exports);modules.Add(0x2001,0,exports);
        const std::array<GuestModuleList::Export,4> libc{{{"eLdDw6l0-bU",services.SnprintfAddress()},
            {"Q2V+iqvjgC0",services.VsnprintfAddress()},{"RIa6GnWp+iU",services.StrerrorAddress()},
            {"q1OvUam0BJU",services.IsThreadedAddress(),GuestModuleList::ExportKind::Object,4,true}}};
#ifdef NATIVE_PS5_RUST_PAYLOAD
        std::vector<GuestModuleList::Export> rustLibc(libc.begin(),libc.end());
        for(uint64_t i=0;i<10;++i){const auto entry=native_ps5_string_export_v1(i);rustLibc.push_back({entry.nid.data(),services.RustStringAddress(i)});}
        modules.Add(2,0,rustLibc);
#else
        modules.Add(2,0,libc);
#endif
        user.Add(args,true);user.Add(descriptors,true);user.Add(result,true);user.Add(parameters,true);
        user.Store(Address(parameters),parameters.size(),8);
        user.Store(Address(parameters)+20,0x07000000,4);
        services.RegisterProcessParameters(2,Address(parameters));
        auto [selector,writer]=services.CreatePacketControlPair(kernel);
        auto [reader,pipeWriter]=services.CreateKernelPipe(kernel,Base+0x6100000);
        user.Store(Address(descriptors),reader,4);user.Store(Address(descriptors)+4,pipeWriter,4);
        user.Store(Address(descriptors)+8,selector,4);user.Store(Address(descriptors)+12,writer,4);
        user.Store(Address(args),services.DlsymAddress(),8);
        user.Store(Address(args)+8,Address(descriptors),8);
        user.Store(Address(args)+16,Address(descriptors)+8,8);
        user.Store(Address(args)+24,Base+0x6100000,8);
        user.Store(Address(args)+32,Base,8);
        user.Store(Address(args)+40,Address(result),8);
    }
    PayloadEnvironment(const PayloadEnvironment&)=delete;
    PayloadEnvironment& operator=(const PayloadEnvironment&)=delete;
    uint64_t Arguments(){return Address(args);}
    uint64_t Result(){return Address(result);}
};
}
