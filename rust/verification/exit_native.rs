//! Actual ARM64-translated noreturn calls. Exit requests retain pending cleanup.
use native_ps5::{
    loader::Image,
    module_executor::{Execution, ExitKind, ExitRequest, NativeExecutor, Service, memory_fixture},
    module_files::ModuleFiles,
    modules::{Arguments, EntryPoints, LoadPlan, Modules, Phase, StartOutcome},
};
use std::{collections::BTreeSet, path::Path};
fn thunk(address: u64, tail: bool) -> Vec<u8> {
    let mut code = vec![0x48, 0xb8];
    code.extend(address.to_le_bytes());
    if tail {
        code.extend([0x31, 0xe4, 0xff, 0xe0]);
    } else {
        code.extend([0xff, 0xd0]);
    }
    code.extend([0x0f, 0x0b]);
    code
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for (service, kind) in [
        (Service::Exit, ExitKind::Normal),
        (Service::ImmediateExit, ExitKind::Immediate),
    ] {
        for tail in [false, true] {
            for status in [0u64, 1, 0xdead_beef_ffff_ffff] {
                let mut file = memory_fixture();
                let code = thunk(0x700000, tail);
                file[0x100..0x100 + code.len()].copy_from_slice(&code);
                let mut suspend = vec![0x48, 0xb8];
                suspend.extend(0x700010u64.to_le_bytes());
                suspend.extend([0xff, 0xd0, 0xc3]);
                file[0x140..0x140 + suspend.len()].copy_from_slice(&suspend);
                let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
                native.register_service(0x700000, service)?;
                native.register_service(0x700010, Service::Dlsym)?;
                // SAFETY: caller supplies the matching locally built Rust archive.
                unsafe {
                    native.compile_image(
                        &file,
                        0x100000,
                        BTreeSet::from([0x100, 0x140]),
                        100,
                        archive,
                        directory,
                    )?;
                }
                let mut image = Image::load(&file, 0x100000, 65536)?;
                let Execution::Dlsym { continuation, .. } =
                    native.run(&mut image, 0x100140, Arguments::default(), &mut [])?
                else {
                    return Err("Expected suspended caller".into());
                };
                let execution = native.run(
                    &mut image,
                    0x100100,
                    Arguments {
                        size: status,
                        pointer: 0,
                    },
                    &mut [],
                )?;
                let expected = ExitRequest {
                    status: status as u32 as i32,
                    kind,
                };
                assert!(matches!(execution,Execution::ExitRequested(request) if request==expected));
                assert_eq!(native.exit_request(), Some(expected));
                assert!(native.exit_remaining_fuel().is_some_and(|fuel| fuel < 100));
                assert!(
                    native
                        .resume(&mut image, &mut [], *continuation, 0)
                        .err()
                        .unwrap()
                        .contains("exit requested")
                );
                assert!(
                    native
                        .run(&mut image, 0x100100, Arguments::default(), &mut [])
                        .err()
                        .unwrap()
                        .contains("exit requested")
                );
            }
        }
    }
    let mut caller_file = memory_fixture();
    let mut load = vec![0x48, 0xb8];
    load.extend(0x700010u64.to_le_bytes());
    load.extend([0xff, 0xd0, 0x31, 0xc0, 0xc3]);
    caller_file[0x100..0x100 + load.len()].copy_from_slice(&load);
    let path = b"/app0/exit-child.prx\0";
    caller_file[0x500..0x500 + path.len()].copy_from_slice(path);
    let mut child_file = native_ps5::modules::lifecycle_fixture(false);
    let init = thunk(0x700000, false);
    child_file[0x120..0x120 + init.len()].copy_from_slice(&init);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    native.register_service(0x700000, Service::Exit)?;
    native.register_service(0x700010, Service::LoadStartModule)?;
    // SAFETY: matching archive contract forwarded from this verifier's caller.
    unsafe {
        native.compile_image(
            &caller_file,
            0x100000,
            BTreeSet::from([0x100]),
            100,
            archive,
            directory,
        )?;
    }
    let mut modules = Modules::new(65536);
    let parent = modules.load_dynamic(
        "exit-parent.prx",
        &caller_file,
        0x100000,
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    let mut files = ModuleFiles::default();
    let child_path = directory.join(format!("exit-child-{}.elf", std::process::id()));
    std::fs::write(&child_path, &child_file).map_err(|error| error.to_string())?;
    files.register(
        "/app0/exit-child.prx",
        &child_path,
        "exit-child.prx",
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    let mut biases = [0x200000].into_iter();
    // SAFETY: same matching archive, including the dynamically loaded initializer.
    let outcome = unsafe {
        modules.start_native_with_files(
            parent,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0,
            },
            &mut files,
            LoadPlan {
                biases: &mut biases,
                max_requests: 4,
                instruction_limit: 100,
                archive,
                directory,
            },
        )?
    };
    let expected = ExitRequest {
        status: 0,
        kind: ExitKind::Normal,
    };
    assert_eq!(outcome, StartOutcome::ExitRequested(expected));
    assert_eq!(modules.exit_request(), Some(expected));
    assert_eq!(modules.phase(parent), Some(Phase::Faulted));
    assert!(
        modules
            .finish_native_load(parent, &mut native)
            .err()
            .unwrap()
            .contains("exit requested")
    );
    assert!(modules.unload(parent).is_err());
    println!(
        "PASS: 12 translated normal/immediate exit requests with signed statuses, UD2 continuation and invalid tail return stack; stale continuation rejected; nested initializer propagates exit without module-start success. Cleanup remains pending."
    );
    Ok(())
}
