//! Actual pinned SDL semaphore wrappers over cooperative guest semaphore services.
use native_ps5::{
    guest_memory::DataRegion,
    guest_time::{TimeSource, Timeval},
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_34000000;
const ARGS: u64 = 0x35000000;
const ERRNO: u64 = 0x36000000;
const SERVICE: u64 = 0x7b0000;
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing semaphore fixture pointer")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        0x100100,
        ARGS,
        &mut [r],
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
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Semaphore call did not return".into()),
    }
}
fn fixed(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn call(c: &mut Vec<u8>, target: u64) {
    fixed(c, 0xb8, target);
    c.extend([0xff, 0xd0]);
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    let adapter = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + adapter.len()].copy_from_slice(&adapter);
    // A one-millisecond timeout through the actual SDL wall-clock calculation.
    let mut timed = Vec::new();
    fixed(&mut timed, 0xbe, 1);
    call(&mut timed, BIAS + 0x329d0);
    timed.push(0xc3);
    f[0x200..0x200 + timed.len()].copy_from_slice(&timed);
    // A live return-address overlap must be rejected before sem_init mutates it.
    let mut frame = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x31, 0xf6, 0x31, 0xd2];
    call(&mut frame, SERVICE);
    frame.push(0xc3);
    f[0x280..0x280 + frame.len()].copy_from_slice(&frame);
    // Parent creates a real SDL waiter, posts, then joins it. Child entry is real SDL_SemWait.
    let mut parent = vec![
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 16, 0x48, 0x89, 0xe7, 0x31, 0xf6,
    ];
    fixed(&mut parent, 0xba, BIAS + 0x32950);
    parent.extend([0x48, 0x89, 0xd9]);
    call(&mut parent, SERVICE + 176);
    parent.extend([0x48, 0x89, 0xdf]);
    call(&mut parent, BIAS + 0x32ba0);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
    call(&mut parent, SERVICE + 192);
    parent.extend([0x48, 0x83, 0xc4, 16, 0x5b, 0xc3]);
    f[0x300..0x300 + parent.len()].copy_from_slice(&parent);
    let mut n = NativeExecutor::new(0x20000000, 2 * 1024 * 1024, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.configure_manual_sleep_clock(100)?;
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 100,
        microseconds: 999500,
    }));
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut a = Image::load(&f, 0x100000, 65536)?;
    for (index, (service, got)) in [
        (Service::SemInit, 0x26f980),
        (Service::SemDestroy, 0x26f988),
        (Service::SemTryWait, 0x26f990),
        (Service::SemWait, 0x26f998),
        (Service::SemTimedWait, 0x26f9a0),
        (Service::SemGetValue, 0x26f9a8),
        (Service::SemPost, 0x26f9b0),
        (Service::Errno, 0x26f910),
        (Service::Gettimeofday, 0x26f908),
        (Service::Malloc, 0x531b88),
        (Service::Free, 0x531ba0),
    ]
    .into_iter()
    .enumerate()
    {
        let target = SERVICE + 16 * index as u64;
        n.register_service(target, service)?;
        patch(&mut real, got, target)?;
    }
    n.register_service(SERVICE + 176, Service::ThreadCreate)?;
    n.register_service(SERVICE + 192, Service::ThreadJoin)?;
    n.register_service(SERVICE + 208, Service::Realloc)?;
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([
                0x32890, 0x328f0, 0x32920, 0x32950, 0x329d0, 0x32b50, 0x32ba0, 0x18d00, 0x18de0,
                0x18b50,
            ]),
            20000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200, 0x280, 0x300]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut data = [0xa5; 128];
    let mut errno = 77u32.to_le_bytes();
    let sem = invoke(
        &mut n,
        &mut a,
        &mut real,
        [1, 0, 0, BIAS + 0x32890],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_ne!(sem, 0);
    for (entry, arg, result) in [
        (0x32b50, 0, 1),
        (0x32920, 0, 0),
        (0x32920, 0, 1),
        (0x329d0, 0, 1),
        (0x32ba0, 0, 0),
        (0x32b50, 0, 1),
        (0x32950, 0, 0),
        (0x32ba0, 0, 0),
        (0x329d0, u32::MAX as u64, 0),
    ] {
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [sem, arg, 0, BIAS + entry],
                &mut data,
                true,
                &mut errno
            )?,
            result
        );
    }
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [sem, 0, 0, BIAS + 0x328f0],
        &mut data,
        true,
        &mut errno,
    )?;
    // Direct service guards use separately registered inline sem_t storage.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    let before = data;
    errno = 77u32.to_le_bytes();
    for (output, writable) in [(DATA + 126, true), (DATA + 64, false), (u64::MAX, true)] {
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, output, 0, SERVICE + 80],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    assert!(
        n.run_worker_with_data(
            &mut a,
            0x100280,
            0,
            &mut [&mut real],
            &mut [DataRegion {
                address: ERRNO,
                bytes: &mut errno,
                writable: true
            }]
        )
        .is_err()
    );
    // Copied valid magic is not a registered object; reject it rather than adopt.
    data[32..48].copy_from_slice(&before[..16]);
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 32, 0, 0, SERVICE + 96],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    // Immediate timed acquisition must precede timeout-pointer inspection.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE + 96],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, u64::MAX, 0, SERVICE + 64],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, u64::MAX, 0, SERVICE + 64],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    // Destroy invalidates the object; a stale address returns EINVAL, not success.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE + 96],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 22u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    let snapshot = data;
    let pending = n.semaphores.pending_count();
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, SERVICE + 48],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(data, snapshot);
    assert_eq!(n.semaphores.pending_count(), pending);
    let owned = invoke(
        &mut n,
        &mut a,
        &mut real,
        [0, 0, 0, BIAS + 0x32890],
        &mut data,
        true,
        &mut errno,
    )?;
    cooperative(&mut n, &mut a, &mut real, &mut data, &mut errno, owned)?;
    replay_lifetime(&mut n, &mut a, &mut real, &mut data, &mut errno, owned)?;
    let mut parent_scheduler = Scheduler::new(1000, 100, 10000)?;
    parent_scheduler.enqueue(&mut n, 10, 0x100300, owned)?;
    for _ in 0..50 {
        if parent_scheduler.step(
            &mut n,
            &mut a,
            &mut [&mut real],
            &mut [
                DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true,
                },
                DataRegion {
                    address: ERRNO,
                    bytes: &mut errno,
                    writable: true,
                },
            ],
        )? == Step::Finished
        {
            break;
        }
    }
    assert_eq!(parent_scheduler.result(10), Some(0));
    println!(
        "Actual SDL semaphore create/destroy/post/value/try/wait/timed paths and guarded cooperative waits passed"
    );
    Ok(())
}
fn cooperative(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    errno: &mut [u8],
    sem: u64,
) -> Result<(), String> {
    let mut s = Scheduler::new(1000, 100, 10000)?;
    s.enqueue(n, 2, 0x100200, sem)?;
    let mut sleeping = false;
    for _ in 0..20 {
        let step = s.step(
            n,
            a,
            &mut [real],
            &mut [
                DataRegion {
                    address: DATA,
                    bytes: data,
                    writable: true,
                },
                DataRegion {
                    address: ERRNO,
                    bytes: errno,
                    writable: true,
                },
            ],
        )?;
        if matches!(step, Step::Sleeping { .. }) {
            sleeping = true;
            break;
        }
    }
    assert!(sleeping);
    let budgets = (
        s.remaining_dispatches(),
        s.remaining_instructions(),
        n.consumed_instruction_fuel(),
    );
    for _ in 0..20 {
        assert!(matches!(
            s.step(
                n,
                a,
                &mut [real],
                &mut [
                    DataRegion {
                        address: DATA,
                        bytes: data,
                        writable: true
                    },
                    DataRegion {
                        address: ERRNO,
                        bytes: errno,
                        writable: true
                    }
                ]
            )?,
            Step::Sleeping { .. }
        ));
    }
    assert_eq!(
        budgets,
        (
            s.remaining_dispatches(),
            s.remaining_instructions(),
            n.consumed_instruction_fuel()
        )
    );
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 101,
        microseconds: 500,
    }));
    // Wall-clock deadline must wake without advancing the monotonic hint clock.
    assert_eq!(n.sleep_clock_now(), 100);
    for _ in 0..20 {
        if s.step(
            n,
            a,
            &mut [real],
            &mut [
                DataRegion {
                    address: DATA,
                    bytes: data,
                    writable: true,
                },
                DataRegion {
                    address: ERRNO,
                    bytes: errno,
                    writable: true,
                },
            ],
        )? == Step::Finished
        {
            break;
        }
    }
    assert_eq!(s.result(2), Some(1));
    assert_eq!(errno, 60u32.to_le_bytes());
    Ok(())
}

fn replay_lifetime(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    errno: &mut [u8],
    sem: u64,
) -> Result<(), String> {
    n.select_guest_thread(1)?;
    let Execution::ThreadRequest(mut request) = n.run_worker_with_data(
        real,
        BIAS + 0x32950,
        sem,
        &mut [],
        &mut [DataRegion {
            address: ERRNO,
            bytes: errno,
            writable: true,
        }],
    )?
    else {
        return Err("Expected retained semaphore request".into());
    };
    let pending = n.semaphores.pending_count();
    assert_eq!(pending, 1);
    for target in [SERVICE + 160, SERVICE + 208] {
        assert!(invoke(n, a, real, [sem, 32, 0, target], data, true, errno).is_err());
        assert_eq!(n.semaphores.pending_count(), pending);
    }
    let mut foreign = NativeExecutor::new(0x40000000, 65536, 10000, 10000)?;
    assert!(
        foreign
            .resume_semaphore_request(&mut request, real, &mut [], &mut [])
            .is_err()
    );
    assert_eq!(
        n.resume_semaphore_request(
            &mut request,
            real,
            &mut [],
            &mut [DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true
            }]
        )?,
        None
    );
    assert_eq!(
        invoke(n, a, real, [sem, 0, 0, BIAS + 0x32ba0], data, true, errno)?,
        0
    );
    // Real SDL continuation starts at 0x32976; changed code must not consume a permit.
    let address = BIAS + 0x32976;
    let segment = real
        .segments
        .iter_mut()
        .find(|v| address >= v.address && address < v.address + v.bytes.len() as u64)
        .ok_or("Missing SDL return code")?;
    let offset = (address - segment.address) as usize;
    let original = segment.bytes[offset];
    segment.bytes[offset] ^= 1;
    assert!(
        n.resume_semaphore_request(
            &mut request,
            real,
            &mut [],
            &mut [DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true
            }]
        )
        .is_err()
    );
    assert_eq!(n.semaphores.pending_count(), 1);
    let segment = real
        .segments
        .iter_mut()
        .find(|v| address >= v.address && address < v.address + v.bytes.len() as u64)
        .unwrap();
    segment.bytes[(address - segment.address) as usize] = original;
    assert_eq!(
        n.resume_semaphore_request(
            &mut request,
            real,
            &mut [],
            &mut [DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true
            }]
        )?,
        Some(0)
    );
    assert_eq!(n.semaphores.pending_count(), 0);
    assert!(
        n.resume_semaphore_request(
            &mut request,
            real,
            &mut [],
            &mut [DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true
            }]
        )
        .is_err()
    );
    Ok(())
}
