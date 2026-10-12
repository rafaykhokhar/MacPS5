//! Worker invocation ABI only; this verifier does not claim pthread scheduling.
use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const SERVICE: u64 = 0x780000;
fn call(code: &mut Vec<u8>, target: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(target.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn worker(outcome: Execution, expected: u64) {
    match outcome {
        Execution::WorkerReturned(value) => assert_eq!(value, expected),
        _ => panic!("Expected full-width worker return"),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    // mov rax,rdi; ret: opaque argument survives with every high bit intact.
    file[0x100..0x104].copy_from_slice(&[0x48, 0x89, 0xf8, 0xc3]);
    let mut yielding = Vec::new();
    call(&mut yielding, SERVICE);
    yielding.push(0xc3);
    file[0x120..0x120 + yielding.len()].copy_from_slice(&yielding);
    // Tail-called host service returns directly through the terminal sentinel.
    let mut tail = vec![0x48, 0xb8];
    tail.extend(SERVICE.to_le_bytes());
    tail.extend([0xff, 0xe0]);
    file[0x140..0x140 + tail.len()].copy_from_slice(&tail);
    // A second image returns the same argument, across a translated call.
    let mut cross = Vec::new();
    call(&mut cross, 0x200100);
    cross.push(0xc3);
    file[0x160..0x160 + cross.len()].copy_from_slice(&cross);
    // Indirect target is discovered only while running, testing convention capture.
    let mut lazy = vec![0x48, 0xb8];
    lazy.extend(0x1001c0u64.to_le_bytes());
    lazy.extend([0xff, 0xe0]);
    file[0x180..0x180 + lazy.len()].copy_from_slice(&lazy);
    file[0x1c0..0x1c4].copy_from_slice(&[0x48, 0x89, 0xf8, 0xc3]);
    let dependency_file = file.clone();
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    native.register_service(SERVICE, Service::Dlsym)?;
    // SAFETY: caller supplies the matching locally built runtime archive.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x120, 0x140, 0x160, 0x180]),
            1000,
            archive,
            directory,
        )?;
        native.compile_image(
            &dependency_file,
            0x200000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut dependency = Image::load(&dependency_file, 0x200000, 65536)?;
    let value = 0xfedc_ba98_8765_4321;
    for argument in [0, value, u64::MAX] {
        worker(
            native.run_worker(&mut image, 0x100100, argument, &mut [])?,
            argument,
        );
        worker(
            native.run_worker(&mut image, 0x100160, argument, &mut [&mut dependency])?,
            argument,
        );
    }
    worker(
        native.run_worker(&mut image, 0x100180, value, &mut [])?,
        value,
    );
    assert!(
        matches!(native.run(&mut image, 0x100100, Arguments { size: value, pointer: 0 }, &mut [])?, Execution::Returned(v) if v == value as u32 as i32)
    );
    native.tls.register_thread(2)?;
    native.select_guest_thread(2)?;
    for target in [0x100120, 0x100140] {
        let Execution::Dlsym { continuation, .. } =
            native.run(&mut image, target, Arguments::default(), &mut [])?
        else {
            return Err("Module status fixture did not suspend".into());
        };
        assert!(matches!(
            native.resume(&mut image, &mut [], *continuation, value)?,
            Execution::Returned(v) if v == value as u32 as i32
        ));
        let Execution::Dlsym { continuation, .. } =
            native.run_worker(&mut image, target, 0, &mut [])?
        else {
            return Err("Worker did not suspend".into());
        };
        assert_eq!(continuation.guest_thread_id(), 2);
        native.select_guest_thread(1)?;
        worker(
            native.resume(&mut image, &mut [], *continuation, value)?,
            value,
        );
        assert_eq!(native.guest_thread_id(), 1);
        native.select_guest_thread(2)?;
    }
    let Execution::Dlsym { continuation, .. } =
        native.run_worker(&mut image, 0x100120, 0, &mut [])?
    else {
        return Err("Retirement fixture did not suspend".into());
    };
    // Deliberately violate retirement's caller precondition to check the executor
    // fails closed instead of reanimating a retired context.
    native.tls.retire_thread(2)?;
    assert!(
        native
            .run_worker(&mut image, 0x100100, value, &mut [])
            .is_err()
    );
    assert!(
        native
            .resume(&mut image, &mut [], *continuation, value)
            .is_err()
    );
    native.select_guest_thread(1)?;
    worker(
        native.run_worker(&mut image, 0x100100, value, &mut [])?,
        value,
    );
    println!(
        "Worker ABI verified: full-width RDI/RAX, module i32 preservation, cross-image/lazy translation, suspended call/tail returns, captured identity and retired-context rejection; no scheduler claimed"
    );
    Ok(())
}
