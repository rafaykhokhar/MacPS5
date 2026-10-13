use native_ps5::{
    c_locale::Ctype,
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service},
    modules::Arguments,
    stdio_file::Operation as FileOperation,
};
use std::{collections::BTreeSet, path::Path};
mod audio_native;
mod direct_memory_native;
mod equeue_native;
mod exit_native;
mod fma213_native;
mod fma231_native;
mod ime_native;
mod keyboard_native;
mod libc_memory_native;
mod locale_native;
mod mutex_native;
mod net_native;
mod pad_native;
mod puts_native;
mod quantum_native;
mod realloc_native;
mod scheduler_native;
mod sprx_symbol_native;
mod stat_native;
mod stdio_native;
mod stdio_file_native;
mod file_syscalls_native;
mod fstat_native;
mod stat_service_native;
mod semaphore_native;
mod atomic_xadd_native;
mod adc_native;
mod sysconf_native;
mod sysctl_native;
mod sigaction_native;
mod wcstombs_native;
mod sort_search_native;
mod fgets_native;
mod getcwd_native;
mod strdup_native;
mod mkdir_native;
mod cond_native;
mod multibyte_native;
mod abort_native;
mod movdqu_native;
mod vasprintf_native;
mod vasprintf_hex_native;
mod printf_stdout_native;
mod fprintf_native;
mod remove_native;
mod rename_native;
mod strncpy_native;
mod sscanf_native;
mod directory_native;
mod putenv_native;
mod fgetc_native;
mod strcoll_native;
mod strxfrm_native;
mod wcscoll_native;
mod lzcnt_native;
mod bit_test_native;
mod narrow_shift_native;
mod mul_native;
mod byte_divide_native;
mod wcsxfrm_native;
mod mbrlen_native;
mod mbtowc_native;
mod mb_cur_max_native;
mod strtok_r_native;
mod stdout_byte_native;
mod packed_compare_shift_native;
mod vpbroadcastd_native;
mod packed_broadcast_native;
mod packed_insert_native;
mod packed_extract_native;
mod packed_shift_native;
mod packed_equal_native;
mod packed_addsub_native;
mod packed_test_native;
mod packed_extend_native;
mod packed_logical_native;
mod packed_shuffle_native;
mod packed_unpack_native;
mod packed_permute_native;
mod packed_blend_native;
mod packed_minmax_native;
mod vector_extract128_native;
mod strftime_native;
mod strtof_native;
mod strtold_native;
mod vmovd_native;
mod vmovq_native;
mod cvtsi2sd_native;
mod vpxor_native;
mod system_service_native;
mod threadattr_native;
mod thread_services_native;
mod thread_module_native;
mod thread_name_native;
mod thread_pins_native;
mod signal_set_native;
mod signal_mask_native;
mod cancel_type_native;
mod scheduling_native;
mod sdl_scheduling_native;
mod environment_native;
mod memory_compare_native;
mod wide_strings_native;
mod narrow_strings_native;
mod integer_parse_native;
mod x87_control_native;
mod x87_store_native;
mod c_locale_native;
mod float_parse_native;
mod scan_native;
mod guest_time_native;
mod imul_implicit_native;
mod sleep_native;
mod tiler_native;
mod tiler_gpu_native;
mod tls_native;
mod truncate_native;
mod truncate64_native;
mod user_service_native;
mod vmovsd_native;
mod vmulsd_native;
mod worker_native;
fn main() -> Result<(), String> {
    let startup_only = match std::env::args_os().nth(2).as_deref() {
        None => false,
        Some(value) if value == "--startup-only" => true,
        Some(_) => return Err("Expected optional --startup-only".into()),
    };
    let file = std::fs::read(
        std::env::args_os()
            .nth(1)
            .ok_or("Missing real homebrew ELF")?,
    )
    .map_err(|e| e.to_string())?;
    let archive =
        std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").ok_or("Missing runtime archive")?;
    let directory =
        std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").ok_or("Missing compiler output directory")?;
    let presentation_result = if startup_only {
        println!("STARTUP-ONLY: component regressions and Metal presentation are SKIPPED; this run cannot establish their success");
        None
    } else {
        stat_native::verify(Path::new(&archive), Path::new(&directory))?;
        stdio_native::verify(Path::new(&archive), Path::new(&directory))?;
        file_syscalls_native::verify(Path::new(&archive), Path::new(&directory))?;
        fstat_native::verify(Path::new(&archive), Path::new(&directory))?;
        stat_service_native::verify(Path::new(&archive), Path::new(&directory))?;
        atomic_xadd_native::verify(Path::new(&archive), Path::new(&directory))?;
        adc_native::verify(Path::new(&archive), Path::new(&directory))?;
        sysconf_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        sysctl_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        sigaction_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        wcstombs_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        sort_search_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("sort-search"))?;
        fgets_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("fgets"))?;
        getcwd_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("getcwd"))?;
        strdup_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strdup"))?;
        mkdir_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("mkdir"))?;
        cond_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("condition"))?;
        multibyte_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("multibyte"))?;
        abort_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("abort"))?;
        movdqu_native::verify(Path::new(&archive), &Path::new(&directory).join("movdqu"))?;
        vasprintf_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("vasprintf"))?;
        vasprintf_hex_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("vasprintf-hex"))?;
        printf_stdout_native::verify(Path::new(&archive), &Path::new(&directory).join("printf-stdout"))?;
        fprintf_native::verify(Path::new(&archive), &Path::new(&directory).join("fprintf"))?;
        remove_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("remove"))?;
        rename_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("rename"))?;
        strncpy_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strncpy"))?;
        sscanf_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("sscanf"))?;
        directory_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("directory"))?;
        putenv_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("putenv"))?;
        fgetc_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("fgetc"))?;
        strcoll_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strcoll"))?;
        strxfrm_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strxfrm"))?;
        wcscoll_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("wcscoll"))?;
        lzcnt_native::verify(Path::new(&archive), &Path::new(&directory).join("lzcnt"))?;
        bit_test_native::verify(Path::new(&archive), &Path::new(&directory).join("bit-test"))?;
        narrow_shift_native::verify(Path::new(&archive), &Path::new(&directory).join("narrow-shift"))?;
        mul_native::verify(Path::new(&archive), &Path::new(&directory).join("mul"))?;
        byte_divide_native::verify(Path::new(&archive), &Path::new(&directory).join("byte-divide"))?;
        wcsxfrm_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("wcsxfrm"))?;
        mbrlen_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("mbrlen"))?;
        mbtowc_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("mbtowc"))?;
        mb_cur_max_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("mb-cur-max"))?;
        strtok_r_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strtok-r"))?;
        stdout_byte_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("stdout-byte"))?;
        packed_compare_shift_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-compare-shift"))?;
        vpbroadcastd_native::verify(Path::new(&archive), &Path::new(&directory).join("vpbroadcastd"))?;
        packed_broadcast_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-broadcast"))?;
        packed_insert_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-insert"))?;
        packed_extract_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-extract"))?;
        packed_shift_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-shift"))?;
        packed_equal_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-equal"))?;
        packed_addsub_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-addsub"))?;
        packed_test_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-test"))?;
        packed_extend_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-extend"))?;
        packed_logical_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-logical"))?;
        packed_shuffle_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-shuffle"))?;
        packed_unpack_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-unpack"))?;
        packed_permute_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-permute"))?;
        packed_blend_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-blend"))?;
        packed_minmax_native::verify(Path::new(&archive), &Path::new(&directory).join("packed-minmax"))?;
        vector_extract128_native::verify(Path::new(&archive), &Path::new(&directory).join("vector-extract128"))?;
        strftime_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strftime"))?;
        strtof_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strtof"))?;
        strtold_native::verify(&file, Path::new(&archive), &Path::new(&directory).join("strtold"))?;
        vmovd_native::verify(Path::new(&archive), &Path::new(&directory).join("vmovd"))?;
        vmovq_native::verify(Path::new(&archive), &Path::new(&directory).join("vmovq"))?;
        cvtsi2sd_native::verify(Path::new(&archive), &Path::new(&directory).join("cvtsi2sd"))?;
        vpxor_native::verify(Path::new(&archive), &Path::new(&directory).join("vpxor"))?;
        semaphore_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        stdio_file_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        system_service_native::verify(Path::new(&archive), Path::new(&directory))?;
        tls_native::verify(Path::new(&archive), Path::new(&directory))?;
        threadattr_native::verify(Path::new(&archive), Path::new(&directory))?;
        vmovsd_native::verify(Path::new(&archive), Path::new(&directory))?;
        worker_native::verify(Path::new(&archive), Path::new(&directory))?;
        quantum_native::verify(Path::new(&archive), Path::new(&directory))?;
        truncate_native::verify(Path::new(&archive), Path::new(&directory))?;
        truncate64_native::verify(Path::new(&archive), &Path::new(&directory).join("truncate64"))?;
        scheduler_native::verify(Path::new(&archive), Path::new(&directory))?;
        vmulsd_native::verify(Path::new(&archive), Path::new(&directory))?;
        fma213_native::verify(Path::new(&archive), Path::new(&directory))?;
        fma231_native::verify(Path::new(&archive), &Path::new(&directory).join("fma231"))?;
        thread_services_native::verify(Path::new(&archive), Path::new(&directory))?;
        thread_module_native::verify(Path::new(&archive), Path::new(&directory))?;
        thread_name_native::verify(Path::new(&archive), Path::new(&directory))?;
        thread_pins_native::verify(Path::new(&archive), Path::new(&directory))?;
        signal_set_native::verify(Path::new(&archive), Path::new(&directory))?;
        signal_mask_native::verify(Path::new(&archive), Path::new(&directory))?;
        cancel_type_native::verify(Path::new(&archive), Path::new(&directory))?;
        scheduling_native::verify(Path::new(&archive), Path::new(&directory))?;
        sdl_scheduling_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        environment_native::verify(Path::new(&archive), Path::new(&directory))?;
        memory_compare_native::verify(Path::new(&archive), Path::new(&directory))?;
        wide_strings_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        narrow_strings_native::verify(Path::new(&archive), Path::new(&directory))?;
        integer_parse_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        x87_control_native::verify(Path::new(&archive), Path::new(&directory))?;
        x87_store_native::verify(Path::new(&archive), Path::new(&directory))?;
        c_locale_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        float_parse_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        scan_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        imul_implicit_native::verify(Path::new(&archive), Path::new(&directory))?;
        guest_time_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        sleep_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        tiler_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        // The presenter closes before returning. Preserve its failure while checking
        // independent startup behavior; do not turn a presentation failure into PASS.
        let presentation_result =
            tiler_gpu_native::verify(&file, Path::new(&archive), Path::new(&directory));
        if let Err(error) = &presentation_result {
            eprintln!("PRESENTATION FAILED (retained until final result): {error}");
        }
        user_service_native::verify(Path::new(&archive), Path::new(&directory))?;
        pad_native::verify(Path::new(&archive), Path::new(&directory))?;
        puts_native::verify(Path::new(&archive), Path::new(&directory))?;
        realloc_native::verify(Path::new(&archive), Path::new(&directory))?;
        mutex_native::verify(Path::new(&archive), Path::new(&directory))?;
        audio_native::verify(Path::new(&archive), Path::new(&directory))?;
        direct_memory_native::verify(Path::new(&archive), Path::new(&directory))?;
        equeue_native::verify(Path::new(&archive), Path::new(&directory))?;
        exit_native::verify(Path::new(&archive), Path::new(&directory))?;
        keyboard_native::verify(Path::new(&archive), Path::new(&directory))?;
        libc_memory_native::verify(Path::new(&archive), Path::new(&directory))?;
        locale_native::verify(Path::new(&archive), Path::new(&directory))?;
        ime_native::verify(Path::new(&archive), Path::new(&directory))?;
        net_native::verify(Path::new(&archive), Path::new(&directory))?;
        sprx_symbol_native::verify(&file, Path::new(&archive), Path::new(&directory))?;
        Some(presentation_result)
    };
    let bias = 0x10000000;
    let mut image = Image::load(&file, bias, 64 * 1024 * 1024)?;
    image.relocate(|_| None)?;
    println!(
        "Real ELF loaded: {} unresolved imports; only entry bootstrap is being probed",
        image.unresolved.len()
    );
    println!("Guest DT_NEEDED dependencies: {:?}", image.dependencies);
    let mut native = NativeExecutor::new(
        0x20000000,
        native_ps5::guest_thread_attr::DEFAULT_STACK_SIZE as usize,
        30_000_000,
        30_000_000,
    )?;
    // Reported virtual hardware; aggregate host/unified-memory fit is unverified.
    native.configure_system_profile(native_ps5::guest_system::GuestSystemProfile::new(
        16 * 1024 * 1024 * 1024,
        1,
        1,
    )?)?;
    native.register_service(0x700020, Service::Dlsym)?;
    native.register_service(0x710000, Service::Getpid)?;
    native.register_service(0x71000a, Service::Syscall)?;
    native.register_service(0x710020, Service::Errno)?;
    native.register_service(0x7b0000, Service::DirectAllocate)?;
    native.register_service(0x7b0010, Service::DirectMap)?;
    native.register_service(0x7b0020, Service::DirectRelease)?;
    native.register_service(0x7b0030, Service::EqueueCreate)?;
    native.register_service(0x7b0040, Service::EqueueWait)?;
    native.register_service(0x7b0050, Service::EqueueDelete)?;
    native.register_service(0x720000, Service::Snprintf)?;
    native.register_service(0x720010, Service::Strerror)?;
    native.register_service(0x720020, Service::Vsnprintf)?;
    native.register_service(0x740000, Service::LoadStartModule)?;
    native.register_service(0x740010, Service::StopUnloadModule)?;
    native.register_service(0x750000, Service::SysmoduleLoadInternal)?;
    native.register_service(0x770000, Service::SystemHideSplashScreen)?;
    native.register_service(0x770010, Service::SystemLaunchWebBrowser)?;
    let system_service_exports = [
        ("sceSystemServiceHideSplashScreen", 0x770000),
        ("sceSystemServiceLaunchWebBrowser", 0x770010),
    ];
    native.user_service.register_user(1, b"Player 1")?;
    native.user_service.set_foreground_user(1)?;
    let user_service_exports = [
        ("sceUserServiceInitialize", 0x780000),
        ("sceUserServiceGetForegroundUser", 0x780010),
        ("sceUserServiceGetLoginUserIdList", 0x780020),
        ("sceUserServiceGetUserName", 0x780030),
    ];
    for (address, service) in [
        (0x780000, Service::UserInitialize),
        (0x780010, Service::UserGetForeground),
        (0x780020, Service::UserGetLoginList),
        (0x780030, Service::UserGetName),
    ] {
        native.register_service(address, service)?;
    }
    native.register_service(0x760000, Service::Getargc)?;
    let pad_exports = [
        ("scePadInit", 0x790000),
        ("scePadOpen", 0x790010),
        ("scePadClose", 0x790020),
        ("scePadReadState", 0x790030),
        ("scePadSetLightBar", 0x790040),
        ("scePadSetVibration", 0x790050),
        ("scePadSetVibrationMode", 0x790060),
    ];
    let audio_exports = [
        ("sceAudioOutInit", 0x7a0000),
        ("sceAudioOutOpen", 0x7a0010),
        ("sceAudioOutOutput", 0x7a0020),
        ("sceAudioOutClose", 0x7a0030),
    ];
    let video_exports = [
        ("sceVideoOutOpen", 0x7c0000),
        ("sceVideoOutClose", 0x7c0010),
        ("sceVideoOutSetFlipRate", 0x7c0020),
        ("sceVideoOutSetBufferAttribute2", 0x7c0030),
        ("sceVideoOutRegisterBuffers2", 0x7c0040),
        ("sceVideoOutAddFlipEvent", 0x7c0050),
        ("sceVideoOutDeleteFlipEvent", 0x7c0060),
        ("sceVideoOutSubmitFlip", 0x7c0070),
    ];
    let keyboard_exports = [
        ("sceKeyboardInit", 0x7d0000),
        ("sceKeyboardOpen", 0x7d0010),
        ("sceKeyboardReadState", 0x7d0020),
        ("sceKeyboardClose", 0x7d0030),
    ];
    let ime_exports = [
        ("sceImeDialogGetStatus", 0x7e0000),
        ("sceImeDialogInit", 0x7e0010),
        ("sceImeDialogGetResult", 0x7e0020),
        ("sceImeDialogTerm", 0x7e0030),
    ];
    let net_exports = [
        ("sceNetErrnoLoc", 0x7f0000),
        ("sceNetPoolCreate", 0x7f0010),
        ("sceNetPoolDestroy", 0x7f0020),
        ("sceNetResolverCreate", 0x7f0030),
        ("sceNetResolverDestroy", 0x7f0040),
        ("sceNetResolverStartAton", 0x7f0050),
        ("sceNetResolverStartNtoa", 0x7f0060),
    ];
    for ((_, address), service) in net_exports.iter().zip([
        Service::NetErrnoLoc,
        Service::NetPoolCreate,
        Service::NetPoolDestroy,
        Service::NetResolverCreate,
        Service::NetResolverDestroy,
        Service::NetResolverStartAton,
        Service::NetResolverStartNtoa,
    ]) {
        native.register_service(*address, service)?;
    }
    for ((_, address), service) in ime_exports.iter().zip([
        Service::ImeGetStatus,
        Service::ImeInitialize,
        Service::ImeGetResult,
        Service::ImeTerminate,
    ]) {
        native.register_service(*address, service)?;
    }
    for ((_, address), service) in keyboard_exports.iter().zip([
        Service::KeyboardInitialize,
        Service::KeyboardOpen,
        Service::KeyboardReadState,
        Service::KeyboardClose,
    ]) {
        native.register_service(*address, service)?;
    }
    for ((_, address), service) in video_exports.iter().zip([
        Service::VideoOpen,
        Service::VideoClose,
        Service::VideoSetFlipRate,
        Service::VideoSetBufferAttribute,
        Service::VideoRegisterBuffers,
        Service::VideoAddFlipEvent,
        Service::VideoDeleteFlipEvent,
        Service::VideoSubmitFlip,
    ]) {
        native.register_service(*address, service)?;
    }
    for ((_, address), service) in audio_exports.iter().zip([
        Service::AudioInitialize,
        Service::AudioOpen,
        Service::AudioOutput,
        Service::AudioClose,
    ]) {
        native.register_service(*address, service)?;
    }
    for ((_, address), service) in pad_exports.iter().zip([
        Service::PadInitialize,
        Service::PadOpen,
        Service::PadClose,
        Service::PadReadState,
        Service::PadSetLightBar,
        Service::PadSetVibration,
        Service::PadSetVibrationMode,
    ]) {
        native.register_service(*address, service)?;
    }
    native.register_service(0x760010, Service::Getargv)?;
    native.process_arguments = Some((1, 0x30004000));
    // Explicit bounded fixture stdout capture, separate from host terminal I/O.
    native.stdout.configure_capture(1024 * 1024)?;
    native.stdio.configure_capture(1024 * 1024)?;
    // This fixture supplies an empty guest environment and explicitly chooses
    // the C locale; host macOS locale is never consulted.
    native
        .locale
        .configure_empty_environment(native_ps5::guest_locale::EmptyEnvironmentPolicy::C);
    let sysmodule_exports = [("sceSysmoduleLoadModuleInternal", 0x750000)];
    let kernel_exports = [
        ("sceKernelAllocateMainDirectMemory", 0x7b0000),
        ("sceKernelMapDirectMemory", 0x7b0010),
        ("sceKernelReleaseDirectMemory", 0x7b0020),
        ("sceKernelCreateEqueue", 0x7b0030),
        ("sceKernelWaitEqueue", 0x7b0040),
        ("sceKernelDeleteEqueue", 0x7b0050),
        ("__error", 0x710020),
        ("getpid", 0x710000),
        ("sceKernelDlsym", 0x700020),
        ("sceKernelLoadStartModule", 0x740000),
        ("sceKernelStopUnloadModule", 0x740010),
        ("getargc", 0x760000),
        ("getargv", 0x760010),
        ("environ", 0x30004010),
    ];
    let mut libc_exports = vec![
        ("snprintf", 0x720000),
        ("strerror", 0x720010),
        ("vsnprintf", 0x720020),
        ("__isthreaded", 0x30003000),
        ("__stderrp", native_ps5::guest_stdio::STDERR_PTR_ADDR),
        ("__stdinp", native_ps5::stdio_file::STDIN_PTR_ADDR),
        ("__mb_cur_max", native_ps5::guest_locale::MB_CUR_MAX_ADDR),
        ("__stdoutp", native_ps5::stdout_file::STDOUT_PTR_ADDR),
    ];
    for (index, (name, service)) in [
        ("strcpy", Service::Strcpy),
        ("strcat", Service::Strcat),
        ("strcmp", Service::Strcmp),
        ("strncmp", Service::Strncmp),
        ("strlen", Service::Strlen),
        ("sprintf", Service::Sprintf),
        ("calloc", Service::Calloc),
        ("free", Service::Free),
        ("malloc", Service::Malloc),
        ("getenv", Service::Getenv),
        ("memcpy", Service::Memcpy),
        ("_Strerror", Service::InternalStrerror),
        ("strdup", Service::Strdup),
        ("memset", Service::Memset),
        ("setlocale", Service::Setlocale),
        ("puts", Service::Puts),
        ("exit", Service::Exit),
        ("_exit", Service::ImmediateExit),
        ("fprintf", Service::Fprintf),
        ("fflush", Service::Fflush),
        ("pthread_key_create", Service::TlsKeyCreate),
        ("pthread_getspecific", Service::TlsGetSpecific),
        ("pthread_setspecific", Service::TlsSetSpecific),
        ("pthread_key_delete", Service::TlsKeyDelete),
        ("realloc", Service::Realloc),
        ("pthread_mutexattr_init", Service::MutexAttrInit),
        ("pthread_mutexattr_settype", Service::MutexAttrSetType),
        ("pthread_mutex_init", Service::MutexInit),
        ("pthread_mutex_destroy", Service::MutexDestroy),
        ("pthread_mutex_lock", Service::MutexLock),
        ("pthread_mutex_trylock", Service::MutexTryLock),
        ("pthread_mutex_unlock", Service::MutexUnlock),
        ("pthread_mutexattr_destroy", Service::MutexAttrDestroy),
        ("pthread_attr_init", Service::ThreadAttrInit),
        (
            "pthread_attr_setdetachstate",
            Service::ThreadAttrSetDetachState,
        ),
        ("pthread_attr_setstacksize", Service::ThreadAttrSetStackSize),
        ("pthread_attr_destroy", Service::ThreadAttrDestroy),
        ("pthread_create", Service::ThreadCreate),
        ("pthread_self", Service::ThreadSelf),
        ("pthread_join", Service::ThreadJoin),
        ("pthread_detach", Service::ThreadDetach),
        ("pthread_set_name_np", Service::ThreadSetName),
        ("sigemptyset", Service::Sigemptyset),
        ("sigaddset", Service::Sigaddset),
        ("pthread_sigmask", Service::ThreadSigmask),
        ("pthread_setcanceltype", Service::ThreadSetCancelType),
        ("pthread_getschedparam", Service::ThreadGetschedparam),
        ("sched_get_priority_max", Service::SchedPriorityMax),
        ("sched_get_priority_min", Service::SchedPriorityMin),
        ("pthread_setschedparam", Service::ThreadSetschedparam),
        ("setenv", Service::Setenv),
        ("memmove", Service::Memmove),
        ("memcmp", Service::Memcmp),
        ("wcslen", Service::Wcslen),
        ("wcsstr", Service::Wcsstr),
        ("wcscmp", Service::Wcscmp),
        ("wcsncmp", Service::Wcsncmp),
        ("strlcpy", Service::Strlcpy),
        ("strlcat", Service::Strlcat),
        ("strchr", Service::Strchr),
        ("strrchr", Service::Strrchr),
        ("strstr", Service::Strstr),
        ("atoi", Service::Atoi),
        ("strtol", Service::Strtol),
        ("strtoul", Service::Strtoul),
        ("strtoll", Service::Strtoll),
        ("strtoull", Service::Strtoull),
        ("strncasecmp", Service::Strncasecmp),
        ("isalpha", Service::Ctype(Ctype::Isalpha)),
        ("isalnum", Service::Ctype(Ctype::Isalnum)),
        ("isxdigit", Service::Ctype(Ctype::Isxdigit)),
        ("ispunct", Service::Ctype(Ctype::Ispunct)),
        ("isspace", Service::Ctype(Ctype::Isspace)),
        ("isupper", Service::Ctype(Ctype::Isupper)),
        ("islower", Service::Ctype(Ctype::Islower)),
        ("isprint", Service::Ctype(Ctype::Isprint)),
        ("isgraph", Service::Ctype(Ctype::Isgraph)),
        ("iscntrl", Service::Ctype(Ctype::Iscntrl)),
        ("toupper", Service::Ctype(Ctype::Toupper)),
        ("tolower", Service::Ctype(Ctype::Tolower)),
        ("isblank", Service::Ctype(Ctype::Isblank)),
        ("atof", Service::Atof),
        ("strtod", Service::Strtod),
        ("vsscanf", Service::Vsscanf),
        ("gettimeofday", Service::Gettimeofday),
        ("nanosleep", Service::Nanosleep),
        ("_Exit", Service::ImmediateExit),
        ("fopen", Service::File(FileOperation::Open)),
        ("fread", Service::File(FileOperation::Read)),
        ("fseek", Service::File(FileOperation::Seek)),
        ("ftell", Service::File(FileOperation::Tell)),
        ("fclose", Service::File(FileOperation::Close)),
        ("fileno", Service::File(FileOperation::Fileno)),
        ("feof", Service::File(FileOperation::Eof)),
        ("ferror", Service::File(FileOperation::Error)),
        ("clearerr", Service::File(FileOperation::Clearerr)),
        ("fseeko", Service::File(FileOperation::Seek)),
        ("ftello", Service::File(FileOperation::Tell)),
        ("fstat", Service::Fstat),
        ("fwrite", Service::File(FileOperation::Write)),
        ("sem_init", Service::SemInit),
        ("sem_destroy", Service::SemDestroy),
        ("sem_trywait", Service::SemTryWait),
        ("sem_wait", Service::SemWait),
        ("sem_timedwait", Service::SemTimedWait),
        ("sem_getvalue", Service::SemGetValue),
        ("sem_post", Service::SemPost),
        ("stat", Service::Stat),
        ("sysconf", Service::Sysconf),
        ("sysctlbyname", Service::SysctlByName),
        ("sysctl", Service::Sysctl),
        ("sigaction", Service::Sigaction),
        ("signal", Service::Signal),
        ("wcstombs", Service::Wcstombs),
        ("qsort", Service::Qsort),
        ("bsearch", Service::Bsearch),
        ("fgets", Service::Fgets),
        ("getcwd", Service::Getcwd),
        ("mkdir", Service::Mkdir),
        ("pthread_cond_init", Service::CondInit),
        ("pthread_cond_destroy", Service::CondDestroy),
        ("pthread_cond_signal", Service::CondSignal),
        ("pthread_cond_broadcast", Service::CondBroadcast),
        ("pthread_cond_timedwait", Service::CondTimedWait),
        ("pthread_cond_wait", Service::CondWait),
        ("mbrtowc", Service::Mbrtowc),
        ("wcrtomb", Service::Wcrtomb),
        ("mbsrtowcs", Service::Mbsrtowcs),
        ("abort", Service::Abort),
        ("vasprintf", Service::Vasprintf),
        ("strcoll", Service::Strcoll),
        ("strxfrm", Service::Strxfrm),
        ("wcscoll", Service::Wcscoll),
        ("strftime", Service::Strftime),
        ("strtof", Service::Strtof),
        ("strtold", Service::Strtold),
        ("wcsxfrm", Service::Wcsxfrm),
        ("mbrlen", Service::Mbrlen),
        ("mbtowc", Service::Mbtowc),
        ("strtok_r", Service::StrtokR),
        ("putc", Service::Fputc),
        ("__swbuf", Service::Swbuf),
        ("fputc", Service::Fputc),
        ("putchar", Service::Putchar),
        ("printf", Service::Printf),
        ("vprintf", Service::Vprintf),
        ("vfprintf", Service::Vfprintf),
        ("remove", Service::Remove),
        ("rename", Service::Rename),
        ("strncpy", Service::Strncpy),
        ("sscanf", Service::Sscanf),
        ("opendir", Service::Opendir),
        ("readdir", Service::Readdir),
        ("closedir", Service::Closedir),
        ("putenv", Service::Putenv),
        ("fgetc", Service::File(native_ps5::stdio_file::Operation::ReadByte)),
    ]
    .into_iter()
    .enumerate()
    {
        let address = 0x730000 + index as u64 * 16;
        native.register_service(address, service)?;
        libc_exports.push((name, address));
    }
    // SAFETY: local archive is this exact runtime build, supplied by the developer.
    let manifest = unsafe {
        native.set_automatic_translation(true);
        native.compile_image(
            &file,
            bias,
            // Address-taken and jump-table targets are compiled only when
            // encountered during actual execution, preserving suspended state.
            BTreeSet::from([0x40]),
            65_536,
            Path::new(&archive),
            Path::new(&directory),
        )?
    };
    if manifest.sha256 != "308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567" {
        return Err("Real-entry probe addresses require the pinned Crispy Doom ELF".into());
    }
    println!(
        "Initially compiled {} instructions; {} unsupported boundaries",
        manifest.instruction_count,
        manifest.untranslated.len()
    );
    let begin = bias + 0x58f780;
    let end = bias + 0x80b2c0;
    let bss = image
        .segments
        .iter_mut()
        .find(|s| begin >= s.address && end <= s.address + s.bytes.len() as u64)
        .ok_or("Missing BSS span")?;
    bss.bytes[(begin - bss.address) as usize..(end - bss.address) as usize].fill(0xa5);
    let mut kernel = native_ps5::kernel::Kernel::new(65536, 0x30001000)?;
    kernel.files.set_stat_tracing(true);
    // Explicit redirected empty input for this startup fixture, not implicit terminal EOF.
    kernel.files.register(b"/fixture/stdin", Vec::new())?;
    kernel.register_sandbox_component(b"guest_sandbox")?;
    kernel.register_working_directory(b"/app0")?;
    kernel.register_process_name(std::process::id(), b"crispy-doom.elf")?;
    let control = 0xffff800006100000;
    kernel.map(control, vec![0; 64], true)?;
    // Explicit 7.00 model, matching the retained payload environment. Only
    // implemented function thunks are published in the kernel module record.
    let base = 0xffff800000000000u64;
    let process = base + 0x4000000;
    kernel.map(base + 0x2859d50, process.to_le_bytes().to_vec(), true)?;
    kernel.map(
        process,
        native_ps5::payload::ProcessRecord::new(
            std::process::id(),
            0,
            base + 0x6000000,
            base + 0x5000000,
        )?
        .bytes
        .to_vec(),
        true,
    )?;
    kernel.map(
        base + 0x6000000,
        native_ps5::payload::CredentialRecord::new(
            base + 0x6200000,
            native_ps5::payload::FIXTURE_ATTRIBUTES,
        )
        .bytes
        .to_vec(),
        true,
    )?;
    kernel.map(base + 0x6200000, vec![0; 64], true)?;
    let mut container = native_ps5::payload::ModuleContainer::default();
    container.bytes[..8].copy_from_slice(&(base + 0x51a0000).to_le_bytes());
    kernel.map(base + 0x5000000, container.bytes.to_vec(), true)?;
    let kernel_nids = encode_exports(&kernel_exports)?;
    let mut process_arguments = [0u8; 64];
    process_arguments[..8].copy_from_slice(&0x30004020u64.to_le_bytes());
    process_arguments[32..48].copy_from_slice(b"crispy-doom.elf\0");
    native.environment = 0x30004010;
    let module = {
        use native_ps5::kernel_module::{Export, Kind};
        let mut user = native_ps5::guest_memory::Memory::default();
        user.add(0x30004000, &mut process_arguments, true, false)?;
        let exports = kernel_nids
            .iter()
            .map(|(nid, address)| Export {
                nid,
                address: *address,
                kind: if *address == 0x30004010 {
                    Kind::Object {
                        size: 8,
                        writable: true,
                    }
                } else {
                    Kind::ServiceEntry
                },
            })
            .collect::<Vec<_>>();
        native_ps5::kernel_module::ModuleRecord::build(
            base + 0x5100000,
            0,
            1,
            0x6ff000,
            &exports,
            &user,
            |address| {
                address != 0x30004010 && kernel_exports.iter().any(|(_, target)| *target == address)
            },
        )?
    };
    let module = module.with_path(base + 0x5100000, "/system/common/lib/libkernel_web.sprx")?;
    kernel.map(base + 0x5100000, module.bytes, true)?;
    let mut isthreaded = [0u8; 4];
    let mut stderr_pointer = native_ps5::guest_stdio::stderr_pointer_bytes();
    let mut stdin_pointer = native_ps5::stdio_file::stdin_pointer_bytes();
    let mut mb_cur_max = 1i32.to_le_bytes();
    let mut stdout_pointer = native_ps5::stdout_file::FILE_ADDR.to_le_bytes();
    let libc = {
        use native_ps5::kernel_module::{Export, Kind};
        let mut user = native_ps5::guest_memory::Memory::default();
        user.add(0x30003000, &mut isthreaded, true, false)?;
        user.add(native_ps5::guest_locale::MB_CUR_MAX_ADDR, &mut mb_cur_max, true, false)?;
        user.add(native_ps5::stdout_file::STDOUT_PTR_ADDR, &mut stdout_pointer, true, false)?;
        user.add(
            native_ps5::guest_stdio::STDERR_PTR_ADDR,
            &mut stderr_pointer,
            false,
            false,
        )?;
        user.add(native_ps5::stdio_file::STDIN_PTR_ADDR, &mut stdin_pointer, false, false)?;
        let nids = encode_exports(&libc_exports)?;
        let exports = nids
            .iter()
            .map(|(nid, address)| Export {
                nid,
                address: *address,
                kind: if *address == 0x30003000 || *address == native_ps5::guest_locale::MB_CUR_MAX_ADDR {
                    Kind::Object {
                        size: 4,
                        writable: true,
                    }
                } else if *address == native_ps5::stdout_file::STDOUT_PTR_ADDR {
                    Kind::Object { size: 8, writable: true }
                } else if *address == native_ps5::guest_stdio::STDERR_PTR_ADDR
                    || *address == native_ps5::stdio_file::STDIN_PTR_ADDR {
                    Kind::Object {
                        size: 8,
                        writable: false,
                    }
                } else {
                    Kind::ServiceEntry
                },
            })
            .collect::<Vec<_>>();
        native_ps5::kernel_module::ModuleRecord::build(
            base + 0x5110000,
            base + 0x5100000,
            2,
            0x71f000,
            &exports,
            &user,
            |address| {
                address != 0x30003000
                    && address != native_ps5::guest_locale::MB_CUR_MAX_ADDR
                    && address != native_ps5::stdout_file::STDOUT_PTR_ADDR
                    && address != native_ps5::guest_stdio::STDERR_PTR_ADDR
                    && address != native_ps5::stdio_file::STDIN_PTR_ADDR
                    && libc_exports.iter().any(|(_, target)| *target == address)
            },
        )?
    };
    let libc = libc.with_path(
        base + 0x5110000,
        "/system/common/lib/libSceLibcInternal.sprx",
    )?;
    kernel.map(base + 0x5110000, libc.bytes, true)?;
    let sysmodule_nids = encode_exports(&sysmodule_exports)?;
    let exports = sysmodule_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let sysmodule = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5120000,
        base + 0x5110000,
        3,
        0x74f000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| address == 0x750000,
    )?;
    // Reserve pathname backing now, but publish only once the provider is
    // registered/started after the explicit bootstrap load request.
    let mut sysmodule =
        sysmodule.with_path(base + 0x5120000, "/system/common/lib/libSceSysmodule.sprx")?;
    let sysmodule_path_pointer = u64::from_le_bytes(sysmodule.bytes[8..16].try_into().unwrap());
    sysmodule.bytes[8..16].fill(0);
    kernel.map(base + 0x5120000, sysmodule.bytes, true)?;
    let system_nids = encode_exports(&system_service_exports)?;
    let exports = system_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let system = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5130000,
        base + 0x5120000,
        4,
        0x76f000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| matches!(address, 0x770000 | 0x770010),
    )?
    .with_path(
        base + 0x5130000,
        "/system/common/lib/libSceSystemService.sprx",
    )?;
    kernel.map(base + 0x5130000, system.bytes, true)?;
    let user_nids = encode_exports(&user_service_exports)?;
    let exports = user_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let user = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5140000,
        base + 0x5130000,
        5,
        0x77f000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| matches!(address, 0x780000 | 0x780010 | 0x780020 | 0x780030),
    )?
    .with_path(
        base + 0x5140000,
        "/system/common/lib/libSceUserService.sprx",
    )?;
    kernel.map(base + 0x5140000, user.bytes, true)?;
    let pad_nids = encode_exports(&pad_exports)?;
    let exports = pad_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let pad = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5150000,
        base + 0x5140000,
        6,
        0x78f000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| pad_exports.iter().any(|(_, target)| *target == address),
    )?
    .with_path(base + 0x5150000, "/system/common/lib/libScePad.sprx")?;
    kernel.map(base + 0x5150000, pad.bytes, true)?;
    let audio_nids = encode_exports(&audio_exports)?;
    let exports = audio_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let audio = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5160000,
        base + 0x5150000,
        7,
        0x79f000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| audio_exports.iter().any(|(_, target)| *target == address),
    )?
    .with_path(base + 0x5160000, "/system/common/lib/libSceAudioOut.sprx")?;
    kernel.map(base + 0x5160000, audio.bytes, true)?;
    let video_nids = encode_exports(&video_exports)?;
    let exports = video_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let video = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5170000,
        base + 0x5160000,
        8,
        0x7bf000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| video_exports.iter().any(|(_, target)| *target == address),
    )?
    .with_path(base + 0x5170000, "/system/common/lib/libSceVideoOut.sprx")?;
    kernel.map(base + 0x5170000, video.bytes, true)?;
    let keyboard_nids = encode_exports(&keyboard_exports)?;
    let exports = keyboard_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let keyboard = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5180000,
        base + 0x5170000,
        9,
        0x7cf000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| {
            keyboard_exports
                .iter()
                .any(|(_, target)| *target == address)
        },
    )?
    .with_path(base + 0x5180000, "/system/common/lib/libSceKeyboard.sprx")?;
    kernel.map(base + 0x5180000, keyboard.bytes, true)?;
    let ime_nids = encode_exports(&ime_exports)?;
    let exports = ime_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let ime = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x5190000,
        base + 0x5180000,
        10,
        0x7df000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| ime_exports.iter().any(|(_, target)| *target == address),
    )?
    .with_path(base + 0x5190000, "/system/common/lib/libSceImeDialog.sprx")?;
    kernel.map(base + 0x5190000, ime.bytes, true)?;
    let net_nids = encode_exports(&net_exports)?;
    let exports = net_nids
        .iter()
        .map(|(nid, address)| native_ps5::kernel_module::Export {
            nid,
            address: *address,
            kind: native_ps5::kernel_module::Kind::ServiceEntry,
        })
        .collect::<Vec<_>>();
    let net = native_ps5::kernel_module::ModuleRecord::build(
        base + 0x51a0000,
        base + 0x5190000,
        11,
        0x7ef000,
        &exports,
        &native_ps5::guest_memory::Memory::default(),
        |address| net_exports.iter().any(|(_, target)| *target == address),
    )?
    .with_path(base + 0x51a0000, "/system/common/lib/libSceNet.sprx")?;
    kernel.map(base + 0x51a0000, net.bytes, true)?;
    let (selector, writer) = kernel.descriptors.packet_control_pair()?;
    let (reader, pipe_writer) = kernel.kernel_pipe(control)?;
    let mut payload = native_ps5::payload::BootstrapPayload::new(
        0x30000000,
        0x700020,
        [reader, pipe_writer, selector, writer],
        control,
        0xffff800000000000,
    )?;
    let mut parameters = [0u8; 24];
    parameters[..8].copy_from_slice(&24u64.to_le_bytes());
    parameters[20..].copy_from_slice(&0x07000000u32.to_le_bytes());
    {
        let mut memory = native_ps5::guest_memory::Memory::default();
        memory.add(0x30002000, &mut parameters, false, false)?;
        kernel.register_process_parameters(&memory, 2, 0x30002000)?;
    }
    native.kernel = Some(kernel);
    native.configure_stdin(b"/fixture/stdin")?;
    let mut errno = [0u8; 4];
    let mut regions = [
        DataRegion {
            address: 0x30004000,
            bytes: &mut process_arguments,
            writable: true,
        },
        DataRegion {
            address: 0x30003000,
            bytes: &mut isthreaded,
            writable: true,
        },
        DataRegion {
            address: 0x30002000,
            bytes: &mut parameters,
            writable: false,
        },
        DataRegion {
            address: payload.address,
            bytes: &mut payload.bytes,
            writable: true,
        },
        DataRegion {
            address: 0x30001000,
            bytes: &mut errno,
            writable: true,
        },
    ];
    let outcome = native.run_with_data(
        &mut image,
        bias + 0x40,
        Arguments {
            size: 0x30000000,
            pointer: 0,
        },
        &mut [],
        &mut regions,
    )?;
    let Execution::Dlsym {
        arguments,
        mut continuation,
    } = outcome
    else {
        return Err("Expected real bootstrap dlsym boundary".into());
    };
    let symbol = continuation.with_memory_data(&mut image, &mut [], &mut regions, |memory| {
        let mut bytes = Vec::new();
        for offset in 0..256 {
            let b = memory.load(arguments[1] + offset, 1)? as u8;
            if b == 0 {
                break;
            }
            bytes.push(b);
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
    })?;
    println!(
        "Real entry reached dlsym: handle={:#x} symbol={symbol:?} output={:#x}",
        arguments[0], arguments[2]
    );
    if symbol != "sceKernelDlsym" {
        return Err("Unexpected bootstrap symbol".into());
    }
    let bss = image
        .segments
        .iter()
        .find(|s| begin >= s.address && end <= s.address + s.bytes.len() as u64)
        .unwrap();
    let mut expected = vec![0; (end - begin) as usize];
    expected[..8].copy_from_slice(&0x30000000u64.to_le_bytes());
    if bss.bytes[(begin - bss.address) as usize..(end - bss.address) as usize] != expected {
        return Err("BSS differs from cleared storage plus saved payload pointer".into());
    }
    // Resolve implemented services only. Other kernel calls remain explicit
    // unsupported boundaries instead of reporting synthetic success.
    continuation.with_memory_data(&mut image, &mut [], &mut regions, |memory| {
        memory.store(arguments[2], 0x700020, 8)
    })?;
    let next = native.resume_with_data(&mut image, &mut [], *continuation, 0, &mut regions)?;
    let Execution::Dlsym {
        arguments,
        mut continuation,
    } = next
    else {
        return Err("Expected second bootstrap lookup".into());
    };
    continuation.with_memory_data(&mut image, &mut [], &mut regions, |memory| {
        if arguments[0] != 1 || memory.read(arguments[1], 7)? != b"getpid\0" {
            return Err("Unexpected second bootstrap symbol".into());
        }
        memory.store(arguments[2], 0x710000, 8)?;
        Ok(())
    })?;
    let outcome = native.resume_with_data(&mut image, &mut [], *continuation, 0, &mut regions);
    let (arguments, continuation) = match outcome {
        Ok(Execution::LoadStartModule {
            arguments,
            mut continuation,
        }) => {
            let request = native.with_suspended_memory(
                &mut continuation,
                &mut image,
                &mut [],
                &mut regions,
                |memory| native_ps5::module_executor::LoadRequest::read(memory, arguments),
            )?;
            if request.path != "/system/common/lib/libSceSysmodule.sprx"
                || request.arguments.size != 0
                || request.arguments.pointer != 0
                || request.result_address != 0
            {
                return Err(format!("Unexpected real module request: {}", request.path));
            }
            println!(
                "NEXT RUST ENTRY: load/start {} (no arguments); request suspended for native provider binding",
                request.path
            );
            (arguments, continuation)
        }
        Err(error) => return Err(format!("Unexpected real-entry boundary: {error}")),
        Ok(_) => return Err("Unexpected bootstrap continuation outcome".into()),
    };
    let process_data = regions
        .iter()
        .map(|r| native_ps5::modules::ProcessData {
            address: r.address,
            bytes: r.bytes.to_vec(),
            writable: r.writable,
        })
        .collect();
    drop(regions);
    let slot = bias + 5830800;
    if isthreaded != 1u32.to_le_bytes() {
        return Err("Guest startup did not write __isthreaded".into());
    }
    if native.bytes_allocated() != 0 {
        return Err("Resolver leaked anonymous guest mappings".into());
    }
    for (offset, expected) in [
        (0x58ff60, 0x730000),
        (0x58ff90, 0x730010),
        (0x58ffe8, 0x730020),
        (0x590008, 0x730030),
        (0x590018, 0x730040),
        (0x58ff80, 0x730050),
        (0x58ff98, 0x730060),
        (0x58ffc0, 0x730070),
        (0x58ff88, 0x730090),
        (0x590040, 0x740000),
        (0x590048, 0x740010),
        (0x590028, 0x730080),
        (0x58f8a0, 0x720000),
        (0x58f8a8, 0x720010),
        (0x58f898, 0x720020),
        (0x58f8b0, 0x710020),
        (0x58f8b8, 0x710020),
        (0x58f8d0, base + 0x2859d50),
        (0x58f8e8, base + 0x6200000),
    ] {
        let address = bias + offset;
        let segment = image
            .segments
            .iter()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing kernel state slot")?;
        let start = (address - segment.address) as usize;
        let actual = u64::from_le_bytes(segment.bytes[start..start + 8].try_into().unwrap());
        if actual != expected {
            return Err(format!(
                "Kernel state {offset:#x}: expected {expected:#x}, found {actual:#x}"
            ));
        }
    }
    let segment = image
        .segments
        .iter()
        .find(|s| slot >= s.address && slot + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing syscall slot")?;
    let offset = (slot - segment.address) as usize;
    if segment.bytes[offset..offset + 8] != 0x71000au64.to_le_bytes() {
        return Err("Real bootstrap did not install the callable syscall entry".into());
    }
    let mut modules = native_ps5::modules::Modules::new(64 * 1024 * 1024);
    if modules.register_native("libkernel.prx", &kernel_exports)? != 1
        || modules.register_native("libc.prx", &libc_exports)? != 2
    {
        return Err("Manager handles disagree with guest kernel records".into());
    }
    modules.bind_native_path("/system/common/lib/libkernel_web.sprx", 1)?;
    modules.bind_native_path("/system/common/lib/libSceLibcInternal.sprx", 2)?;
    for (owner, exports) in [
        ("libkernel.prx", kernel_exports.as_slice()),
        ("libc.prx", libc_exports.as_slice()),
    ] {
        for &(name, address) in exports {
            let nid = native_ps5::strings::nid(name);
            let nid = std::str::from_utf8(&nid[..11]).map_err(|e| e.to_string())?;
            for token in [name, nid] {
                let mut identity = native_ps5::imports::identify(token, &Default::default(), &[])?;
                identity.module = owner.into();
                if modules.resolve(&identity) != Some(address) {
                    return Err(format!(
                        "Manager cannot resolve bootstrap export {owner}:{token}"
                    ));
                }
            }
        }
    }
    let sysmodule = modules.register_native("libSceSysmodule.prx", &sysmodule_exports)?;
    if sysmodule != 3 {
        return Err("Unexpected native sysmodule handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceSysmodule.sprx", sysmodule)?;
    // The provider is now Started; name publication must precede continuation
    // so later basename traversal can pass it and reach the libc record.
    native
        .kernel
        .as_mut()
        .ok_or("Missing kernel owner")?
        .publish_module_path(base + 0x5120000, sysmodule_path_pointer)?;
    let system = modules.register_native("libSceSystemService.prx", &system_service_exports)?;
    if system != 4 {
        return Err("Unexpected native SystemService handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceSystemService.sprx", system)?;
    modules.bind_sysmodule(0x80000010, system)?;
    let user = modules.register_native("libSceUserService.prx", &user_service_exports)?;
    if user != 5 {
        return Err("Unexpected native UserService handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceUserService.sprx", user)?;
    modules.bind_sysmodule(0x80000011, user)?;
    let pad = modules.register_native("libScePad.prx", &pad_exports)?;
    if pad != 6 {
        return Err("Unexpected native Pad handle".into());
    }
    modules.bind_native_path("/system/common/lib/libScePad.sprx", pad)?;
    let audio = modules.register_native("libSceAudioOut.prx", &audio_exports)?;
    if audio != 7 {
        return Err("Unexpected native AudioOut handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceAudioOut.sprx", audio)?;
    let video = modules.register_native("libSceVideoOut.prx", &video_exports)?;
    if video != 8 {
        return Err("Unexpected native VideoOut handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceVideoOut.sprx", video)?;
    let keyboard = modules.register_native("libSceKeyboard.prx", &keyboard_exports)?;
    if keyboard != 9 {
        return Err("Unexpected native Keyboard handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceKeyboard.sprx", keyboard)?;
    let ime = modules.register_native("libSceImeDialog.prx", &ime_exports)?;
    if ime != 10 {
        return Err("Unexpected native IME handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceImeDialog.sprx", ime)?;
    let net = modules.register_native("libSceNet.prx", &net_exports)?;
    if net != 11 {
        return Err("Unexpected native Net handle".into());
    }
    modules.bind_native_path("/system/common/lib/libSceNet.sprx", net)?;
    for (module, exports) in [
        ("libSceSystemService.prx", system_service_exports.as_slice()),
        ("libSceUserService.prx", user_service_exports.as_slice()),
        ("libScePad.prx", pad_exports.as_slice()),
        ("libSceAudioOut.prx", audio_exports.as_slice()),
        ("libSceVideoOut.prx", video_exports.as_slice()),
        ("libSceKeyboard.prx", keyboard_exports.as_slice()),
        ("libSceImeDialog.prx", ime_exports.as_slice()),
        ("libSceNet.prx", net_exports.as_slice()),
    ] {
        for &(name, address) in exports {
            let nid = native_ps5::strings::nid(name);
            for token in [
                name,
                std::str::from_utf8(&nid[..11]).map_err(|e| e.to_string())?,
            ] {
                let mut identity = native_ps5::imports::identify(token, &Default::default(), &[])?;
                identity.module = module.into();
                if modules.resolve(&identity) != Some(address) {
                    return Err(format!(
                        "{module} registry differs from published kernel exports"
                    ));
                }
            }
        }
    }
    let caller = modules.adopt_bootstrap(
        "crispy-doom.elf",
        native_ps5::modules::Bootstrap {
            image,
            continuation,
            arguments,
            data: process_data,
        },
        &mut native,
    )?;
    let before = modules.bytes();
    if modules.unload(caller).is_ok() {
        return Err("Bootstrap root unexpectedly unloadable".into());
    }
    // SAFETY: same trusted runtime archive; explicit native provider has its
    // working Rust export registered above. No system ELF is fabricated.
    let result = unsafe {
        modules.dispatch_native_load(
            caller,
            &mut native,
            &mut native_ps5::module_files::ModuleFiles::default(),
            native_ps5::modules::LoadConfig {
                bias: 0x40000000,
                instruction_limit: 20_000,
                archive: Path::new(&archive),
                directory: Path::new(&directory),
            },
        )
    };
    let guest_logs = native.kernel.as_mut().unwrap().take_logs();
    println!(
        "Observed translation rebuilds: {:?}; roots: {:x?}",
        native.translation_rebuilds(bias),
        native.translation_roots(bias)
    );
    let stat_trace = native.kernel.as_mut().unwrap().files.take_stat_trace();
    for request in &stat_trace.requests {
        println!(
            "GUEST STAT: {:?} -> {:?}",
            String::from_utf8_lossy(&request.path),
            request.error
        );
    }
    for message in &guest_logs {
        println!("GUEST LOG: {}", String::from_utf8_lossy(&message));
    }
    match result {
        Ok(native_ps5::modules::LoadProgress::Caller {
            event: native_ps5::modules::StartEvent::SysmoduleLoadInternal { arguments },
        }) => {
            println!(
                "NEXT RUST ENTRY: actual sysmodule ID {:#x}",
                arguments[0] as u32
            );
            if modules
                .dispatch_native_sysmodule(caller, &mut native)
                .is_ok()
            {
                return Err("Missing sysmodule provider unexpectedly succeeded".into());
            }
        }
        Ok(native_ps5::modules::LoadProgress::Caller {
            event: native_ps5::modules::StartEvent::ExitRequested(request),
        }) => {
            println!("Actual startup exit request: {request:?}; cleanup remains pending");
            if native.exit_request() != Some(request) || modules.exit_request() != Some(request) {
                return Err("Startup exit request ownership mismatch".into());
            }
            if request.status != -1 || request.kind != native_ps5::module_executor::ExitKind::Normal
            {
                return Err("Unexpected startup failure exit status/kind".into());
            }
            let expected_paths = [
                "/system/priv/lib/libSceSystemService.sprx",
                "/system/common/lib/libSceSystemService.sprx",
                "/system_ex/priv_ex/lib/libSceSystemService.sprx",
                "/system_ex/common_ex/lib/libSceSystemService.sprx",
                "/guest_sandbox/priv/lib/libSceSystemService.sprx",
                "/guest_sandbox/common/lib/libSceSystemService.sprx",
                "/guest_sandbox/priv_ex/lib/libSceSystemService.sprx",
                "/guest_sandbox/common_ex/lib/libSceSystemService.sprx",
                "/user/homebrew/lib/libSceSystemService.sprx",
                "/app0/libSceSystemService.sprx",
            ];
            if stat_trace.dropped != 0
                || stat_trace.requests.len() != expected_paths.len() * 10
                || stat_trace
                    .requests
                    .iter()
                    .enumerate()
                    .zip(expected_paths.into_iter().cycle())
                    .any(|((index, request), path)| {
                        let soname = [
                            "libSceSystemService.sprx",
                            "libSceUserService.sprx",
                            "libScePad.sprx",
                            "libSceAudioOut.sprx",
                            "libSceVideoOut.sprx",
                            "libSceKeyboard.sprx",
                            "libSceImeDialog.sprx",
                            "libkernel_web.sprx",
                            "libSceLibcInternal.sprx",
                            "libSceNet.sprx",
                        ][index / 10];
                        let path = path.replace("libSceSystemService.sprx", soname);
                        request.path != path.as_bytes() || request.error != Some(2)
                    })
            {
                return Err(format!(
                    "Unexpected dependency lookup trace: {stat_trace:?}"
                ));
            }
            let roots = native
                .translation_roots(bias)
                .ok_or("Missing translation roots")?;
            if native.translation_rebuilds(bias) != Some(12)
                || roots.len() != 13
                || !roots.contains(&0x6da0)
                || !roots.contains(&0x67e0)
                || !roots.contains(&0x7340)
                || roots.contains(&0x373)
            {
                return Err("Actual observed-target recovery did not match the pinned ELF".into());
            }
            let image = modules.image(caller).ok_or("Missing adopted image")?;
            let locale_slot = bias + 0x26f6a0;
            let locale_segment = image
                .segments
                .iter()
                .find(|s| {
                    locale_slot >= s.address
                        && locale_slot + 1160 <= s.address + s.bytes.len() as u64
                })
                .ok_or("Missing locale import slot")?;
            let locale_offset = (locale_slot - locale_segment.address) as usize;
            if locale_segment.bytes[locale_offset..locale_offset + 8] != 0x7300e0u64.to_le_bytes() {
                return Err("Actual guest resolver did not bind setlocale".into());
            }
            if locale_segment.bytes[locale_offset + 8..locale_offset + 16]
                != 0x7300f0u64.to_le_bytes()
            {
                return Err("Actual guest resolver did not bind puts".into());
            }
            if locale_segment.bytes[locale_offset + 16..locale_offset + 24]
                != 0x730100u64.to_le_bytes()
            {
                return Err("Actual guest resolver did not bind exit".into());
            }
            for (delta, expected) in [
                (24, native_ps5::guest_stdio::STDERR_PTR_ADDR),
                (32, 0x730120),
                (40, 0x730130),
                (56, 0x730140),
                (64, 0x730150),
                (72, 0x730160),
                (80, 0x730170),
                (96, 0x730180),
                (112, 0x730190),
                (120, 0x7301a0),
                (128, 0x7301b0),
                (136, 0x7301c0),
                (144, 0x7301d0),
                (152, 0x7301e0),
                (160, 0x7301f0),
                (168, 0x730210),
                (176, 0x730220),
                (184, 0x730230),
                (192, 0x730250),
                (200, 0x730260),
                (208, 0x730290),
                (216, 0x7302a0),
                (224, 0x7302b0),
                (232, 0x7302c0),
                (240, 0x7302d0),
                (248, 0x7302e0),
                (256, 0x7302f0),
                (264, 0x730300),
                (272, 0x730310),
                (296, 0x730320),
                (312, 0x730330),
                (320, 0x730340),
                (336, 0x730350),
                (344, 0x730360),
                (352, 0x730370),
                (360, 0x730380),
                (368, 0x730390),
                (376, 0x7303a0),
                (384, 0x7303b0),
                (392, 0x7303c0),
                (400, 0x7303d0),
                (408, 0x7303e0),
                (416, 0x730510),
                (424, 0x7303f0),
                (432, 0x730400),
                (440, 0x730410),
                (448, 0x730420),
                (456, 0x730520),
                (480, 0x730430),
                (488, 0x730530),
                (504, 0x730440),
                (512, 0x730450),
                (520, 0x730460),
                (528, 0x730470),
                (536, 0x730480),
                (544, 0x730490),
                (552, 0x7304a0),
                (560, 0x7304b0),
                (568, 0x7304c0),
                (576, 0x7304d0),
                (584, 0x7304e0),
                (592, 0x7304f0),
                (608, 0x730500),
                (616, 0x730540),
                (632, 0x730550),
                (640, 0x730560),
                (648, 0x730570),
                (656, 0x30003000),
                (664, 0x7305c0),
                (672, 0x730620),
                (680, 0x720010),
                (688, 0x7305b0),
                (696, 0x730600),
                (704, 0x730610),
                (712, 0x730580),
                (720, 0x7305e0),
                (728, 0x730630),
                (736, 0x730640),
                (744, 0x730650),
                (752, 0x730660),
                (760, 0x730670),
                (768, 0x730680),
                (776, 0x730690),
                (784, 0x7306a0),
                (792, 0x7306b0),
                (800, 0x730050),
                (808, 0x7306c0),
                (816, 0x7306d0),
                (824, 0x7306e0),
                (832, 0x7306f0),
                (840, 0x730700),
                (848, 0x7b0000),
                (856, 0x7b0010),
                (864, 0x7b0030),
                (872, 0x7b0020),
                (880, 0x7b0050),
                (888, 0x7b0040),
                (896, 0x730710),
                (904, 0x730720),
                (912, 0x730730),
                (920, native_ps5::stdio_file::STDIN_PTR_ADDR),
                (928, 0x730740),
                (936, 0x760010),
                (944, 0x730750),
                (952, 0x730000),
                (960, 0x7300c0),
                (968, 0x730010),
                (976, 0x730760),
                (984, 0x730770),
                (992, 0x730780),
                (1000, 0x730790),
                (1008, 0x7307a0),
                (1016, 0x7307b0),
                (1024, 0x7307c0),
                (1032, 0x7307d0),
            ] {
                if locale_segment.bytes[locale_offset + delta..locale_offset + delta + 8]
                    != expected.to_le_bytes()
                {
                    return Err(format!("Actual libc import slot {delta} not bound"));
                }
            }
            for (delta, name) in [
                (1080, "strftime"),
                (1088, "strtof"),
                (1096, "strtold"),
                (1104, "strxfrm"),
                (1112, "wcscoll"),
                (1120, "wcsxfrm"),
                (1128, "mbrlen"),
                (1136, "mbtowc"),
                (1144, "__mb_cur_max"),
                (1152, "strtok_r"),
                (1160, "printf"),
                (1168, "__stdoutp"),
                (1176, "putc"),
                (1184, "__swbuf"),
                (1192, "vfprintf"),
                (1200, "remove"),
                (1208, "rename"),
                (1216, "ftell"),
                (1224, "fseek"),
                (1232, "strncpy"),
                (1240, "sscanf"),
                (1272, "opendir"),
                (1280, "closedir"),
                (1288, "readdir"),
                (1296, "putenv"),
                (1328, "fgetc"),
            ] {
                let expected = libc_exports.iter().find(|(symbol, _)| *symbol == name)
                    .ok_or("Missing registered libc service")?.1;
                if locale_segment.bytes[locale_offset + delta..locale_offset + delta + 8]
                    != expected.to_le_bytes()
                {
                    return Err(format!("Actual libc import {name} not bound"));
                }
            }
            let address = bias + 0x590050;
            let segment = image
                .segments
                .iter()
                .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
                .ok_or("Missing sysmodule export slot")?;
            let index = (address - segment.address) as usize;
            for (offset, expected) in [
                (0x590030, 0x7300a0u64),
                (0x590038, 0x7300a0),
                (0x58ffb8, 0x730060),
                (0x590080, 0x7300b0),
            ] {
                let index = (bias + offset - segment.address) as usize;
                if segment.bytes[index..index + 8] != expected.to_le_bytes() {
                    return Err(format!("Unexpected resolved linker slot {offset:#x}"));
                }
            }
            if segment.bytes[index..index + 8] != 0x750000u64.to_le_bytes()
                // 164 libc exports: 165*24=3960 symbol bytes (rounded3968)
                // and 1+164*12=1969 NID bytes (rounded1984).
                || native.bytes_allocated() != 20288
                || native.bytes_allocated() > 32 * 1024
                || guest_logs
                    != [b"<118>[crispy-doom.elf] Unable to resolve 'fscanf'\n".to_vec()]
            {
                return Err(format!(
                    "Startup state: sysmodule slot {:#x}, allocated {}, logs {:?}",
                    u64::from_le_bytes(segment.bytes[index..index + 8].try_into().unwrap()),
                    native.bytes_allocated(),
                    guest_logs
                ));
            }
            println!(
                "NEXT RUST ENTRY: all ten named DT_NEEDED providers accepted; fgetc import bound; unresolved fscanf causes a captured exit request; {} guest dependency bytes remain owned; cleanup pending and game main has not executed",
                native.bytes_allocated()
            );
        }
        Err(error) => return Err(format!("Native sysmodule startup boundary: {error}")),
        _ => return Err("Unexpected native sysmodule continuation".into()),
    }
    if modules.bytes() != before
        || modules.phase(caller) != Some(native_ps5::modules::Phase::Faulted)
        || modules.unload(caller).is_ok()
    {
        return Err("Missing-module failure lost bootstrap ownership".into());
    }
    if modules.phase(system) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(system).is_ok()
        || !native.system_service.splash_visible()
    {
        return Err("SystemService ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(user) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(user).is_ok()
        || native.user_service.initialized()
    {
        return Err("UserService ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(pad) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(pad).is_ok()
        || native.pad.initialized()
    {
        return Err("Pad ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(audio) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(audio).is_ok()
        || native.audio.initialized()
        || native.audio.active_count() != 0
    {
        return Err("AudioOut ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(video) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(video).is_ok()
        || native.video_out.last_evidence().is_some()
    {
        return Err("VideoOut ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(keyboard) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(keyboard).is_ok()
        || native.keyboard.initialized()
    {
        return Err("Keyboard ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(ime) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(ime).is_ok()
        || native.ime_dialog.get_status()? != 0
    {
        return Err("IME ownership or pre-main state changed unexpectedly".into());
    }
    if modules.phase(net) != Some(native_ps5::modules::Phase::Started)
        || modules.unload(net).is_ok()
    {
        return Err("Net provider ownership changed unexpectedly".into());
    }
    println!(
        "PASS: real homebrew Rust execution clears {} BSS bytes and accepts all ten named dependencies; unresolved fscanf triggers an owned ExitRequested; cleanup pending and game main has not executed",
        end - begin
    );
    if let Some(result) = presentation_result {
        result?;
    }
    Ok(())
}

fn encode_exports(exports: &[(&str, u64)]) -> Result<Vec<(String, u64)>, String> {
    exports
        .iter()
        .map(|&(name, address)| {
            let nid = native_ps5::strings::nid(name);
            Ok((
                String::from_utf8(nid[..11].to_vec()).map_err(|e| e.to_string())?,
                address,
            ))
        })
        .collect()
}
