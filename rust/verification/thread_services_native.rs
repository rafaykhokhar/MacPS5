//! Real translated pthread create/join with a child using its parent's stack.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const VALUE: u64 = 0xfedcba9876543210;
const DATA: u64 = 0x30000000;
fn call(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let mut parent = vec![0x48, 0x83, 0xec, 48, 0x48, 0xb8];
    parent.extend(VALUE.to_le_bytes());
    parent.extend([
        0x48, 0x89, 0x44, 0x24, 16, 0xc7, 0x44, 0x24, 24, 0x80, 0x3f, 0, 0, 0x0f, 0xae, 0x54, 0x24,
        24,
    ]);
    parent.extend([0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x1001c0u64.to_le_bytes());
    parent.extend([0x48, 0x8d, 0x4c, 0x24, 16]);
    call(&mut parent, 0x780000);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x48, 0x8d, 0x74, 0x24, 8]);
    call(&mut parent, 0x780010);
    parent.extend([0x48, 0xb9]);
    parent.extend(DATA.to_le_bytes());
    parent.extend([
        0x48, 0x89, 0x01, 0x48, 0x8b, 0x44, 0x24, 24, 0x48, 0x89, 0x41, 8, 0x48, 0x8b, 0x44, 0x24,
        32, 0x48, 0x89, 0x41, 16, 0x48, 0x8b, 0x44, 0x24, 8, 0x48, 0x83, 0xc4, 48, 0xc3,
    ]);
    assert!(parent.len() <= 0xc0);
    file[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    let mut child = vec![
        0xb9, 20, 0, 0, 0, 0xff, 0xc9, 0x75, 0xfc, 0x0f, 0xae, 0x5f, 8,
    ];
    call(&mut child, 0x780020);
    child.extend([0x48, 0x89, 0x47, 16, 0x48, 0x8b, 0x07, 0xc3]);
    file[0x1c0..0x1c0 + child.len()].copy_from_slice(&child);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
    for (address, service) in [
        (0x780000, Service::ThreadCreate),
        (0x780010, Service::ThreadJoin),
        (0x780020, Service::ThreadSelf),
        (0x780030, Service::ThreadDetach),
    ] {
        native.register_service(address, service)?;
    }
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut scheduler = Scheduler::new(2, 1000, 10000)?;
    scheduler.enqueue(&mut native, 2, 0x100100, 0)?;
    let mut data = [0u8; 24];
    let mut joins = 0;
    for _ in 0..1000 {
        match scheduler.step(
            &mut native,
            &mut image,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        )? {
            Step::Finished => break,
            Step::JoinPending(_) => joins += 1,
            _ => {}
        }
    }
    assert_eq!(scheduler.result(2), Some(VALUE));
    assert_eq!(scheduler.result(3), None);
    assert_eq!(u64::from_le_bytes(data[..8].try_into().unwrap()), 0);
    assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 0x3f80);
    assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 3);
    assert_eq!(native.tls.live_threads(), 1);
    assert!(joins > 0);
    for case in 0..4 {
        let mut file = memory_fixture();
        let mut code = vec![0x48, 0xbf];
        code.extend(DATA.to_le_bytes());
        code.extend([0x31, 0xf6, 0x48, 0xba]);
        code.extend(if case == 1 { 0u64 } else { 0x1001c0u64 }.to_le_bytes());
        code.extend([0x48, 0xb9]);
        code.extend(VALUE.to_le_bytes());
        call(&mut code, 0x780000);
        if case == 3 {
            code.extend([0x48, 0xbf]);
            code.extend(DATA.to_le_bytes());
            code.extend([0x48, 0x8b, 0x3f]);
            call(&mut code, 0x780030);
        }
        code.push(0xc3);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        file[0x1c0..0x1c4].copy_from_slice(&[0x48, 0x89, 0xf8, 0xc3]);
        let mut native = NativeExecutor::new(
            0x10000000,
            if case == 2 { 4096 } else { 2 * 1024 * 1024 },
            10000,
            10000,
        )?;
        native.register_service(0x780000, Service::ThreadCreate)?;
        native.register_service(0x780030, Service::ThreadDetach)?;
        let case_directory = directory.join(format!("case-{case}"));
        std::fs::create_dir_all(&case_directory).map_err(|e| e.to_string())?;
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                1000,
                archive,
                &case_directory,
            )?;
            native.set_automatic_translation(true);
        }
        let mut image = Image::load(&file, 0x100000, 65536)?;
        let mut scheduler = Scheduler::new(2, 1000, 10000)?;
        scheduler.enqueue(&mut native, 2, 0x100100, 0)?;
        let mut slot = 0xaaaaaaaaaaaaaaaau64.to_le_bytes();
        let mut error = None;
        for _ in 0..1000 {
            match scheduler.step(
                &mut native,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut slot,
                    writable: case != 0,
                }],
            ) {
                Ok(Step::Finished) => break,
                Ok(_) => {}
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        if case < 3 {
            assert!(
                error.is_some(),
                "Creation failure case {case} unexpectedly succeeded"
            );
            assert_eq!(slot, 0xaaaaaaaaaaaaaaaau64.to_le_bytes());
            assert_eq!(native.tls.live_threads(), 2);
            assert!(!native.tls.contains_thread(3));
            assert_eq!(scheduler.result(2), None);
            assert_eq!(scheduler.result(3), None);
        } else {
            assert!(error.is_none(), "Detached child failed: {error:?}");
            assert_eq!(scheduler.result(2), Some(0));
            assert_eq!(scheduler.result(3), None);
            assert_eq!(native.tls.live_threads(), 1);
        }
    }
    println!(
        "Guest create/join native verified: prepared real child, parent-stack argument, child pthread_self identity, inherited MXCSR, full64 join return and retired TLS; observed {joins} join waits"
    );
    Ok(())
}
