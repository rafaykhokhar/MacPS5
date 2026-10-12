//! Native translated FreeBSD scheduling metadata subset, no realtime scheduling.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<i32, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match native.run_with_data(
        image,
        0x100100,
        Arguments {
            size: ARGS,
            pointer: 0,
        },
        &mut [],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut descriptor,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable,
            },
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true,
            },
        ],
    )? {
        Execution::Returned(value) => Ok(value),
        _ => Err("Scheduling call did not return".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (address, service) in [
        (0x780000, Service::ThreadGetschedparam),
        (0x780010, Service::ThreadSetschedparam),
        (0x780020, Service::SchedPriorityMin),
        (0x780030, Service::SchedPriorityMax),
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
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0xa5; 16];
    let mut errno = 99u32.to_le_bytes();
    for (policy, max) in [(1, 31), (2, 103), (3, 31)] {
        for (target, expected) in [(0x780020, 0), (0x780030, max)] {
            assert_eq!(
                invoke(
                    &mut native,
                    &mut image,
                    [policy, 0, 0, target],
                    &mut data,
                    true,
                    &mut errno
                )?,
                expected
            );
        }
    }
    assert_eq!(errno, 99u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [u64::MAX, 0, 0, 0x780030],
            &mut data,
            true,
            &mut errno
        )?,
        -1
    );
    assert_eq!(errno, 22u32.to_le_bytes());
    errno = 99u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [1, DATA, DATA + 4, 0x780000],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..8], &[2, 0, 0, 0, 0, 0, 0, 0]);
    for priority in [i32::MIN, -1, 0, 51, 76, 103, i32::MAX] {
        data[..4].copy_from_slice(&priority.to_le_bytes());
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                [1, 2, DATA, 0x780010],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        // Exact output alias: last write is priority, not policy.
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                [1, DATA + 4, DATA + 4, 0x780000],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        assert_eq!(&data[4..8], &priority.to_le_bytes());
    }
    assert_eq!(errno, 99u32.to_le_bytes());
    for (target, policy, param, expected) in [
        (99, 0, u64::MAX, 22),
        (0, u64::MAX, u64::MAX, 22),
        (99, u64::MAX, u64::MAX, 3),
    ] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                [target, policy, param, 0x780000],
                &mut data,
                true,
                &mut errno
            )?,
            expected
        );
    }
    let before = data;
    for (len, writable) in [(7, true), (16, false)] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                [1, DATA, DATA + 4, 0x780000],
                &mut data[..len],
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    for policy in [1, 3, 0] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                [1, policy, DATA, 0x780010],
                &mut data,
                true,
                &mut errno
            )
            .unwrap_err()
            .contains("realtime scheduler")
        );
    }
    native.tls.register_thread_inheriting(2, 1)?;
    native.select_guest_thread(2)?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [2, DATA, DATA + 4, 0x780000],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[4..8], &i32::MAX.to_le_bytes());
    data[..4].copy_from_slice(&51i32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [2, 2, DATA, 0x780010],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [1, DATA, DATA + 4, 0x780000],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[4..8], &i32::MAX.to_le_bytes());
    println!(
        "PASS: translated scheduling bounds, owned OTHER cache with signed priorities, alias/error/errno ordering, output atomicity, explicit realtime rejection and inherited context isolation; no realtime execution claim."
    );
    verify_creation(archive, &directory.join("creation"))
}

fn call(code: &mut Vec<u8>, target: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(target.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn scheduling_call(code: &mut Vec<u8>, target: u64, second: u64, third: u64) {
    call(code, 0x780030); // pthread_self, not an invented child identity
    code.extend([0x48, 0x89, 0xc7, 0x48, 0xbe]);
    code.extend(second.to_le_bytes());
    code.extend([0x48, 0xba]);
    code.extend(third.to_le_bytes());
    call(code, target);
}
fn verify_creation(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    file[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    let mut parent = vec![0x48, 0x83, 0xec, 16];
    scheduling_call(&mut parent, 0x780010, 2, DATA); // cache priority76
    parent.extend([0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x100280u64.to_le_bytes());
    parent.extend([0x31, 0xc9]);
    call(&mut parent, 0x780040);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
    call(&mut parent, 0x780050);
    scheduling_call(&mut parent, 0x780000, DATA + 24, DATA + 28);
    parent.extend([0x48, 0x83, 0xc4, 16, 0xc3]);
    assert!(parent.len() <= 0x180);
    file[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    let mut child = Vec::new();
    scheduling_call(&mut child, 0x780000, DATA + 8, DATA + 12);
    scheduling_call(&mut child, 0x780010, 2, DATA + 4); // childcache51
    scheduling_call(&mut child, 0x780000, DATA + 16, DATA + 20);
    child.push(0xc3);
    assert!(child.len() <= 0x180);
    file[0x280..0x280 + child.len()].copy_from_slice(&child);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
    for (address, service) in [
        (0x780000, Service::ThreadGetschedparam),
        (0x780010, Service::ThreadSetschedparam),
        (0x780030, Service::ThreadSelf),
        (0x780040, Service::ThreadCreate),
        (0x780050, Service::ThreadJoin),
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
    let mut data = [0u8; 32];
    data[..4].copy_from_slice(&76i32.to_le_bytes());
    data[4..8].copy_from_slice(&51i32.to_le_bytes());
    let mut finished = false;
    for _ in 0..1000 {
        if matches!(
            scheduler.step(
                &mut native,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )?,
            Step::Finished
        ) {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert_eq!(scheduler.result(2), Some(0));
    for (offset, priority) in [(8, 76i32), (16, 51), (24, 76)] {
        assert_eq!(&data[offset..offset + 4], &2i32.to_le_bytes());
        assert_eq!(&data[offset + 4..offset + 8], &priority.to_le_bytes());
    }
    assert_eq!(native.tls.live_threads(), 1);
    println!(
        "PASS: actual pthread_create inherits OTHER priority76; translated child changes its cache to51 without altering parent76, then joins and retires."
    );
    Ok(())
}
