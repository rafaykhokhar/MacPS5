//! Cooperative worker slicing, not a pthread scheduler.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
const VALUE: u64 = 0xfedcba9876543210;
fn regions(data: &mut [u8; 8]) -> [DataRegion<'_>; 1] {
    [DataRegion {
        address: DATA,
        bytes: data,
        writable: true,
    }]
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let mut code = vec![0x48, 0xb8];
    code.extend(VALUE.to_le_bytes());
    code.extend([
        0xb9, 20, 0, 0, 0, 0x48, 0xff, 0x07, 0xff, 0xc9, 0x75, 0xf9, 0xc3,
    ]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    // A yielding host request exercises capture of slice policy across services.
    let mut service = vec![0x48, 0xb8];
    service.extend(0x780000u64.to_le_bytes());
    service.extend([0xff, 0xd0, 0xc3]);
    file[0x140..0x140 + service.len()].copy_from_slice(&service);
    native.register_service(0x780000, Service::Dlsym)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0u8; 8];
    assert!(native.set_worker_quantum(Some(0)).is_err());
    native.set_worker_quantum(Some(3))?;
    let mut outcome = native.run_worker_with_data(
        &mut image,
        0x100100,
        DATA,
        &mut [],
        &mut regions(&mut data),
    )?;
    let mut slices = 0;
    while let Execution::QuantumYield(mut wait) = outcome {
        assert_eq!(wait.guest_thread_id(), Some(1));
        if slices == 0 {
            let mut stranger = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
            assert!(
                stranger
                    .resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data))
                    .is_err()
            );
            native.tls.register_thread(2)?;
            native.select_guest_thread(2)?;
            assert!(
                native
                    .resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data))
                    .is_err()
            );
            assert!(!wait.is_consumed());
            native.select_guest_thread(1)?;
        }
        outcome = native.resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data))?;
        assert!(wait.is_consumed());
        assert!(
            native
                .resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data))
                .is_err()
        );
        slices += 1;
        assert!(slices < 100);
    }
    assert!(matches!(outcome, Execution::WorkerReturned(VALUE)));
    assert_eq!(u64::from_le_bytes(data), 20);
    assert!(slices > 10);
    data.fill(0);
    assert!(
        matches!(native.run_with_data(&mut image,0x100100,Arguments{size:DATA,pointer:0},&mut [],&mut regions(&mut data))?,Execution::Returned(v) if v==VALUE as u32 as i32)
    );
    assert_eq!(u64::from_le_bytes(data), 20);
    native.fuel = 9;
    data.fill(0);
    let mut outcome = native.run_worker_with_data(
        &mut image,
        0x100100,
        DATA,
        &mut [],
        &mut regions(&mut data),
    )?;
    let mut exhausted = false;
    for _ in 0..10 {
        let Execution::QuantumYield(mut wait) = outcome else {
            return Err("Budget unexpectedly completed".into());
        };
        match native.resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data)) {
            Ok(next) => outcome = next,
            Err(e) => {
                assert!(e.contains("fuel"));
                exhausted = true;
                break;
            }
        }
    }
    assert!(exhausted);
    assert!(u64::from_le_bytes(data) < 20);
    native.fuel = 1000;
    native.set_worker_quantum(Some(1))?;
    let mut service_outcome = native.run_worker(&mut image, 0x100140, 0, &mut [])?;
    for _ in 0..10 {
        match service_outcome {
            Execution::QuantumYield(mut wait) => {
                service_outcome = native.resume_quantum(&mut image, &mut [], &mut wait, &mut [])?;
            }
            Execution::Dlsym { continuation, .. } => {
                // Configuration changes affect new entries, never captured work.
                native.set_worker_quantum(None)?;
                service_outcome = native.resume(&mut image, &mut [], *continuation, VALUE)?;
            }
            Execution::WorkerReturned(value) => {
                assert_eq!(value, VALUE);
                break;
            }
            _ => return Err("Unexpected quantum/service outcome".into()),
        }
    }
    assert!(matches!(service_outcome, Execution::WorkerReturned(VALUE)));
    native.set_worker_quantum(Some(1))?;
    native.transitions = 4;
    let mut transition_outcome = native.run_worker_with_data(
        &mut image,
        0x100100,
        DATA,
        &mut [],
        &mut regions(&mut data),
    )?;
    let mut exhausted = false;
    for _ in 0..10 {
        let Execution::QuantumYield(mut wait) = transition_outcome else {
            return Err("Transition budget unexpectedly completed".into());
        };
        match native.resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data)) {
            Ok(next) => transition_outcome = next,
            Err(e) => {
                assert!(e.contains("transition"));
                exhausted = true;
                break;
            }
        }
    }
    assert!(exhausted);
    native.transitions = 1000;
    native.select_guest_thread(2)?;
    let Execution::QuantumYield(mut wait) = native.run_worker_with_data(
        &mut image,
        0x100100,
        DATA,
        &mut [],
        &mut regions(&mut data),
    )?
    else {
        return Err("No retirement quantum".into());
    };
    native.tls.retire_thread(2)?;
    assert!(
        native
            .resume_quantum(&mut image, &mut [], &mut wait, &mut regions(&mut data))
            .is_err()
    );
    assert!(!wait.is_consumed());
    println!(
        "Worker quantum native verified: repeated slices, exactly-once loop writes, full-width return, default module behavior, one-shot executor/thread ownership, cumulative fuel and retired-context guard"
    );
    Ok(())
}
