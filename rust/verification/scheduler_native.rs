//! Real round-robin translated workers sharing a retained parent's stack.
use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let worker = [
        0x48, 0x83, 0xec, 8, 0x48, 0x89, 0x3c, 0x24, 0xb9, 20, 0, 0, 0, 0x48, 0x8b, 0x3c, 0x24,
        0x48, 0xff, 0x07, 0xff, 0xc9, 0x75, 0xf5, 0x48, 0x8b, 0x04, 0x24, 0x48, 0x83, 0xc4, 8,
        0xc3,
    ];
    file[0x100..0x100 + worker.len()].copy_from_slice(&worker);
    let mut parent = vec![
        0x48, 0x83, 0xec, 32, 0x48, 0xc7, 0x04, 0x24, 0, 0, 0, 0, 0x48, 0xc7, 0x44, 0x24, 8, 0, 0,
        0, 0, 0x48, 0x89, 0xe7, 0x48, 0xb8,
    ];
    parent.extend(0x780000u64.to_le_bytes());
    parent.extend([
        0xff, 0xd0, 0x48, 0x8b, 0x04, 0x24, 0x48, 0x83, 0xc4, 32, 0xc3,
    ]);
    file[0x140..0x140 + parent.len()].copy_from_slice(&parent);
    file[0x1c0..0x1c4].copy_from_slice(&[0x48, 0x89, 0xf8, 0xc3]);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    native.register_service(0x780000, Service::Dlsym)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x1c0]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let Execution::Dlsym {
        arguments,
        mut continuation,
    } = native.run(&mut image, 0x100140, Arguments::default(), &mut [])?
    else {
        return Err("Parent did not retain stack".into());
    };
    let pointer = arguments[0];
    let mut scheduler = Scheduler::new(2, 1000, 10000)?;
    scheduler.enqueue(&mut native, 2, 0x100100, pointer)?;
    scheduler.enqueue(&mut native, 3, 0x100100, pointer + 8)?;
    let mut foreign = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    foreign.tls.register_thread(2)?;
    foreign.tls.register_thread(3)?;
    let budget = scheduler.remaining_dispatches();
    assert!(
        scheduler
            .step(
                &mut foreign,
                &mut image,
                &mut [],
                &mut [continuation.stack_region()]
            )
            .is_err()
    );
    assert_eq!(scheduler.remaining_dispatches(), budget);
    let mut switches = Vec::new();
    for _ in 0..1000 {
        let step = scheduler.step(
            &mut native,
            &mut image,
            &mut [],
            &mut [continuation.stack_region()],
        )?;
        match step {
            Step::Yielded(id) => switches.push(id),
            Step::Completed { .. } => {}
            Step::Finished => break,
            other => return Err(format!("Unexpected scheduler event {other:?}")),
        }
    }
    assert_eq!(scheduler.result(2), Some(pointer));
    assert_eq!(scheduler.result(3), Some(pointer + 8));
    assert!(switches.len() > 20);
    assert_eq!(&switches[..6], &[2, 3, 2, 3, 2, 3]);
    let region = continuation.stack_region();
    let offset = (pointer - region.address) as usize;
    assert_eq!(
        u64::from_le_bytes(region.bytes[offset..offset + 8].try_into().unwrap()),
        20
    );
    assert_eq!(
        u64::from_le_bytes(region.bytes[offset + 8..offset + 16].try_into().unwrap()),
        20
    );
    assert!(!native.tls.contains_thread(2));
    assert!(!native.tls.contains_thread(3));
    native.select_guest_thread(1)?;
    assert!(matches!(
        native.resume(&mut image, &mut [], *continuation, 0)?,
        Execution::Returned(20)
    ));
    let value = 0xfedcba9876543210;
    for id in 4..74 {
        scheduler.enqueue(&mut native, id, 0x1001c0, value)?;
        for _ in 0..10 {
            if matches!(scheduler.step(&mut native,&mut image,&mut [],&mut [])?,Step::Completed{thread,..} if thread==id)
            {
                break;
            }
        }
        assert_eq!(scheduler.result(id), Some(value));
        assert!(!native.tls.contains_thread(id));
    }
    assert_eq!(native.tls.live_threads(), 1);
    native.select_guest_thread(1)?;
    let Execution::Dlsym {
        arguments,
        mut continuation,
    } = native.run(&mut image, 0x100140, Arguments::default(), &mut [])?
    else {
        return Err("Budget parent did not suspend".into());
    };
    let mut bounded = Scheduler::new(2, 1000, 9)?;
    bounded.enqueue(&mut native, 74, 0x100100, arguments[0])?;
    bounded.enqueue(&mut native, 75, 0x100100, arguments[0] + 8)?;
    let before = native.consumed_instruction_fuel();
    let mut exhausted = false;
    for _ in 0..20 {
        if let Err(error) = bounded.step(
            &mut native,
            &mut image,
            &mut [],
            &mut [continuation.stack_region()],
        ) {
            assert!(error.contains("fuel") || error.contains("instruction"));
            exhausted = true;
            break;
        }
    }
    assert!(exhausted);
    assert_eq!(bounded.remaining_instructions(), 0);
    assert_eq!(native.consumed_instruction_fuel() - before, 9);
    assert!(
        bounded
            .step(
                &mut native,
                &mut image,
                &mut [],
                &mut [continuation.stack_region()]
            )
            .is_err()
    );
    assert_eq!(native.consumed_instruction_fuel() - before, 9);
    println!(
        "Scheduler native verified: two round-robin workers, distinct retained stacks, shared parent-stack arguments, exactly-once counts, 70 additional retired identities, full-width results, actual shared instruction ledger"
    );
    Ok(())
}
