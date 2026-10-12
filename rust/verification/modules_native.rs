use native_ps5::{
    Result,
    legacy_cpu::Cpu,
    loader::Image,
    modules::{Arguments, EntryPoints, Executor, Modules, Phase},
};
unsafe extern "C" {
    fn rust_module_start(cpu: *mut Cpu);
    fn rust_module_stop(cpu: *mut Cpu);
    fn rust_module_init(cpu: *mut Cpu);
    fn rust_module_init_array(cpu: *mut Cpu);
    fn rust_module_fini(cpu: *mut Cpu);
    fn rust_module_fini_array(cpu: *mut Cpu);
    fn rust_module_memory(cpu: *mut Cpu);
    fn rust_module_nested_call(cpu: *mut Cpu);
    fn rust_module_nested_return(cpu: *mut Cpu);
    fn rust_module_nested_body(cpu: *mut Cpu);
    fn rust_module_loop_entry(cpu: *mut Cpu);
    fn rust_module_loop_body(cpu: *mut Cpu);
    fn rust_module_loop_exit(cpu: *mut Cpu);
    fn rust_module_loop_zero(cpu: *mut Cpu);
    fn rust_module_shift(cpu: *mut Cpu);
    fn rust_module_vector128(cpu: *mut Cpu);
    fn rust_module_vector256(cpu: *mut Cpu);
    fn rust_module_service_call(cpu: *mut Cpu);
    fn rust_module_service_return(cpu: *mut Cpu);
    fn rust_module_import_call(cpu: *mut Cpu);
    fn rust_module_import_return(cpu: *mut Cpu);
}
struct Native {
    calls: Vec<u64>,
    fuel: u64,
}
impl Native {
    fn execute(&mut self, image: &mut Image, address: u64, args: Arguments) -> Result<i32> {
        let offset = address.checked_sub(image.bias).ok_or("Entry below bias")?;
        let (code, function): (&[u8], unsafe extern "C" fn(*mut Cpu)) = match offset {
            0x100 => (&[0x89, 0xf8, 0xc3], rust_module_start),
            0x110 => (&[0x31, 0xc0, 0xc3], rust_module_stop),
            0x120 => (&[0xb8, 7, 0, 0, 0, 0xc3], rust_module_init),
            0x130 => (&[0xb8, 8, 0, 0, 0, 0xc3], rust_module_init_array),
            0x140 => (&[0xb8, 9, 0, 0, 0, 0xc3], rust_module_fini),
            0x150 => (&[0xb8, 10, 0, 0, 0, 0xc3], rust_module_fini_array),
            _ => return Err("Unregistered translated entry".into()),
        };
        let segment = image
            .segments
            .iter()
            .find(|s| address >= s.address && address - s.address < s.bytes.len() as u64)
            .ok_or("Missing executable segment")?;
        let index = (address - segment.address) as usize;
        if segment.flags & 1 == 0 || segment.bytes.get(index..index + code.len()) != Some(code) {
            return Err("Module code differs from translated bytes".into());
        }
        let mut cpu = Cpu {
            fuel: self.fuel,
            bias: image.bias,
            rip: offset,
            ..Cpu::default()
        };
        cpu.registers[7] = args.size;
        cpu.registers[6] = args.pointer;
        // SAFETY: generated ARM64 block accepts the checked Cpu ABI. This fixture
        // uses only registers and returns at the guest RET boundary without memory access.
        unsafe {
            function(&mut cpu);
        }
        self.calls.push(offset);
        if cpu.fault != 0 || cpu.stop != 3 || cpu.rip != offset + code.len() as u64 - 1 {
            return Err("Guest start/stop did not reach its matching return".into());
        }
        Ok(cpu.registers[0] as u32 as i32)
    }
}
impl Executor for Native {
    fn call(&mut self, image: &mut Image, address: u64, args: Arguments) -> Result<i32> {
        self.execute(image, address, args)
    }
    fn initialize(&mut self, image: &mut Image, address: u64) -> Result<()> {
        let value = self.execute(image, address, Arguments::default())?;
        assert!((7..=10).contains(&value)); // Nonzero return register is valid for a void initializer.
        Ok(())
    }
}
fn main() {
    process_calls();
    formatting_call();
    kernel_calls();
    shared_start_arguments();
    native_dlsym();
    native_sysmodule();
    native_memcpy();
    native_process_metadata();
    native_tail_jumps();
    loader_suspension();
    resumable_lifecycle();
    guest_driven_load();
    memory_execution();
    branch_execution();
    shift_execution();
    vector_execution();
    service_execution();
    imported_service_execution();
    cross_module_execution();
    automatic_compilation();
    translation_extension();
    automatic_translation_misses();
    let directory =
        std::env::temp_dir().join(format!("native-ps5-file-start-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    for sce in [false, true] {
        let file = native_ps5::exports::fixture(sce);
        let path = directory.join(if sce { "sce.elf" } else { "standard.elf" });
        std::fs::write(&path, &file).unwrap();
        let entries = EntryPoints {
            start: Some(0x100),
            stop: Some(0x110),
        };
        let mut modules = Modules::new(8192);
        let mut native = Native {
            calls: vec![],
            fuel: 10,
        };
        let mut files = native_ps5::module_files::ModuleFiles::default();
        files
            .register("/app0/native-test.prx", &path, "native-test.prx", entries)
            .unwrap();
        let id = modules
            .load_start_file(
                &mut files,
                "/app0/native-test.prx",
                0x100000,
                &mut native,
                Arguments::default(),
            )
            .unwrap();
        let entry = native_ps5::imports::identify("entry", &std::collections::BTreeMap::new(), &[])
            .unwrap();
        let object =
            native_ps5::imports::identify("object", &std::collections::BTreeMap::new(), &[])
                .unwrap();
        assert_eq!(modules.resolve(&entry), Some(0x100100));
        assert_eq!(modules.resolve(&object), Some(0x100500));
        assert_eq!(modules.phase(id), Some(Phase::Started));
        assert!(modules.unload(id).is_err());
        modules.stop(id, &mut native, Arguments::default()).unwrap();
        assert_eq!(modules.phase(id), Some(Phase::Stopped));
        modules.unload(id).unwrap();
        assert_eq!(modules.resolve(&entry), None);
        assert_eq!(modules.resolve(&object), None);
        assert_eq!(modules.bytes(), 0);
        assert_eq!(native.calls, [0x120, 0x130, 0x100, 0x110, 0x150, 0x140]);
        let error = modules
            .load_start_file(
                &mut files,
                "/app0/native-test.prx",
                0x100000,
                &mut native,
                Arguments {
                    size: 7,
                    pointer: 0,
                },
            )
            .unwrap_err();
        let next = error.handle.unwrap();
        assert_ne!(next, id);
        assert_eq!(modules.phase(next), Some(Phase::Faulted));
        assert!(modules.unload(next).is_err());
        let exhausted = modules
            .load_dynamic("fuel-test.prx", &file, 0x200000, entries)
            .unwrap();
        native.fuel = 0;
        assert!(
            modules
                .start(exhausted, &mut native, Arguments::default())
                .is_err()
        );
        assert_eq!(modules.phase(exhausted), Some(Phase::Faulted));
    }
    std::fs::remove_dir_all(&directory).unwrap();
    println!(
        "PASS: Rust file-backed ELF modules load/start with standard/SCE exports and lifecycle routines; ARM64 execution, order, void returns, status, fuel, unload and stale handles verified"
    );
}

fn automatic_translation_misses() {
    use native_ps5::module_executor::{Execution, NativeExecutor, Service, memory_fixture};
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    for transfer in 0..3 {
        let mut file = memory_fixture();
        let mut main = vec![0xff, 0x07, 0x48, 0xb8]; // Increment exactly once, then target.
        main.extend(0x200120u64.to_le_bytes());
        match transfer {
            0 => main.extend([0xff, 0xd0, 0xff, 0x07, 0x48, 0x89, 0x67, 0x18, 0xc3]),
            1 => main.extend([0xff, 0xe0]),
            _ => main.extend([0x50, 0xc3]), // A guest-produced return target.
        }
        file[0x100..0x100 + main.len()].copy_from_slice(&main);
        file[0x500..0x520].fill(0);
        let mut child = memory_fixture();
        child[0x110..0x113].copy_from_slice(&[0x31, 0xc0, 0xc3]);
        child[0x120..0x12c]
            .copy_from_slice(&[0xff, 0x07, 0x48, 0x89, 0x67, 0x10, 0xb8, 42, 0, 0, 0, 0xc3]);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut dependency = Image::load(&child, 0x200000, 4096).unwrap();
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        // SAFETY: immutable matching runtime archive, host-controlled output path.
        unsafe {
            native
                .compile_image(
                    &file,
                    image.bias,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
            native
                .compile_image(
                    &child,
                    dependency.bias,
                    BTreeSet::from([0x110]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
            native.set_automatic_translation(true);
        }
        let execution = native
            .run(
                &mut image,
                0x100100,
                Arguments {
                    size: 0x100500,
                    pointer: 0,
                },
                &mut [&mut dependency],
            )
            .unwrap();
        assert!(matches!(execution, Execution::Returned(42)));
        assert_eq!(native.translation_rebuilds(0x100000), Some(0));
        assert_eq!(native.translation_rebuilds(0x200000), Some(1));
        assert!(native.translation_roots(0x200000).unwrap().contains(&0x120));
        let data = &image.segments[1].bytes;
        assert_eq!(
            u32::from_le_bytes(data[..4].try_into().unwrap()),
            if transfer == 0 { 3 } else { 2 }
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            0x900ff8 - if transfer == 0 { 8 } else { 0 }
        );
        if transfer == 0 {
            assert_eq!(
                u64::from_le_bytes(data[24..32].try_into().unwrap()),
                0x900ff8
            );
        }
        // A warmed target does not request another compiler invocation.
        native
            .run(
                &mut image,
                0x100100,
                Arguments {
                    size: 0x100500,
                    pointer: 0,
                },
                &mut [&mut dependency],
            )
            .unwrap();
        assert_eq!(native.translation_rebuilds(0x200000), Some(1));
    }
    // Resolve a missing service return address before invoking malloc. Retrying
    // must neither replay the guest increment nor allocate a second buffer.
    let mut file = memory_fixture();
    let mut code = vec![0xbb];
    code.extend(0x100500u32.to_le_bytes());
    code.extend([0xff, 0x03, 0xbf]);
    code.extend(16u32.to_le_bytes());
    code.extend([0x48, 0xb8]);
    code.extend(0x100160u64.to_le_bytes());
    code.extend([0x50, 0x48, 0xb8]);
    code.extend(0x710000u64.to_le_bytes());
    code.extend([0xff, 0xe0]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    file[0x160..0x16e].copy_from_slice(&[
        0x48, 0x89, 0x43, 8, 0x48, 0x89, 0x63, 16, 0xb8, 42, 0, 0, 0, 0xc3,
    ]);
    file[0x500..0x520].fill(0);
    let mut image = Image::load(&file, 0x100000, 4096).unwrap();
    let mut native = NativeExecutor::new(0xa00000, 4096, 100, 100).unwrap();
    native.kernel = Some(native_ps5::kernel::Kernel::new(4096, 0x100580).unwrap());
    native.register_service(0x710000, Service::Malloc).unwrap();
    unsafe {
        native
            .compile_image(
                &file,
                image.bias,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
        native.set_automatic_translation(true);
    }
    assert!(matches!(
        native
            .run(&mut image, 0x100100, Arguments::default(), &mut [])
            .unwrap(),
        Execution::Returned(42)
    ));
    assert_eq!(native.bytes_allocated(), 16);
    let data = &image.segments[1].bytes;
    assert_eq!(&data[..4], &1u32.to_le_bytes());
    assert_ne!(&data[8..16], &[0; 8]);
    assert_eq!(&data[16..24], &0xa00ff8u64.to_le_bytes());
    assert_eq!(native.translation_rebuilds(image.bias), Some(1));
    println!(
        "PASS: automatic observed-target compilation preserves cross-image CALL/JMP/RET stacks, registers and exactly-once guest/service effects; cached targets do not rebuild"
    );
}

fn translation_extension() {
    use native_ps5::module_executor::{
        Execution, MAX_TRANSLATION_REBUILDS, NativeExecutor, memory_fixture,
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    let mut file = memory_fixture();
    file[0x100..0x102].copy_from_slice(&[0xff, 0x27]);
    file[0x110..0x116].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
    for index in 0..=MAX_TRANSLATION_REBUILDS {
        let offset = 0x120 + index * 8;
        file[offset..offset + 6].copy_from_slice(&[0xb8, 7, 0, 0, 0, 0xc3]);
    }
    file[0x1f0..0x1f2].copy_from_slice(&[0x0f, 0x0b]);
    file[0x500..0x508].copy_from_slice(&0x100120u64.to_le_bytes());
    let mut image = Image::load(&file, 0x100000, 4096).unwrap();
    let mut executor = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
    // SAFETY: exact trusted runtime archive; all compilation occurs between runs.
    unsafe {
        executor
            .compile_image(
                &file,
                image.bias,
                BTreeSet::from([0x100, 0x110]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    let args = Arguments {
        size: 0x100500,
        pointer: 0,
    };
    assert!(
        executor
            .run(&mut image, 0x100100, args, &mut [])
            .err()
            .unwrap()
            .contains("tail jump")
    );
    let before = image.segments[1].bytes.clone();
    unsafe {
        executor
            .extend_image_translation(&image, 0x100120, &archive, &directory)
            .unwrap();
    }
    assert!(matches!(
        executor.run(&mut image, 0x100100, args, &mut []).unwrap(),
        Execution::Returned(7)
    ));
    assert_eq!(image.segments[1].bytes, before);
    let mut altered_file = file.clone();
    altered_file[0x530] ^= 1;
    let other = Image::load(&altered_file, image.bias, 4096).unwrap();
    assert!(
        unsafe { executor.extend_image_translation(&other, 0x100128, &archive, &directory) }
            .is_err()
    );
    for target in [0, 0x100500, 0x100100, 0x100121, 0x1001f0] {
        assert!(
            unsafe { executor.extend_image_translation(&image, target, &archive, &directory) }
                .is_err()
        );
        assert!(matches!(
            executor
                .run(&mut image, 0x100110, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(42)
        ));
    }
    image.segments[0].bytes[0x128] = 0x90;
    assert!(
        unsafe { executor.extend_image_translation(&image, 0x100128, &archive, &directory) }
            .is_err()
    );
    image.segments[0].bytes[0x128] = file[0x128];
    assert!(matches!(
        executor.run(&mut image, 0x100100, args, &mut []).unwrap(),
        Execution::Returned(7)
    ));
    // A smaller original instruction budget cannot be silently enlarged.
    let mut limited = NativeExecutor::new(0xa00000, 4096, 100, 100).unwrap();
    unsafe {
        limited
            .compile_image(
                &file,
                image.bias,
                BTreeSet::from([0x110]),
                3,
                &archive,
                &directory,
            )
            .unwrap();
        assert!(
            limited
                .extend_image_translation(&image, 0x100120, &archive, &directory)
                .err()
                .unwrap()
                .contains("budget")
        );
    }
    assert!(matches!(
        limited
            .run(&mut image, 0x100110, Arguments::default(), &mut [])
            .unwrap(),
        Execution::Returned(42)
    ));
    // Rebuild allowance is independent of each compilation's instruction bound.
    let mut bounded = NativeExecutor::new(0xb00000, 4096, 100, 100).unwrap();
    unsafe {
        bounded
            .compile_image(
                &file,
                image.bias,
                BTreeSet::from([0x110]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    for index in 0..MAX_TRANSLATION_REBUILDS {
        unsafe {
            bounded
                .extend_image_translation(&image, 0x100120 + index as u64 * 8, &archive, &directory)
                .unwrap();
        }
    }
    assert!(
        unsafe {
            bounded.extend_image_translation(
                &image,
                0x100120 + MAX_TRANSLATION_REBUILDS as u64 * 8,
                &archive,
                &directory,
            )
        }
        .err()
        .unwrap()
        .contains("rebuild budget")
    );
    assert!(matches!(
        bounded
            .run(&mut image, 0x100110, Arguments::default(), &mut [])
            .unwrap(),
        Execution::Returned(42)
    ));
    bounded.remove_image_translations(image.bias);
    assert!(
        unsafe { bounded.extend_image_translation(&image, 0x100120, &archive, &directory) }
            .is_err()
    );
    unsafe {
        bounded
            .compile_image(
                &file,
                image.bias,
                BTreeSet::from([0x110]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    println!(
        "PASS: explicit observed-target translation extension executes a formerly missing indirect target; immutable identity, instruction interiors, modified code, instruction/rebuild limits and atomic failure preservation verified"
    );
}

fn kernel_calls() {
    use native_ps5::{
        kernel::Kernel,
        module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    for valid in [true, false] {
        let mut kernel = Kernel::new(4096, 0x1005f0).unwrap();
        let mut control = vec![0; 24];
        control[..4].copy_from_slice(&8u32.to_le_bytes());
        control[4..8].copy_from_slice(&8u32.to_le_bytes());
        control[12..16].copy_from_slice(&16u32.to_le_bytes());
        control[16..].copy_from_slice(&0xffff800000002000u64.to_le_bytes());
        kernel.map(0xffff800000001000, control, true).unwrap();
        let mut ring = vec![0; 16];
        ring[..8].copy_from_slice(b"rustpipe");
        kernel.map(0xffff800000002000, ring, true).unwrap();
        let (reader, _) = kernel.kernel_pipe(0xffff800000001000).unwrap();
        let fd = if valid { reader } else { 999 };
        // Preserve output pointer in RBX, set read(fd, output+128, 8), then
        // invoke the library syscall entry and save its result and carry flag.
        let mut code = vec![0x48, 0x89, 0xfb, 0xbf];
        code.extend(fd.to_le_bytes());
        code.extend([0xbe]);
        code.extend(0x100580u32.to_le_bytes());
        code.extend([0xba, 8, 0, 0, 0, 0xb8, 3, 0, 0, 0, 0x49, 0xbb]);
        code.extend(0x71000au64.to_le_bytes());
        code.extend([
            0x41, 0xff, 0xd3, 0x48, 0x89, 0x03, 0x0f, 0x92, 0x43, 8, 0x31, 0xc0, 0xc3,
        ]);
        let mut file = memory_fixture();
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        native.kernel = Some(kernel);
        native.register_service(0x71000a, Service::Syscall).unwrap();
        // SAFETY: local trusted archive matches this runtime and generated code.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        assert!(matches!(
            native
                .run(
                    &mut image,
                    0x100100,
                    Arguments {
                        size: 0x100500,
                        pointer: 0
                    },
                    &mut []
                )
                .unwrap(),
            Execution::Returned(0)
        ));
        let data = &image.segments[1].bytes;
        assert_eq!(
            &data[..8],
            &if valid { 8u64 } else { u64::MAX }.to_le_bytes()
        );
        assert_eq!(data[8], u8::from(!valid));
        assert_eq!(&data[128..136], if valid { b"rustpipe" } else { &[0; 8] });
        assert_eq!(&data[240..244], &if valid { 0u32 } else { 9 }.to_le_bytes());
        native.remove_image_translations(0x100000);
    }
    println!(
        "PASS: translated ARM64 read syscall copies owned kernel-pipe bytes; invalid descriptors return -1, carry and guest errno without output writes"
    );
}
fn formatting_call() {
    use native_ps5::{
        kernel::Kernel,
        module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    for valid in [true, false] {
        // snprintf(RDI, 5, format, 42): verify SysV varargs and truncation.
        let mut code = vec![0x48, 0x89, 0xfb, 0xbf];
        code.extend(if valid { 35u32 } else { u32::MAX }.to_le_bytes());
        code.extend([0x49, 0xbb]);
        code.extend(0x720010u64.to_le_bytes());
        code.extend([
            0x41, 0xff, 0xd3, 0x48, 0x8b, 0x00, 0x48, 0x89, 0x43, 16, 0x48, 0x89, 0xdf,
        ]);
        code.extend([0xbf, 2, 0, 0, 0, 0xbe, 8, 0, 0, 0, 0x49, 0xbb]);
        code.extend(0x720020u64.to_le_bytes());
        code.extend([
            0x41, 0xff, 0xd3, 0x48, 0x8b, 0x08, 0x48, 0x89, 0x4b, 24, 0x48, 0xc7, 0x00, 0x34, 0x12,
            0, 0, 0x48, 0x8b, 0x08, 0x48, 0x89, 0x4b, 32, 0x48, 0x89, 0xc7, 0x49, 0xbb,
        ]);
        code.extend(0x720030u64.to_le_bytes());
        code.extend([0x41, 0xff, 0xd3, 0x48, 0x89, 0xdf]);
        code.extend([0xbf]);
        code.extend(0x100568u32.to_le_bytes());
        code.extend([0x49, 0xbb]);
        code.extend(0x720040u64.to_le_bytes());
        code.extend([0x41, 0xff, 0xd3, 0x48, 0x89, 0x43, 40, 0x48, 0x89, 0xdf]);
        code.extend([0xbe, 5, 0, 0, 0, 0xba]);
        code.extend(0x100580u32.to_le_bytes());
        code.extend([0xb9, 42, 0, 0, 0, 0x49, 0xbb]);
        code.extend(0x720000u64.to_le_bytes());
        code.extend([0x41, 0xff, 0xd3, 0x48, 0x89, 0x47, 8, 0x31, 0xc0, 0xc3]);
        let mut file = memory_fixture();
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let format: &[u8] = if valid { b"value=%u\0" } else { b"%f\0" };
        image.segments[1].bytes[128..128 + format.len()].copy_from_slice(format);
        image.segments[1].bytes[..5].fill(0xa5);
        image.segments[1].bytes[64..72].copy_from_slice(&0x100560u64.to_le_bytes());
        image.segments[1].bytes[96..104].copy_from_slice(b"TEST=ok\0");
        image.segments[1].bytes[104..109].copy_from_slice(if valid {
            b"TEST\0"
        } else {
            b"NONE\0"
        });
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        native.kernel = Some(Kernel::new(0, 0x1005f0).unwrap());
        native
            .register_service(0x720000, Service::Snprintf)
            .unwrap();
        native
            .register_service(0x720010, Service::Strerror)
            .unwrap();
        native.register_service(0x720020, Service::Calloc).unwrap();
        native.register_service(0x720030, Service::Free).unwrap();
        native.register_service(0x720040, Service::Getenv).unwrap();
        native.environment = 0x100540;
        // SAFETY: this harness uses the matching trusted runtime archive.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        assert!(matches!(
            native
                .run(
                    &mut image,
                    0x100100,
                    Arguments {
                        size: 0x100500,
                        pointer: 0
                    },
                    &mut []
                )
                .unwrap(),
            Execution::Returned(0)
        ));
        let data = &image.segments[1].bytes;
        assert_eq!(&data[..5], if valid { b"valu\0" } else { &[0xa5; 5] });
        assert_eq!(&data[16..24], if valid { b"Resource" } else { b"Unknown " });
        assert_eq!(&data[24..32], &[0; 8]);
        assert_eq!(&data[32..40], &0x1234u64.to_le_bytes());
        assert_eq!(native.bytes_allocated(), 0);
        assert_eq!(
            &data[40..48],
            &if valid { 0x100565u64 } else { 0 }.to_le_bytes()
        );
        assert_eq!(
            &data[8..16],
            &if valid { 8u64 } else { u32::MAX as u64 }.to_le_bytes()
        );
        assert_eq!(
            &data[240..244],
            &if valid { 0u32 } else { 22 }.to_le_bytes()
        );
        native.remove_image_translations(0x100000);
    }
    println!(
        "PASS: translated ARM64 snprintf uses Rust SysV formatting, returns untruncated length and preserves output on unsupported conversion"
    );
}
fn process_calls() {
    use native_ps5::{
        loader::Image,
        module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    for (direct, number) in [(true, 20u32), (false, 20), (false, 21)] {
        let target = if direct { 0x710000u64 } else { 0x71000a };
        let mut code = vec![0xb9, 0x55, 0, 0, 0, 0xb8];
        code.extend(number.to_le_bytes());
        code.extend([0x49, 0xbb]);
        code.extend(target.to_le_bytes());
        code.extend([
            0x41, 0xff, 0xd3, 0x48, 0x89, 0x07, 0x48, 0x89, 0x4f, 8, 0x4c, 0x89, 0x57, 16, 0x4c,
            0x89, 0x5f, 24, 0x31, 0xc0, 0xc3,
        ]);
        let mut file = memory_fixture();
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        native
            .register_service(
                target,
                if direct {
                    Service::Getpid
                } else {
                    Service::Syscall
                },
            )
            .unwrap();
        // SAFETY: harness supplies the matching trusted archive for emitted code.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        let result = native.run(
            &mut image,
            0x100100,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
            &mut [],
        );
        let data = &image.segments[1].bytes;
        if number == 21 {
            assert!(result.err().unwrap().contains("syscall 21"));
            assert_eq!(&data[..32], &[0; 32]);
        } else {
            assert!(matches!(result.unwrap(), Execution::Returned(0)));
            for (offset, expected) in [
                (0, u64::from(std::process::id())),
                (8, 0x71000c),
                (16, if direct { 0x55 } else { 0 }),
                (24, 2),
            ] {
                assert_eq!(&data[offset..offset + 8], &expected.to_le_bytes());
            }
        }
        native.remove_image_translations(0x100000);
    }
    println!(
        "PASS: Rust getpid/syscall-20 thunks return the host PID with modeled syscall register effects; unsupported syscall 21 stops without caller writes"
    );
}

fn shared_start_arguments() {
    use native_ps5::{
        module_executor::{NativeExecutor, memory_fixture},
        module_files::ModuleFiles,
        modules::LoadPlan,
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory =
        PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap()).join("shared-args");
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("writer.elf");
    let mut child = memory_fixture();
    let writer = [
        0x48, 0x8b, 0x06, 0x48, 0x83, 0xc0, 1, 0x48, 0x89, 0x06, 0x31, 0xc0, 0xc3,
    ];
    child[0x100..0x100 + writer.len()].copy_from_slice(&writer);
    std::fs::write(&source, child).unwrap();
    for (mode, unload) in [
        (0, false),
        (1, false),
        (2, false),
        (0, true),
        (1, true),
        (3, true),
    ] {
        let mut file = memory_fixture();
        let mut code = if mode == 1 {
            // Reserve caller stack argument, initialize it, pass its actual address.
            vec![
                0x48, 0x83, 0xec, 0x10, 0x48, 0xc7, 0x04, 0x24, 0x22, 0x11, 0, 0, 0x48, 0x89, 0xe2,
            ]
        } else if mode == 2 {
            vec![0xba, 0, 1, 0x10, 0] // readonly code at 100100h
        } else {
            vec![0x48, 0x8d, 0x57, 0x60]
        };
        code.extend([
            0xbe, 8, 0, 0, 0, 0x4c, 0x8d, 0x4f, 0x40, 0x48, 0xb8, 0, 0, 0x70, 0, 0, 0, 0, 0, 0xff,
            0xd0,
        ]);
        if unload {
            if mode == 3 {
                code.extend([0xba, 0, 1, 0x10, 0]);
            }
            // Save path pointer, stop the returned handle with the same size/argp,
            // then restore the pointer used for checking caller-visible writes.
            code.extend([
                0x48, 0x89, 0xfb, 0x48, 0x89, 0xc7, 0x48, 0xb8, 0x10, 0, 0x70, 0, 0, 0, 0, 0, 0xff,
                0xd0, 0x48, 0x89, 0xdf,
            ]);
        }
        if mode == 1 {
            code.extend([
                0x48, 0x8b, 0x04, 0x24, 0x48, 0x89, 0x47, 0x60, 0x48, 0x83, 0xc4, 0x10,
            ]);
        }
        code.extend([0x31, 0xc0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let path = b"/app0/writer.prx\0";
        file[0x500..0x500 + path.len()].copy_from_slice(path);
        file[0x560..0x568].copy_from_slice(&0x1122u64.to_le_bytes());
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let mut modules = Modules::new(65536);
        native
            .register_loader_module(&mut modules, "libkernel.prx", 0x700000)
            .unwrap();
        // SAFETY: harness supplies this exact runtime's trusted archive.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        let caller = modules
            .load_dynamic(
                "arguments.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        let mut files = ModuleFiles::default();
        files
            .register(
                "/app0/writer.prx",
                &source,
                "writer.prx",
                EntryPoints {
                    start: Some(0x100),
                    stop: unload.then_some(0x100),
                },
            )
            .unwrap();
        let mut biases = [0x200000].into_iter();
        // SAFETY: same matching archive contract, including loaded child code.
        let result = unsafe {
            modules.start_native_with_files(
                caller,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: 0,
                },
                &mut files,
                LoadPlan {
                    biases: &mut biases,
                    max_requests: 2,
                    instruction_limit: 100,
                    archive: &archive,
                    directory: &directory,
                },
            )
        };
        if mode == 2 || mode == 3 {
            assert!(result.err().unwrap().contains("fault"));
            assert_eq!(modules.phase(caller), Some(Phase::Faulted));
            assert_eq!(modules.phase(caller + 1), Some(Phase::Faulted));
            assert_eq!(
                &modules.image(caller).unwrap().segments[0].bytes[0x100..0x100 + code.len()],
                &code
            );
        } else {
            result.unwrap();
            assert_eq!(
                &modules.image(caller).unwrap().segments[1].bytes[0x60..0x68],
                &(if unload { 0x1124u64 } else { 0x1123u64 }).to_le_bytes()
            );
            if unload {
                assert_eq!(modules.phase(caller + 1), None);
            } else {
                modules.bind_sysmodule(6, caller + 1).unwrap();
                modules.retain_sysmodule(6).unwrap();
                assert!(modules.sysmodule_loaded(6));
                assert!(
                    modules
                        .stop_dynamic_module(caller, caller + 1, &mut native, Arguments::default())
                        .is_err()
                );
                assert_eq!(modules.phase(caller + 1), Some(Phase::Started));
                modules.release_sysmodule(6).unwrap();
                modules
                    .stop_dynamic_module(caller, caller + 1, &mut native, Arguments::default())
                    .unwrap();
            }
            modules
                .stop(caller, &mut native, Arguments::default())
                .unwrap();
            modules.unload(caller).unwrap();
        }
        drop(modules);
        native.remove_image_translations(0x100000);
        native.remove_image_translations(0x200000);
    }
    std::fs::remove_file(source).unwrap();
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir(directory).unwrap();
    println!(
        "PASS: native module start/stop routines update original caller data and suspended stack arguments with distinct stacks; readonly writes fault and retain images"
    );
}

fn compile_native_fixture(
    native: &mut native_ps5::module_executor::NativeExecutor,
    file: &[u8],
    bias: u64,
    roots: &[u64],
) {
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    // SAFETY: regression uses its own matching trusted runtime archive.
    unsafe {
        native
            .compile_image(
                file,
                bias,
                BTreeSet::from_iter(roots.iter().copied()),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
}
fn native_process_metadata() {
    use native_ps5::module_executor::{Execution, NativeExecutor, Service, memory_fixture};
    for (service, config, first, second, expected) in [
        (Service::Getargc, Some((1, 0x100520)), 0, 0, Some(1u64)),
        (Service::Getargv, Some((1, 0x100520)), 0, 0, Some(0x100520)),
        (Service::Getargc, None, 0, 0, None),
        (Service::Getargv, Some((2, 0x100520)), 0, 0, None),
        (Service::Getargv, Some((1, 0x1005fc)), 0, 0, None),
        (Service::Getargv, Some((1, 0x1005f8)), 0, 0, None),
        (Service::InternalStrerror, None, 22, 0, Some(0x7fff00000000)),
        (Service::InternalStrerror, None, 22, 0x100560, None),
    ] {
        let mut file = memory_fixture();
        let mut code = vec![0x48, 0xb8];
        code.extend(0x760000u64.to_le_bytes());
        code.extend([0xff, 0xd0, 0xb9]);
        code.extend(0x100580u32.to_le_bytes());
        code.extend([0x48, 0x89, 0x01]);
        if matches!(service, Service::InternalStrerror) {
            code.extend([0x48, 0x8b, 0x10, 0x48, 0x89, 0x51, 8]);
        }
        code.extend([0x31, 0xc0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        file[0x520..0x528].copy_from_slice(&0x100540u64.to_le_bytes());
        file[0x528..0x538].fill(0);
        file[0x540..0x544].copy_from_slice(b"app\0");
        file[0x580..0x590].fill(0xa5);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        native.process_arguments = config;
        native.register_service(0x760000, service).unwrap();
        compile_native_fixture(&mut native, &file, 0x100000, &[0x100]);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let outcome = native.run(
            &mut image,
            0x100100,
            Arguments {
                size: first,
                pointer: second,
            },
            &mut [],
        );
        if let Some(value) = expected {
            assert!(matches!(outcome, Ok(Execution::Returned(0))));
            assert_eq!(&image.segments[1].bytes[0x80..0x88], &value.to_le_bytes());
            if matches!(service, Service::InternalStrerror) {
                assert_eq!(&image.segments[1].bytes[0x88..0x90], b"Invalid ");
            }
        } else {
            assert!(outcome.is_err());
            assert_eq!(&image.segments[1].bytes[0x80..0x90], &[0xa5; 16]);
        }
    }
    println!(
        "PASS: native argc/argv use explicit checked guest tables; null-buffer _Strerror returns readonly text and unsupported buffers fault"
    );
}
fn native_tail_jumps() {
    use native_ps5::module_executor::{Execution, NativeExecutor, Service, memory_fixture};
    for mode in 0..5 {
        let mut file = memory_fixture();
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let mut dependencies = Vec::new();
        let argument;
        if mode == 0 {
            file[0x100..0x102].copy_from_slice(&[0xff, 0x27]);
            file[0x120..0x126].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
            file[0x500..0x508].copy_from_slice(&0x200120u64.to_le_bytes());
            compile_native_fixture(&mut native, &file, 0x200000, &[0x120]);
            dependencies.push(Image::load(&file, 0x200000, 4096).unwrap());
            argument = 0x100500;
        } else {
            let target = if mode >= 3 { 0x700020u64 } else { 0x710000 };
            native
                .register_service(
                    target,
                    if mode >= 3 {
                        Service::Dlsym
                    } else {
                        Service::Strlen
                    },
                )
                .unwrap();
            let start = if mode == 4 {
                let mut call = vec![0x48, 0xb8];
                call.extend(0x200120u64.to_le_bytes());
                call.extend([0xff, 0xd0, 0xc3]);
                file[0x100..0x100 + call.len()].copy_from_slice(&call);
                0x120
            } else if mode == 1 {
                file[0x100..0x106].copy_from_slice(&[0xe8, 0x1b, 0, 0, 0, 0xc3]);
                0x120
            } else {
                0x100
            };
            let mut code = vec![0x48, 0xb8];
            code.extend(target.to_le_bytes());
            code.extend([0xff, 0xe0]);
            file[start..start + code.len()].copy_from_slice(&code);
            if mode == 4 {
                compile_native_fixture(&mut native, &file, 0x200000, &[0x120]);
                dependencies.push(Image::load(&file, 0x200000, 4096).unwrap());
            }
            file[0x520..0x524].copy_from_slice(b"abc\0");
            argument = 0x100520;
        }
        compile_native_fixture(&mut native, &file, 0x100000, &[0x100]);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut refs = dependencies.iter_mut().collect::<Vec<_>>();
        let result = native
            .run(
                &mut image,
                0x100100,
                Arguments {
                    size: argument,
                    pointer: 0,
                },
                &mut refs,
            )
            .unwrap();
        if mode >= 3 {
            let Execution::Dlsym { continuation, .. } = result else {
                panic!("Expected tail service suspension");
            };
            assert!(matches!(
                native
                    .resume(&mut image, &mut refs, *continuation, 17)
                    .unwrap(),
                Execution::Returned(17)
            ));
        } else {
            assert!(
                matches!(result,Execution::Returned(value) if value == if mode == 0 {42} else {3})
            );
        }
    }
    println!(
        "PASS: native indirect tail jumps cross images, return through existing frames and suspend/resume a terminal service without stack pushes"
    );
}

fn native_memcpy() {
    use native_ps5::module_executor::{Execution, NativeExecutor, Service, memory_fixture};
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    let mut file = memory_fixture();
    let mut code = vec![0xba, 8, 0, 0, 0, 0x48, 0xb8];
    code.extend(0x750000u64.to_le_bytes());
    code.extend([0xff, 0xd0, 0xb9]);
    code.extend(0x100580u32.to_le_bytes());
    code.extend([0x48, 0x89, 0x01, 0x31, 0xc0, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    file[0x520..0x528].copy_from_slice(&[0, 1, 2, 3, 4, 0xfe, 0xff, 7]);
    file[0x560..0x570].fill(0xa5);
    let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
    native.register_service(0x750000, Service::Memcpy).unwrap();
    // SAFETY: exact trusted runtime archive used by native regression.
    unsafe {
        native
            .compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    for (destination, source, valid) in [
        (0x100560, 0x100520, true),
        (0x100100, 0x100520, false),
        (0x100560, 0x1005fc, false),
        (0x1005fc, 0x100520, false),
        (0x100524, 0x100520, false),
    ] {
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let before = image.segments[1].bytes.clone();
        let result = native.run(
            &mut image,
            0x100100,
            Arguments {
                size: destination,
                pointer: source,
            },
            &mut [],
        );
        if valid {
            assert!(matches!(result, Ok(Execution::Returned(0))));
            assert_eq!(&image.segments[1].bytes[0x60..0x68], &file[0x520..0x528]);
            assert_eq!(
                &image.segments[1].bytes[0x80..0x88],
                &destination.to_le_bytes()
            );
        } else {
            assert!(result.is_err());
            assert_eq!(image.segments[1].bytes, before);
        }
    }
    println!(
        "PASS: native ARM64 memcpy copies raw bytes and returns destination; readonly, crossing and overlapping ranges fault without writes"
    );
}

fn native_sysmodule() {
    use native_ps5::{
        module_executor::{NativeExecutor, Service, memory_fixture},
        modules::StartEvent,
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    let mut file = memory_fixture();
    let mut code = vec![0x48, 0xb8];
    code.extend(0x750000u64.to_le_bytes());
    code.extend([0xff, 0xd0, 0xb9]);
    code.extend(0x100540u32.to_le_bytes());
    code.extend([0x89, 0x01, 0x31, 0xc0, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    file[0x540..0x544].fill(0xff);
    for argument in [0u64, 0x80000000, 0xdeadbeef80001234] {
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let mut modules = Modules::new(65536);
        native
            .register_service(0x750000, Service::SysmoduleLoadInternal)
            .unwrap();
        modules
            .register_native(
                "libSceSysmodule.prx",
                &[("sceSysmoduleLoadModuleInternal", 0x750000)],
            )
            .unwrap();
        let provider = native
            .register_string_module(&mut modules, "strings.prx", 0x710000)
            .unwrap();
        // SAFETY: verification uses the matching trusted Rust runtime archive.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        let caller = modules
            .load_dynamic(
                "sysmodule-caller.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        assert!(
            matches!(modules.start_native(caller, &mut native, Arguments { size: argument, pointer: 0 }).unwrap(), StartEvent::SysmoduleLoadInternal { arguments } if arguments[0] == argument)
        );
        assert!(modules.dispatch_native_dlsym(caller, &mut native).is_err());
        // Unknown and invalid IDs neither succeed nor consume a reference.
        assert!(
            modules
                .dispatch_native_sysmodule(caller, &mut native)
                .is_err()
        );
        assert_eq!(
            &modules.image(caller).unwrap().segments[1].bytes[0x40..0x44],
            &[0xff; 4]
        );
        if argument as u32 & 0x7fffffff == 0 {
            modules.abandon_native_start(caller).unwrap();
            continue;
        }
        let id = argument as u32;
        modules.bind_sysmodule(id, provider).unwrap();
        assert!(!modules.sysmodule_loaded(id));
        assert!(matches!(
            modules
                .dispatch_native_sysmodule(caller, &mut native)
                .unwrap(),
            StartEvent::Started
        ));
        assert!(modules.sysmodule_loaded(id));
        assert_eq!(
            &modules.image(caller).unwrap().segments[1].bytes[0x40..0x44],
            &[0; 4]
        );
        assert!(
            modules
                .dispatch_native_sysmodule(caller, &mut native)
                .is_err()
        );
        assert!(modules.unload(provider).is_err());
        modules.release_sysmodule(id).unwrap();
        assert!(!modules.sysmodule_loaded(id));
        assert!(modules.release_sysmodule(id).is_err());
    }
    println!(
        "PASS: native ARM64 sysmodule load yields to the manager, rejects absent/invalid providers, retains an actual started provider and resumes with success; upper ABI register bits ignored"
    );
}

fn native_dlsym() {
    use native_ps5::{
        module_executor::{NativeExecutor, memory_fixture},
        module_files::ModuleFiles,
        modules::LoadPlan,
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    let mut file = memory_fixture();
    let code = [
        0xba, 0x40, 0x05, 0x10, 0, 0x48, 0xb8, 0x20, 0, 0x70, 0, 0, 0, 0, 0, 0xff, 0xd0, 0x89,
        0x05, 0x31, 0x04, 0, 0, 0x31, 0xc0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    for (symbol, invalid_handle, bad_pointer, expected, storage) in [
        ("strlen", false, false, 0u32, 0),
        ("missing", false, false, 0x80020003, 0),
        ("strlen", true, false, 0x80020003, 0),
        ("strlen", false, true, 0x8002000e, 0),
        ("strlen", false, false, 0, 1),
        ("missing", false, false, 0x80020003, 1),
        ("Success", false, false, 0x80020003, 2),
        ("strlen", false, false, 0x8002000e, 3),
    ] {
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let mut modules = Modules::new(65536);
        if storage == 1 {
            use native_ps5::module_executor::Service;
            native.kernel = Some(native_ps5::kernel::Kernel::new(0, 0x1005f0).unwrap());
            native.register_service(0x720000, Service::Malloc).unwrap();
            native.register_service(0x720010, Service::Free).unwrap();
            let mut heap_code = vec![
                0x49, 0x89, 0xfc, 0x49, 0x89, 0xf5, 0xbf, 32, 0, 0, 0, 0x49, 0xbb,
            ];
            heap_code.extend(0x720000u64.to_le_bytes());
            heap_code.extend([
                0x41, 0xff, 0xd3, 0x49, 0x89, 0xc6, 0x48, 0x89, 0xc7, 0x4c, 0x89, 0xee, 0x49, 0xbb,
            ]);
            heap_code.extend(0x710000u64.to_le_bytes());
            heap_code.extend([
                0x41, 0xff, 0xd3, 0x48, 0xc7, 0xc1, 0xff, 0xff, 0xff, 0xff, 0x49, 0x89, 0x4e, 16,
                0x49, 0x8d, 0x56, 16, 0x4c, 0x89, 0xf6, 0x4c, 0x89, 0xe7, 0x49, 0xbb,
            ]);
            heap_code.extend(0x700020u64.to_le_bytes());
            heap_code.extend([0x41, 0xff, 0xd3, 0xb9]);
            heap_code.extend(0x100548u32.to_le_bytes());
            heap_code.extend([0x89, 0x01, 0x49, 0x8b, 0x46, 16, 0xb9]);
            heap_code.extend(0x100540u32.to_le_bytes());
            heap_code.extend([0x48, 0x89, 0x01, 0x4c, 0x89, 0xf7, 0x49, 0xbb]);
            heap_code.extend(0x720010u64.to_le_bytes());
            heap_code.extend([0x41, 0xff, 0xd3, 0x31, 0xc0, 0xc3]);
            file[0x100..0x100 + heap_code.len()].copy_from_slice(&heap_code);
        }
        if storage >= 2 {
            use native_ps5::module_executor::Service;
            native.kernel = Some(native_ps5::kernel::Kernel::new(0, 0x1005f0).unwrap());
            native
                .register_service(0x720020, Service::Strerror)
                .unwrap();
            // Preserve the provider handle; obtain the symbol name from actual
            // service-owned memory, then yield to the module manager via dlsym.
            let mut service_code = vec![0x49, 0x89, 0xfc, 0x49, 0x89, 0xf5, 0x31, 0xff, 0x49, 0xbb];
            service_code.extend(0x720020u64.to_le_bytes());
            service_code.extend([0x41, 0xff, 0xd3]);
            if storage == 2 {
                service_code.extend([0x48, 0x89, 0xc6, 0xba]);
                service_code.extend(0x100540u32.to_le_bytes());
            } else {
                // A result pointer into the same service buffer must remain
                // read-only when the module manager accesses suspended memory.
                service_code.extend([0x48, 0x89, 0xc2, 0x4c, 0x89, 0xee]);
            }
            service_code.extend([0x4c, 0x89, 0xe7]);
            service_code.extend([0x49, 0xbb]);
            service_code.extend(0x700020u64.to_le_bytes());
            service_code.extend([0x41, 0xff, 0xd3, 0xb9]);
            service_code.extend(0x100548u32.to_le_bytes());
            service_code.extend([0x89, 0x01, 0x31, 0xc0, 0xc3]);
            file[0x100..0x100 + service_code.len()].copy_from_slice(&service_code);
        }
        native
            .register_loader_module(&mut modules, "libkernel.prx", 0x700000)
            .unwrap();
        let provider = native
            .register_string_module(&mut modules, "strings.prx", 0x710000)
            .unwrap();
        file[0x500..0x540].fill(0);
        file[0x500..0x500 + symbol.len()].copy_from_slice(symbol.as_bytes());
        file[0x540..0x548].fill(0xff);
        // SAFETY: native verification uses its own matching runtime archive.
        unsafe {
            native
                .compile_image(
                    &file,
                    0x100000,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &directory,
                )
                .unwrap();
        }
        let caller = modules
            .load_dynamic(
                "lookup.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        let mut biases = std::iter::empty();
        // SAFETY: same archive; dlsym must not consume a load address.
        unsafe {
            modules.start_native_with_files(
                caller,
                &mut native,
                Arguments {
                    size: if invalid_handle { u64::MAX } else { provider },
                    pointer: if bad_pointer { u64::MAX } else { 0x100500 },
                },
                &mut ModuleFiles::default(),
                LoadPlan {
                    biases: &mut biases,
                    max_requests: 1,
                    instruction_limit: 100,
                    archive: &archive,
                    directory: &directory,
                },
            )
        }
        .unwrap();
        let data = &modules.image(caller).unwrap().segments[1].bytes;
        assert_eq!(&data[0x48..0x4c], &expected.to_le_bytes());
        assert_eq!(native.bytes_allocated(), 0);
        assert_eq!(
            &data[0x40..0x48],
            &if expected == 0 { 0x710040u64 } else { u64::MAX }.to_le_bytes()
        );
        native.remove_image_translations(0x100000);
    }
    println!(
        "PASS: ARM64 dlsym resolves a module-owned export and writes its guest address; missing symbols, stale handles and invalid pointers return errors without output corruption"
    );
}

fn guest_driven_load() {
    use native_ps5::{
        module_executor::{NativeExecutor, imported_service_fixture},
        module_files::ModuleFiles,
        modules::{LoadConfig, StartEvent},
    };
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory =
        PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap()).join("guest-load");
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("child.elf");
    std::fs::write(&source, native_ps5::modules::lifecycle_fixture(false)).unwrap();
    let mut files = ModuleFiles::default();
    files
        .register(
            "/app0/child.prx",
            &source,
            "child.prx",
            EntryPoints {
                start: Some(0x100),
                stop: Some(0x110),
            },
        )
        .unwrap();
    let mut file = imported_service_fixture();
    let name = "sceKernelLoadStartModule";
    let strings = format!("\0{name}\0libkernel.prx\0");
    file[0x600..0x600 + strings.len()].copy_from_slice(strings.as_bytes());
    for (offset, value) in [
        (0x328, 0x600u64),
        (0x338, strings.len() as u64),
        (0x378, name.len() as u64 + 2),
    ] {
        file[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    // xor edx,edx; lea r9,[rdi+40h]; call [relocated kernel import];
    // save the returned handle; xor eax,eax; ret.
    let code = [
        0x31, 0xd2, 0x4c, 0x8d, 0x4f, 0x40, 0xff, 0x15, 0x74, 0x04, 0, 0, 0x48, 0x89, 0x47, 0x48,
        0x31, 0xc0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let path = b"/app0/child.prx\0";
    file[0x500..0x500 + path.len()].copy_from_slice(path);
    file[0x540..0x544].fill(0xff);
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 100).unwrap();
    let mut modules = Modules::new(65536);
    native
        .register_loader_module(&mut modules, "libkernel.prx", 0x700000)
        .unwrap();
    // SAFETY: test harness supplies its own matching freshly built runtime archive.
    unsafe {
        native
            .compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    let caller = modules
        .load_dynamic(
            "caller.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    assert!(matches!(
        modules
            .start_native(
                caller,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: 0
                }
            )
            .unwrap(),
        StartEvent::LoadStartModule { .. }
    ));
    assert!(modules.finish_native_load(caller, &mut native).is_err());
    let before = modules.bytes();
    modules
        .with_pending_memory(caller, &mut native, |memory| {
            memory.write(0x100500, b"/app0/absent.prx\0")
        })
        .unwrap();
    assert!(
        unsafe {
            modules.begin_native_load(
                caller,
                &mut native,
                &mut files,
                LoadConfig {
                    bias: 0x200000,
                    instruction_limit: 100,
                    archive: &archive,
                    directory: &directory,
                },
            )
        }
        .is_err()
    );
    assert_eq!(modules.bytes(), before);
    modules
        .with_pending_memory(caller, &mut native, |memory| memory.write(0x100500, path))
        .unwrap();
    // SAFETY: same matching archive contract; guest paths only index the catalog.
    let (child, event) = unsafe {
        modules.begin_native_load(
            caller,
            &mut native,
            &mut files,
            LoadConfig {
                bias: 0x200000,
                instruction_limit: 100,
                archive: &archive,
                directory: &directory,
            },
        )
    }
    .unwrap();
    assert!(matches!(event, StartEvent::Started));
    assert!(
        modules
            .stop(child, &mut native, Arguments::default())
            .is_err()
    );
    assert!(
        unsafe {
            modules.begin_native_load(
                caller,
                &mut native,
                &mut files,
                LoadConfig {
                    bias: 0x300000,
                    instruction_limit: 100,
                    archive: &archive,
                    directory: &directory,
                },
            )
        }
        .is_err()
    );
    assert!(matches!(
        modules.finish_native_load(caller, &mut native).unwrap(),
        StartEvent::Started
    ));
    let data = &modules.image(caller).unwrap().segments[1].bytes;
    assert_eq!(&data[0x40..0x44], &[0; 4]);
    assert_eq!(&data[0x48..0x50], &child.to_le_bytes());
    modules
        .stop(caller, &mut native, Arguments::default())
        .unwrap();
    modules.unload(caller).unwrap();
    modules
        .stop(child, &mut native, Arguments::default())
        .unwrap();
    modules.unload(child).unwrap();
    native.remove_image_translations(0x200000);
    let mut missing = Modules::new(65536);
    missing
        .register_native("libkernel.prx", &[("sceKernelLoadStartModule", 0x700000)])
        .unwrap();
    let caller = missing
        .load_dynamic(
            "missing-caller.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    missing
        .start_native(
            caller,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
        )
        .unwrap();
    missing
        .with_pending_memory(caller, &mut native, |memory| {
            memory.write(0x100500, b"/app0/absent.prx\0")
        })
        .unwrap();
    let before = missing.bytes();
    // SAFETY: same trusted runtime archive; a missing path must not compile.
    let event = unsafe {
        missing.dispatch_native_load(
            caller,
            &mut native,
            &mut files,
            LoadConfig {
                bias: 0x200000,
                instruction_limit: 100,
                archive: &archive,
                directory: &directory,
            },
        )
    }
    .unwrap();
    assert!(matches!(
        event,
        native_ps5::modules::LoadProgress::Caller {
            event: StartEvent::Started
        }
    ));
    assert_eq!(missing.bytes(), before);
    let data = &missing.image(caller).unwrap().segments[1].bytes;
    assert_eq!(&data[0x48..0x50], &0x80020002u64.to_le_bytes());
    assert_eq!(&data[0x40..0x44], &[0xff; 4]);
    assert!(missing.resume_native(caller, &mut native, 0).is_err());
    missing
        .stop(caller, &mut native, Arguments::default())
        .unwrap();
    missing.unload(caller).unwrap();
    // Compilation failure must retain the published child and keep the caller
    // suspended/fault-pinned; it must not fabricate a successful loader return.
    let mut failed = Modules::new(65536);
    failed
        .register_native("libkernel.prx", &[("sceKernelLoadStartModule", 0x700000)])
        .unwrap();
    let caller = failed
        .load_dynamic(
            "caller.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    failed
        .start_native(
            caller,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
        )
        .unwrap();
    assert!(
        unsafe {
            failed.begin_native_load(
                caller,
                &mut native,
                &mut files,
                LoadConfig {
                    bias: 0x200000,
                    instruction_limit: 1,
                    archive: &archive,
                    directory: &directory,
                },
            )
        }
        .is_err()
    );
    assert_eq!(failed.phase(caller + 1), Some(Phase::Faulted));
    assert!(failed.finish_native_load(caller, &mut native).is_err());
    assert!(failed.unload(caller + 1).is_err());
    assert!(failed.unload(caller).is_err());
    let mut middle = file.clone();
    let leaf_path = b"/app0/leaf.prx\0";
    middle[0x500..0x500 + leaf_path.len()].copy_from_slice(leaf_path);
    // lea rdi,[rip+2f9h]; xor esi,esi; jmp 100h. Module start receives
    // size/argp, so it must explicitly obtain its own embedded loader path.
    let setup = [
        0x48, 0x8d, 0x3d, 0xf9, 0x02, 0, 0, 0x31, 0xf6, 0xe9, 0xf2, 0xfe, 0xff, 0xff,
    ];
    middle[0x200..0x200 + setup.len()].copy_from_slice(&setup);
    let middle_source = directory.join("middle.elf");
    std::fs::write(&middle_source, &middle).unwrap();
    files
        .register(
            "/app0/middle.prx",
            &middle_source,
            "middle.prx",
            EntryPoints {
                start: Some(0x200),
                stop: None,
            },
        )
        .unwrap();
    files
        .register(
            "/app0/leaf.prx",
            &source,
            "leaf.prx",
            EntryPoints {
                start: Some(0x100),
                stop: Some(0x110),
            },
        )
        .unwrap();
    let mut root = file.clone();
    let middle_path = b"/app0/middle.prx\0";
    root[0x500..0x500 + middle_path.len()].copy_from_slice(middle_path);
    for max_requests in [1, 2] {
        let mut chain = Modules::new(65536);
        chain
            .register_native("libkernel.prx", &[("sceKernelLoadStartModule", 0x700000)])
            .unwrap();
        let root_handle = chain
            .load_dynamic(
                "root.prx",
                &root,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        let mut biases = [0x200000, 0x300000].into_iter();
        // SAFETY: same matching archive used by the verification harness.
        let result = unsafe {
            chain.start_native_with_files(
                root_handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: 0,
                },
                &mut files,
                native_ps5::modules::LoadPlan {
                    biases: &mut biases,
                    max_requests,
                    instruction_limit: 100,
                    archive: &archive,
                    directory: &directory,
                },
            )
        };
        if max_requests == 1 {
            assert!(result.err().unwrap().contains("request budget"));
            assert_eq!(chain.phase(2), Some(Phase::Faulted));
            assert_eq!(chain.phase(3), Some(Phase::Faulted));
            assert_eq!(chain.phase(4), None);
            assert!(chain.unload(2).is_err());
            assert!(chain.unload(3).is_err());
        } else {
            result.unwrap();
            for (id, returned) in [(2, 3u64), (3, 4u64)] {
                assert_eq!(chain.phase(id), Some(Phase::Started));
                let data = &chain.image(id).unwrap().segments[1].bytes;
                assert_eq!(&data[0x48..0x50], &returned.to_le_bytes());
                assert_eq!(&data[0x40..0x44], &[0; 4]);
            }
            assert!(chain.stop(4, &mut native, Arguments::default()).is_err());
            for id in [2, 3, 4] {
                chain.stop(id, &mut native, Arguments::default()).unwrap();
                chain.unload(id).unwrap();
            }
        }
        native.remove_image_translations(0x200000);
        native.remove_image_translations(0x300000);
    }
    std::fs::remove_file(middle_source).unwrap();
    native.remove_image_translations(0x100000);
    let mut unload_file = file.clone();
    let unload_code = [
        0x48, 0x89, 0x47, 0x48, 0x49, 0x89, 0xfc, 0x89, 0xc7, 0x49, 0xbb, 0x10, 0, 0x70, 0, 0, 0,
        0, 0, 0x41, 0xff, 0xd3, 0x4c, 0x89, 0xe7, 0x48, 0x89, 0x47, 0x50, 0x31, 0xc0, 0xc3,
    ];
    unload_file[0x10c..0x10c + unload_code.len()].copy_from_slice(&unload_code);
    // SAFETY: matching runtime archive and generated checked guest code.
    unsafe {
        native
            .compile_image(
                &unload_file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
            )
            .unwrap();
    }
    let mut unloading = Modules::new(65536);
    unloading
        .register_native(
            "libkernel.prx",
            &[
                ("sceKernelLoadStartModule", 0x700000),
                ("sceKernelStopUnloadModule", 0x700010),
            ],
        )
        .unwrap();
    let caller = unloading
        .load_dynamic(
            "unloading.prx",
            &unload_file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    let initial_bytes = unloading.bytes();
    let mut biases = [0x200000].into_iter();
    // SAFETY: matching archive. Driver must consume the stop request without
    // asking for another load address, run finalizers and release child code.
    unsafe {
        unloading.start_native_with_files(
            caller,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
            &mut files,
            native_ps5::modules::LoadPlan {
                biases: &mut biases,
                max_requests: 2,
                instruction_limit: 100,
                archive: &archive,
                directory: &directory,
            },
        )
    }
    .unwrap();
    assert_eq!(unloading.phase(caller), Some(Phase::Started));
    assert_eq!(unloading.phase(caller + 1), None);
    assert_eq!(unloading.bytes(), initial_bytes);
    assert!(
        unloading
            .stop_dynamic_module(caller, caller + 1, &mut native, Arguments::default())
            .is_err()
    );
    assert_eq!(
        &unloading.image(caller).unwrap().segments[1].bytes[0x50..0x58],
        &[0; 8]
    );
    unloading
        .stop(caller, &mut native, Arguments::default())
        .unwrap();
    unloading.unload(caller).unwrap();
    files
        .register(
            "/app0/failstop.prx",
            &source,
            "failstop.prx",
            EntryPoints {
                start: Some(0x100),
                stop: Some(0x120),
            },
        )
        .unwrap();
    let path = b"/app0/failstop.prx\0";
    unload_file[0x500..0x500 + path.len()].copy_from_slice(path);
    let mut stop_failure = Modules::new(65536);
    stop_failure
        .register_native(
            "libkernel.prx",
            &[
                ("sceKernelLoadStartModule", 0x700000),
                ("sceKernelStopUnloadModule", 0x700010),
            ],
        )
        .unwrap();
    let caller = stop_failure
        .load_dynamic(
            "stop-failure.prx",
            &unload_file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    let mut biases = [0x200000].into_iter();
    // SAFETY: matching trusted runtime archive; configured stop returns 7.
    let result = unsafe {
        stop_failure.start_native_with_files(
            caller,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
            &mut files,
            native_ps5::modules::LoadPlan {
                biases: &mut biases,
                max_requests: 2,
                instruction_limit: 100,
                archive: &archive,
                directory: &directory,
            },
        )
    };
    assert!(result.err().unwrap().contains("status 7"));
    assert_eq!(stop_failure.phase(caller), Some(Phase::Faulted));
    assert_eq!(stop_failure.phase(caller + 1), Some(Phase::Faulted));
    assert!(stop_failure.image(caller + 1).is_some());
    assert!(stop_failure.unload(caller + 1).is_err());
    assert!(stop_failure.unload(caller).is_err());
    drop(stop_failure);
    native.remove_image_translations(0x200000);
    native.remove_image_translations(0x100000);
    std::fs::remove_file(source).unwrap();
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir(directory).unwrap();
    println!(
        "PASS: translated guest requests a catalog ELF; Rust loads/compiles/initializes it, pins it, writes status and returns its real handle before reverse-order teardown"
    );
}

fn resumable_lifecycle() {
    use native_ps5::{module_executor::NativeExecutor, modules::StartEvent};
    let file = native_ps5::modules::lifecycle_fixture(false);
    let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
    let functions: [(usize, usize, unsafe extern "C" fn(*mut Cpu)); 6] = [
        (0x100, 3, rust_module_start),
        (0x110, 3, rust_module_stop),
        (0x120, 6, rust_module_init),
        (0x130, 6, rust_module_init_array),
        (0x140, 6, rust_module_fini),
        (0x150, 6, rust_module_fini_array),
    ];
    for (offset, length, function) in functions {
        // SAFETY: these generated register-only blocks use the same CPU ABI and
        // RET boundary; they never access either retained or Rust memory helpers.
        unsafe {
            native
                .register(offset as u64, &file[offset..offset + length], function)
                .unwrap();
        }
    }
    for status in [0, 7] {
        let mut modules = Modules::new(65536);
        let handle = modules
            .load_dynamic(
                "lifecycle.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: Some(0x110),
                },
            )
            .unwrap();
        let result = modules.start_native(
            handle,
            &mut native,
            Arguments {
                size: status,
                pointer: 0,
            },
        );
        if status == 0 {
            assert!(matches!(result.unwrap(), StartEvent::Started));
            modules
                .stop(handle, &mut native, Arguments::default())
                .unwrap();
            modules.unload(handle).unwrap();
        } else {
            assert!(result.err().unwrap().contains("status 7"));
            assert_eq!(modules.phase(handle), Some(Phase::Faulted));
            assert!(modules.unload(handle).is_err());
        }
    }
    println!(
        "PASS: resumable lifecycle executes real ARM64 constructors, ignores void return values, enforces explicit start status and permits finalization after success"
    );
}

fn loader_suspension() {
    use native_ps5::loader::Image;
    use native_ps5::module_executor::{
        Execution, NativeExecutor, SERVICE_CALL_CODE, SERVICE_RETURN_CODE, Service, memory_fixture,
    };
    let mut file = memory_fixture();
    file[0x100..0x111].copy_from_slice(SERVICE_CALL_CODE);
    file[0x111..0x118].copy_from_slice(SERVICE_RETURN_CODE);
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 20).unwrap();
    // SAFETY: linked ARM64 blocks are generated from these exact guest bytes.
    unsafe {
        native
            .register(0x100, SERVICE_CALL_CODE, rust_module_service_call)
            .unwrap();
        native
            .register(0x111, SERVICE_RETURN_CODE, rust_module_service_return)
            .unwrap();
    }
    native
        .register_service(0x700000, Service::LoadStartModule)
        .unwrap();
    for bounded in [false, true] {
        native.transitions = if bounded { 2 } else { 20 };
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let Execution::LoadStartModule {
            arguments,
            mut continuation,
        } = native
            .run(
                &mut image,
                0x100100,
                Arguments {
                    size: 0x100500,
                    pointer: 0x100520,
                },
                &mut [],
            )
            .unwrap()
        else {
            panic!("missing loader request")
        };
        assert_eq!(arguments, [0x100500, 0x100520, 3, 0, 0, 0]);
        continuation
            .with_memory(&mut image, &mut [], |memory| {
                assert_eq!(memory.load(0x903ff8, 8)?, u64::MAX);
                memory.write(0x900100, b"/app0/test.prx\0")?;
                let request = native_ps5::module_executor::LoadRequest::read(
                    memory,
                    [0x900100, 3, 0x100520, 0, 0, 0x100530],
                )?;
                assert_eq!(request.path, "/app0/test.prx");
                memory.store(request.result_address, 42, 4)?;
                assert!(memory.store(0x100100, 1, 4).is_err());
                assert!(memory.store(0x903fff, 1, 4).is_err());
                Ok(())
            })
            .unwrap();
        // A second execution cannot erase the suspended call's stack or budgets.
        native.transitions = 100;
        native.fuel = 1000;
        let mut nested = Image::load(&file, 0x200000, 65536).unwrap();
        assert!(matches!(
            native
                .run(&mut nested, 0x200100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::LoadStartModule { .. }
        ));
        assert!(
            continuation
                .with_memory(&mut nested, &mut [], |_| Ok(()))
                .is_err()
        );
        continuation
            .with_memory(&mut image, &mut [], |memory| {
                assert_eq!(memory.read(0x900100, 15)?, b"/app0/test.prx\0");
                assert_eq!(memory.load(0x100530, 4)?, 42);
                Ok(())
            })
            .unwrap();
        let result = native.resume(&mut image, &mut [], *continuation, 0x12345678);
        if bounded {
            assert!(result.err().unwrap().contains("transition budget"));
            assert_eq!(&image.segments[1].bytes[0x40..0x48], &[0; 8]);
        } else {
            assert!(matches!(result.unwrap(), Execution::Returned(0)));
            assert_eq!(
                &image.segments[1].bytes[0x40..0x48],
                &0x12345678u64.to_le_bytes()
            );
        }
    }
    for abandon in [false, true] {
        use native_ps5::modules::StartEvent;
        native.transitions = 100;
        let mut modules = Modules::new(65536);
        let handle = modules
            .load_dynamic(
                "pending.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        let event = modules
            .start_native(
                handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: 0x100520,
                },
            )
            .unwrap();
        assert!(matches!(
            event,
            StartEvent::LoadStartModule {
                arguments: [0x100500, 0x100520, 3, 0, 0, 0]
            }
        ));
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert!(
            modules
                .start_native(handle, &mut native, Arguments::default())
                .is_err()
        );
        assert!(modules.unload(handle).is_err());
        assert!(
            modules
                .stop(handle, &mut native, Arguments::default())
                .is_err()
        );
        assert!(
            modules
                .with_pending_memory(handle, &mut native, |memory| memory.store(0, 1, 4))
                .is_err()
        );
        modules
            .with_pending_memory(handle, &mut native, |memory| memory.store(0x100530, 7, 4))
            .unwrap();
        if abandon {
            modules.abandon_native_start(handle).unwrap();
            assert!(modules.resume_native(handle, &mut native, 0).is_err());
            assert!(modules.unload(handle).is_err());
        } else {
            assert!(matches!(
                modules.resume_native(handle, &mut native, 0xabc).unwrap(),
                StartEvent::Started
            ));
            assert_eq!(modules.phase(handle), Some(Phase::Started));
            assert_eq!(
                &modules.image(handle).unwrap().segments[1].bytes[0x40..0x48],
                &0xabcu64.to_le_bytes()
            );
            assert!(modules.resume_native(handle, &mut native, 0).is_err());
            modules
                .stop(handle, &mut native, Arguments::default())
                .unwrap();
            modules.unload(handle).unwrap();
        }
    }
    println!(
        "PASS: ARM64 loader suspension preserves CPU/stack/budgets; module lifecycle owns requests, rejects duplicate resumes, protects pending images and retains abandoned faults"
    );
}

fn automatic_compilation() {
    use native_ps5::module_executor::{NativeExecutor, imported_service_fixture};
    use std::{collections::BTreeSet, path::PathBuf};
    let archive = PathBuf::from(
        std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").expect("matching runtime archive"),
    );
    let directory = PathBuf::from(
        std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").expect("native compilation output"),
    ).join(format!("compiler-lifecycle-{}", std::process::id()));
    // Inspect only this fixture's generated files; other native verifiers may
    // retain input ELF fixtures in the shared AOT parent directory.
    std::fs::create_dir_all(&directory).unwrap();
    let file = imported_service_fixture();
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 100).unwrap();
    // SAFETY: the verification runner supplies the same freshly built archive
    // used to link this test, with the identical Cpu and Memory layouts.
    let manifest = unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            100,
            &archive,
            &directory,
        )
    }
    .unwrap();
    assert!(manifest.untranslated.is_empty());
    assert!(manifest.block_count >= 2);
    let mut modules = Modules::new(65536);
    native
        .register_string_module(&mut modules, "libstrings.prx", 0x710000)
        .unwrap();
    let handle = modules
        .load_dynamic(
            "automatic.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    modules
        .start(
            handle,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0x100520,
            },
        )
        .unwrap();
    assert_eq!(
        &modules.image(handle).unwrap().segments[1].bytes[..7],
        b"linked\0"
    );
    assert!(
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
            )
        }
        .is_err()
    );
    modules
        .stop(handle, &mut native, Arguments::default())
        .unwrap();
    modules.unload(handle).unwrap();
    let interior = modules
        .load_dynamic(
            "automatic-interior.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x105),
                stop: None,
            },
        )
        .unwrap();
    modules
        .start(
            interior,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0x100520,
            },
        )
        .unwrap();
    assert_eq!(
        &modules.image(interior).unwrap().segments[1].bytes[..7],
        b"linked\0"
    );
    modules
        .stop(interior, &mut native, Arguments::default())
        .unwrap();
    modules.unload(interior).unwrap();
    native.remove_image_translations(0x100000);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    assert!(
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                1,
                &archive,
                &directory,
            )
        }
        .is_err()
    );
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
    println!(
        "PASS: ELF discovery automatically compiles/loads ARM64 blocks and executes a relocated import; duplicate/budget rejection and executable-library cleanup verified"
    );
}

fn cross_module_execution() {
    use native_ps5::module_executor::{
        IMPORT_CALL_CODE, MEMORY_FIXTURE_CODE, NativeExecutor, SERVICE_RETURN_CODE,
        imported_service_fixture, memory_fixture,
    };
    for (fault, automatic) in [(false, false), (true, false), (false, true)] {
        let mut middle_file = imported_service_fixture();
        middle_file[0x451..0x457].copy_from_slice(b"writer");
        middle_file[0x458..0x466].copy_from_slice(b"leafmodule.prx");
        let mut caller_file = imported_service_fixture();
        caller_file[0x451..0x457].copy_from_slice(b"middle");
        let mut native = NativeExecutor::new(0x900000, 16384, 100, 100).unwrap();
        if automatic {
            let archive =
                std::path::PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
            let directory =
                std::path::PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
            let leaf_file = memory_fixture();
            for (bias, file) in [
                (0x100000, &caller_file[..]),
                (0x200000, &middle_file[..]),
                (0x300000, &leaf_file[..]),
            ] {
                // SAFETY: runner-provided archive matches this runtime build.
                let report = unsafe {
                    native.compile_image(
                        file,
                        bias,
                        std::collections::BTreeSet::from([0x100]),
                        100,
                        &archive,
                        &directory,
                    )
                }
                .unwrap();
                assert!(report.untranslated.is_empty());
            }
        } else {
            // SAFETY: each image uses matching emitted byte ranges. Identical offsets
            // in different images intentionally refer to distinct registered code.
            unsafe {
                for bias in [0x100000, 0x200000] {
                    native
                        .register_in_image(bias, 0x100, IMPORT_CALL_CODE, rust_module_import_call)
                        .unwrap();
                    native
                        .register_in_image(
                            bias,
                            0x10b,
                            SERVICE_RETURN_CODE,
                            rust_module_import_return,
                        )
                        .unwrap();
                }
                native
                    .register_in_image(0x300000, 0x100, MEMORY_FIXTURE_CODE, rust_module_memory)
                    .unwrap();
            }
        }
        let mut modules = Modules::new(65536);
        let leaf = modules
            .load(
                "leafmodule.prx",
                &memory_fixture(),
                0x300000,
                &[("writer", 0x100)],
                EntryPoints::default(),
            )
            .unwrap();
        let middle = modules
            .load(
                "libstrings.prx",
                &middle_file,
                0x200000,
                &[("middle", 0x100)],
                EntryPoints::default(),
            )
            .unwrap();
        let caller = modules
            .load_dynamic(
                "caller.prx",
                &caller_file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        assert!(
            modules
                .start(caller, &mut native, Arguments::default())
                .is_err()
        );
        assert_eq!(modules.phase(caller), Some(Phase::Loaded));
        modules
            .start(leaf, &mut native, Arguments::default())
            .unwrap();
        modules
            .start(middle, &mut native, Arguments::default())
            .unwrap();
        let result = modules.start(
            caller,
            &mut native,
            Arguments {
                size: if fault { 0x100100 } else { 0x100500 },
                pointer: 0x1122334455667788,
            },
        );
        if fault {
            assert!(result.is_err());
            assert_eq!(modules.phase(caller), Some(Phase::Faulted));
            assert!(
                modules
                    .stop(middle, &mut native, Arguments::default())
                    .is_err()
            );
            assert!(modules.unload(caller).is_err());
        } else {
            result.unwrap();
            let data = &modules.image(caller).unwrap().segments[1].bytes;
            assert_eq!(&data[..8], &0x1122334455667788u64.to_le_bytes());
            assert!(
                modules
                    .stop(middle, &mut native, Arguments::default())
                    .is_err()
            );
            modules
                .stop(caller, &mut native, Arguments::default())
                .unwrap();
            modules.unload(caller).unwrap();
            modules
                .stop(middle, &mut native, Arguments::default())
                .unwrap();
            modules.unload(middle).unwrap();
            modules
                .stop(leaf, &mut native, Arguments::default())
                .unwrap();
            modules.unload(leaf).unwrap();
            assert_eq!(modules.bytes(), 0);
        }
    }
    println!(
        "PASS: Rust executor follows a three-image ELF import chain, switches load biases, shares checked data and restores nested returns; faulted consumers pin providers"
    );
}

fn imported_service_execution() {
    use native_ps5::module_executor::{
        IMPORT_CALL_CODE, NativeExecutor, SERVICE_RETURN_CODE, imported_service_fixture,
    };
    let file = imported_service_fixture();
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 20).unwrap();
    // SAFETY: exact registered guest bytes use the Rust executor/memory ABI.
    unsafe {
        native
            .register(0x100, IMPORT_CALL_CODE, rust_module_import_call)
            .unwrap();
        native
            .register(0x10b, SERVICE_RETURN_CODE, rust_module_import_return)
            .unwrap();
    }
    let mut modules = Modules::new(65536);
    assert!(
        modules
            .load_dynamic("missing.prx", &file, 0x100000, EntryPoints::default())
            .is_err()
    );
    assert_eq!(modules.bytes(), 0);
    let provider = native
        .register_string_module(&mut modules, "libstrings.prx", 0x710000)
        .unwrap();
    assert_eq!(modules.phase(provider), Some(Phase::Started));
    let handle = modules
        .load_dynamic(
            "imported.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )
        .unwrap();
    let image = modules.image(handle).unwrap();
    assert!(image.unresolved.is_empty());
    assert_eq!(image.relocations, [(0x100580, 0x710000)]);
    modules
        .start(
            handle,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0x100520,
            },
        )
        .unwrap();
    let data = &modules.image(handle).unwrap().segments[1].bytes;
    assert_eq!(&data[..7], b"linked\0");
    assert_eq!(
        u64::from_le_bytes(data[0x40..0x48].try_into().unwrap()),
        0x100500
    );
    modules
        .stop(handle, &mut native, Arguments::default())
        .unwrap();
    modules.unload(handle).unwrap();
    assert_eq!(modules.bytes(), 0);
    println!(
        "PASS: Rust ELF import relocation binds a registered string module; ARM64 indirect call invokes Rust strcpy and returns correctly"
    );
}

fn service_execution() {
    use native_ps5::module_executor::{
        NativeExecutor, SERVICE_CALL_CODE, SERVICE_RETURN_CODE, Service, memory_fixture,
    };
    let mut file = memory_fixture();
    file[0x100..0x111].copy_from_slice(SERVICE_CALL_CODE);
    file[0x111..0x118].copy_from_slice(SERVICE_RETURN_CODE);
    file[0x500..0x506].copy_from_slice(b"hello\0");
    file[0x520..0x527].copy_from_slice(b" world\0");
    let executor = |service| {
        let mut native = NativeExecutor::new(0x900000, 16384, 100, 20).unwrap();
        // SAFETY: linked blocks use the exact bytes and Rust module ABI.
        unsafe {
            native
                .register(0x100, SERVICE_CALL_CODE, rust_module_service_call)
                .unwrap();
            native
                .register(0x111, SERVICE_RETURN_CODE, rust_module_service_return)
                .unwrap();
        }
        if let Some(service) = service {
            native.register_service(0x700000, service).unwrap();
        }
        native
    };
    let entries = EntryPoints {
        start: Some(0x100),
        stop: None,
    };
    for (service, expected, text) in [
        (Service::Strcpy, 0x100500, &b" world\0"[..]),
        (Service::Strcat, 0x100500, &b"hello world\0"[..]),
        (Service::Strcmp, 72, &b"hello\0"[..]),
        (Service::Strncmp, 72, &b"hello\0"[..]),
        (Service::Strlen, 5, &b"hello\0"[..]),
    ] {
        let mut native = executor(Some(service));
        assert!(native.register_service(0x700000, Service::Strlen).is_err());
        let mut modules = Modules::new(65536);
        let handle = modules
            .load_dynamic("service.prx", &file, 0x100000, entries)
            .unwrap();
        modules
            .start(
                handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: 0x100520,
                },
            )
            .unwrap();
        let data = &modules.image(handle).unwrap().segments[1].bytes;
        assert_eq!(&data[..text.len()], text);
        assert_eq!(
            u64::from_le_bytes(data[0x40..0x48].try_into().unwrap()),
            expected
        );
    }
    for (destination, registered, fuel) in [
        (0x100100, true, 100),
        (0x1005fd, true, 100),
        (0x903ff0, true, 100),
        (0x100500, false, 100),
        (0x100500, true, 3),
    ] {
        let mut native = executor(if registered {
            Some(Service::Strcpy)
        } else {
            None
        });
        native.fuel = fuel;
        let mut modules = Modules::new(65536);
        let handle = modules
            .load_dynamic("fault.prx", &file, 0x100000, entries)
            .unwrap();
        assert!(
            modules
                .start(
                    handle,
                    &mut native,
                    Arguments {
                        size: destination,
                        pointer: 0x100520
                    }
                )
                .is_err()
        );
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert_eq!(
            &modules.image(handle).unwrap().segments[1].bytes,
            &file[0x500..0x600]
        );
    }
    let mut native = executor(None);
    native.register_service(0x100500, Service::Strcpy).unwrap();
    let mut modules = Modules::new(65536);
    let handle = modules
        .load_dynamic("overlap.prx", &file, 0x100000, entries)
        .unwrap();
    let error = modules
        .start(handle, &mut native, Arguments::default())
        .unwrap_err();
    assert!(error.contains("Service thunk overlaps"));
    println!(
        "PASS: translated ARM64 CALLs reach five Rust string services and return through a checked guest frame; bad outputs, unknown services, fuel and return corruption fault"
    );
}

fn vector_execution() {
    use native_ps5::module_executor::{NativeExecutor, VECTOR_FIXTURE_CODE, memory_fixture};
    for width in [16usize, 32] {
        let mut code = VECTOR_FIXTURE_CODE.to_vec();
        let function: unsafe extern "C" fn(*mut Cpu) = if width == 16 {
            code[5] = 0xf8;
            rust_module_vector128
        } else {
            rust_module_vector256
        };
        let mut file = memory_fixture();
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        for (i, byte) in file[0x500..0x600].iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(3);
        }
        let mut native = NativeExecutor::new(0x900000, 16384, 100, 10).unwrap();
        // SAFETY: the linked block is emitted for this width and exact bytes.
        unsafe {
            native.register(0x100, &code, function).unwrap();
        }
        for (source, destination, success) in [
            (0x100501, 0x100580, true),
            (0x1005f0, 0x100580, false),
            (0x100501, 0x1005f0, false),
            (0x100501, 0x100100, false),
        ] {
            let mut modules = Modules::new(65536);
            let handle = modules
                .load_dynamic(
                    "vector.prx",
                    &file,
                    0x100000,
                    EntryPoints {
                        start: Some(0x100),
                        stop: None,
                    },
                )
                .unwrap();
            let result = modules.start(
                handle,
                &mut native,
                Arguments {
                    size: destination,
                    pointer: source,
                },
            );
            let data = &modules.image(handle).unwrap().segments[1].bytes;
            if success {
                result.unwrap();
                let expected: Vec<_> = (0..32)
                    .map(|i| {
                        if i < width {
                            file[0x501 + i] ^ file[0x521 + i]
                        } else {
                            0
                        }
                    })
                    .collect();
                assert_eq!(&data[0x80..0xa0], expected);
            } else {
                assert!(result.is_err());
                assert_eq!(data, &file[0x500..0x600]);
                assert_eq!(modules.phase(handle), Some(Phase::Faulted));
            }
        }
    }
    println!(
        "PASS: native ARM64 VEX128/256 loads, XOR and stores use Rust memory; upper-half clearing and fault atomicity verified"
    );
}

fn shift_execution() {
    use native_ps5::module_executor::{NativeExecutor, SHIFT_FIXTURE_CODE, memory_fixture};
    let mut file = memory_fixture();
    file[0x100..0x100 + SHIFT_FIXTURE_CODE.len()].copy_from_slice(SHIFT_FIXTURE_CODE);
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 10).unwrap();
    // SAFETY: emitted ARM64 has exactly the registered code and Rust helper ABI.
    unsafe {
        native
            .register(0x100, SHIFT_FIXTURE_CODE, rust_module_shift)
            .unwrap();
    }
    let mut modules = Modules::new(65536);
    for input in [0u64, 1, u64::MAX, 0x123456789abcdef0, 1 << 63] {
        let handle = modules
            .load_dynamic(
                "shift.prx",
                &file,
                0x100000,
                EntryPoints {
                    start: Some(0x100),
                    stop: None,
                },
            )
            .unwrap();
        modules
            .start(
                handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: input,
                },
            )
            .unwrap();
        let data = &modules.image(handle).unwrap().segments[1].bytes;
        assert_eq!(
            &data[..8],
            &input.wrapping_shl(7).rotate_right(5).to_le_bytes()
        );
        modules
            .stop(handle, &mut native, Arguments::default())
            .unwrap();
        modules.unload(handle).unwrap();
    }
    println!(
        "PASS: Rust module executor runs ARM64 shifts/rotates with Rust flags and memory helpers"
    );
}

fn branch_execution() {
    use native_ps5::module_executor::{LOOP_FIXTURE_BLOCKS, NativeExecutor, memory_fixture};
    let mut file = memory_fixture();
    let mut native = NativeExecutor::new(0x900000, 16384, 10000, 100).unwrap();
    let functions: [unsafe extern "C" fn(*mut Cpu); 4] = [
        rust_module_loop_entry,
        rust_module_loop_body,
        rust_module_loop_exit,
        rust_module_loop_zero,
    ];
    for (&(offset, bytes, _), function) in LOOP_FIXTURE_BLOCKS.iter().zip(functions) {
        file[offset as usize..offset as usize + bytes.len()].copy_from_slice(bytes);
        // SAFETY: each region is generated for the corresponding fixture bytes.
        unsafe {
            native.register(offset, bytes, function).unwrap();
        }
    }
    let mut modules = Modules::new(65536);
    let entries = EntryPoints {
        start: Some(0x100),
        stop: None,
    };
    for count in [0u64, 1, 31, 1000] {
        let handle = modules
            .load_dynamic("loop.prx", &file, 0x100000, entries)
            .unwrap();
        modules
            .start(
                handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: count,
                },
            )
            .unwrap();
        let data = &modules.image(handle).unwrap().segments[1].bytes;
        assert_eq!(&data[..8], &count.to_le_bytes());
        modules
            .stop(handle, &mut native, Arguments::default())
            .unwrap();
        modules.unload(handle).unwrap();
    }
    native.fuel = 20;
    let handle = modules
        .load_dynamic("bounded-loop.prx", &file, 0x100000, entries)
        .unwrap();
    assert!(
        modules
            .start(
                handle,
                &mut native,
                Arguments {
                    size: 0x100500,
                    pointer: u64::MAX
                }
            )
            .is_err()
    );
    assert_eq!(modules.phase(handle), Some(Phase::Faulted));
    assert!(
        modules.image(handle).unwrap().segments[1]
            .bytes
            .iter()
            .all(|&byte| byte == 0)
    );
    println!(
        "PASS: Rust conditions drive native ARM64 zero/nonzero paths and a 1000-iteration guest loop; runaway loops stop on fuel"
    );
}

fn memory_execution() {
    use native_ps5::module_executor::{MEMORY_FIXTURE_CODE, NativeExecutor, memory_fixture};
    let mut native = NativeExecutor::new(0x800000, 16384, 100, 100).unwrap();
    // SAFETY: linked code is emitted by translate_rust_memory for these exact bytes.
    unsafe {
        native
            .register(0x100, MEMORY_FIXTURE_CODE, rust_module_memory)
            .unwrap();
    }
    let entries = EntryPoints {
        start: Some(0x100),
        stop: None,
    };
    let mut modules = Modules::new(65536);
    let handle = modules
        .load_dynamic("memory.prx", &memory_fixture(), 0x100000, entries)
        .unwrap();
    modules
        .start(
            handle,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0x1122334455667788,
            },
        )
        .unwrap();
    let data = &modules.image(handle).unwrap().segments[1].bytes;
    assert_eq!(&data[..8], &0x1122334455667788u64.to_le_bytes());
    assert_eq!(modules.phase(handle), Some(Phase::Started));
    for (index, target) in [0x200100, 0x2005fc, u64::MAX].into_iter().enumerate() {
        let bias = 0x200000 + index as u64 * 0x100000;
        let target = if index == 1 {
            target + 0x100000
        } else {
            target
        };
        let handle = modules
            .load_dynamic(
                &format!("fault{index}.prx"),
                &memory_fixture(),
                bias,
                entries,
            )
            .unwrap();
        assert!(
            modules
                .start(
                    handle,
                    &mut native,
                    Arguments {
                        size: target,
                        pointer: 99
                    }
                )
                .is_err()
        );
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert!(
            modules.image(handle).unwrap().segments[1]
                .bytes
                .iter()
                .all(|&b| b == 0)
        );
    }
    let mut altered = memory_fixture();
    altered[0x100] = 0x90;
    let handle = modules
        .load_dynamic("changed.prx", &altered, 0x500000, entries)
        .unwrap();
    assert!(
        modules
            .start(
                handle,
                &mut native,
                Arguments {
                    size: 0x500500,
                    pointer: 99
                }
            )
            .is_err()
    );
    println!(
        "PASS: Rust module executor uses guarded Rust memory and a real guest stack; ARM64 writes, readonly/crossing faults and translation byte checks verified"
    );
    let mut file = memory_fixture();
    file[0x100..0x105].copy_from_slice(&[0xe8, 0x1b, 0, 0, 0]);
    file[0x105] = 0xc3;
    file[0x120..0x120 + MEMORY_FIXTURE_CODE.len()].copy_from_slice(MEMORY_FIXTURE_CODE);
    let mut native = NativeExecutor::new(0x900000, 16384, 100, 100).unwrap();
    // SAFETY: each linked block is emitted for these exact guest bytes/offsets.
    unsafe {
        native
            .register(0x100, &file[0x100..0x105], rust_module_nested_call)
            .unwrap();
        native
            .register(0x105, &file[0x105..0x106], rust_module_nested_return)
            .unwrap();
        native
            .register(0x120, MEMORY_FIXTURE_CODE, rust_module_nested_body)
            .unwrap();
    }
    let mut modules = Modules::new(65536);
    let handle = modules
        .load_dynamic("nested.prx", &file, 0x100000, entries)
        .unwrap();
    modules
        .start(
            handle,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 73,
            },
        )
        .unwrap();
    assert_eq!(modules.image(handle).unwrap().segments[1].bytes[0], 73);
    for (index, fuel, transitions) in [(0, 1, 100), (1, 100, 1)] {
        native.fuel = fuel;
        native.transitions = transitions;
        let bias = 0x200000 + index * 0x100000;
        let handle = modules
            .load_dynamic(&format!("bounded{index}.prx"), &file, bias, entries)
            .unwrap();
        assert!(
            modules
                .start(
                    handle,
                    &mut native,
                    Arguments {
                        size: bias + 0x500,
                        pointer: 73
                    }
                )
                .is_err()
        );
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert!(
            modules.image(handle).unwrap().segments[1]
                .bytes
                .iter()
                .all(|&v| v == 0)
        );
    }
    println!(
        "PASS: Rust dispatcher completes nested ARM64 guest CALL/RET and rejects exhausted instruction/transition budgets"
    );
}
