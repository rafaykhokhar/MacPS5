//! Cooperative nanosleep, actual SDL_Delay and module initializer ownership.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::{Arguments, EntryPoints, Modules, StartEvent, lifecycle_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const SLEEP: u64 = 0x790000;
const ERRNO: u64 = DATA + 120;
fn call(c: &mut Vec<u8>, address: u64) {
    c.extend([0x48, 0xb8]);
    c.extend(address.to_le_bytes());
    c.extend([0xff, 0xd0]);
}
fn fixed(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn request(data: &mut [u8], seconds: i64, nanos: i64) {
    data[..8].copy_from_slice(&seconds.to_le_bytes());
    data[8..16].copy_from_slice(&nanos.to_le_bytes());
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    // A seeds nondefault FP control words, sleeps, then snapshots preserved state.
    let mut a = Vec::new();
    fixed(&mut a, 0xbf, DATA + 32);
    a.extend([0xd9, 0x2f]);
    fixed(&mut a, 0xbf, DATA + 36);
    a.extend([0x0f, 0xae, 0x17]);
    fixed(&mut a, 0xbf, DATA);
    fixed(&mut a, 0xbe, u64::MAX);
    call(&mut a, SLEEP);
    fixed(&mut a, 0xbf, DATA + 40);
    a.extend([0xd9, 0x3f]);
    fixed(&mut a, 0xbf, DATA + 44);
    a.extend([0x0f, 0xae, 0x1f]);
    a.extend([0xb8, 7, 0, 0, 0, 0xc3]);
    f[0x100..0x100 + a.len()].copy_from_slice(&a);
    let mut b = Vec::new();
    fixed(&mut b, 0xbf, DATA + 64);
    b.extend([0xc7, 0x07, 1, 0, 0, 0, 0xb8, 9, 0, 0, 0, 0xc3]);
    f[0x200..0x200 + b.len()].copy_from_slice(&b);
    let mut direct = Vec::new();
    fixed(&mut direct, 0xbf, DATA);
    fixed(&mut direct, 0xbe, u64::MAX);
    call(&mut direct, SLEEP);
    direct.push(0xc3);
    f[0x240..0x240 + direct.len()].copy_from_slice(&direct);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.register_service(SLEEP, Service::Nanosleep)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.configure_manual_sleep_clock(100)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200, 0x240]),
            1000,
            archive,
            &directory.join("workers"),
        )?;
    }
    let mut image = Image::load(&f, 0x100000, 65536)?;
    let mut data = [0xa5; 128];
    request(&mut data, 0, 5);
    data[32..34].copy_from_slice(&0x0b7fu16.to_le_bytes());
    data[36..40].copy_from_slice(&0x5fa1u32.to_le_bytes());
    data[64..68].fill(0);
    data[120..124].copy_from_slice(&77u32.to_le_bytes());
    let mut s = Scheduler::new(1000, 100, 10000)?;
    s.enqueue(&mut n, 2, 0x100100, 0)?;
    s.enqueue(&mut n, 3, 0x100200, 0)?;
    for _ in 0..10 {
        let step = s.step(
            &mut n,
            &mut image,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        )?;
        if matches!(step, Step::Sleeping { deadline_ns: 105 }) {
            break;
        }
    }
    assert_eq!(s.result(3), Some(9));
    assert_eq!(s.result(2), None);
    assert_eq!(&data[64..68], &1u32.to_le_bytes());
    let budgets = (s.remaining_dispatches(), s.remaining_instructions());
    let fuel = n.consumed_instruction_fuel();
    request(&mut data, 0, 0); // request was snapshotted, not reread while asleep
    for _ in 0..20 {
        assert_eq!(
            s.step(
                &mut n,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )?,
            Step::Sleeping { deadline_ns: 105 }
        );
    }
    assert_eq!(
        budgets,
        (s.remaining_dispatches(), s.remaining_instructions())
    );
    assert_eq!(fuel, n.consumed_instruction_fuel());
    assert!(n.configure_manual_sleep_clock(0).is_err());
    assert!(n.advance_sleep_clock(99).is_err());
    n.advance_sleep_clock(104)?;
    assert_eq!(
        s.step(
            &mut n,
            &mut image,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true
            }]
        )?,
        Step::Sleeping { deadline_ns: 105 }
    );
    n.advance_sleep_clock(105)?;
    for _ in 0..10 {
        if s.step(
            &mut n,
            &mut image,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        )? == Step::Finished
        {
            break;
        }
    }
    assert_eq!(s.result(2), Some(7));
    assert_eq!(&data[40..42], &0x0b7fu16.to_le_bytes());
    assert_eq!(&data[44..48], &0x5fa1u32.to_le_bytes());
    assert_eq!(&data[120..124], &77u32.to_le_bytes());
    // Immediate and invalid requests have no scheduler or remaining-pointer access.
    n.select_guest_thread(1)?;
    for (sec, ns, expected, errno) in [
        (-1, 999999999, 0, 77u32),
        (0, 0, 0, 77),
        (-1, -1, u32::MAX as u64, 22),
        (0, 1_000_000_000, u32::MAX as u64, 22),
    ] {
        request(&mut data, sec, ns);
        data[120..124].copy_from_slice(&77u32.to_le_bytes());
        assert!(
            matches!(n.run_worker_with_data(&mut image,0x100240,0,&mut[],&mut[DataRegion{address:DATA,bytes:&mut data,writable:true}])?,Execution::WorkerReturned(v)if v==expected)
        );
        assert_eq!(&data[120..124], &errno.to_le_bytes());
    }
    // Real SDL_Delay(1ms) clears errno and passes its stack-local request/remainder.
    let mut real = Image::load(file, 0x10000000, 64 * 1024 * 1024)?;
    n.register_service(SLEEP + 16, Service::Errno)?;
    for (got, target) in [(0x26f910, SLEEP + 16), (0x26f918, SLEEP)] {
        let address = 0x10000000 + got;
        let seg = real
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing SDL delay GOT")?;
        let at = (address - seg.address) as usize;
        seg.bytes[at..at + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        n.compile_image(
            file,
            0x10000000,
            BTreeSet::from([0x20220]),
            1000,
            archive,
            &directory.join("sdl"),
        )?;
    }
    let mut scheduler = Scheduler::new(1000, 100, 10000)?;
    scheduler.enqueue(&mut n, 4, 0x10020220, 1)?;
    assert!(matches!(
        scheduler.step(
            &mut n,
            &mut real,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true
            }]
        )?,
        Step::ServiceBoundary(4)
    ));
    assert_eq!(scheduler.next_sleep_deadline(), Some(1_000_105));
    assert_eq!(&data[120..124], &[0; 4]);
    n.advance_sleep_clock(1_000_105)?;
    for _ in 0..10 {
        if scheduler.step(
            &mut n,
            &mut real,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        )? == Step::Finished
        {
            break;
        }
    }
    assert_eq!(scheduler.result(4), Some(0));
    // Exhaust the dispatch budget exactly at suspension: unready polling still works.
    request(&mut data, 0, 5);
    let mut exhausted = Scheduler::new(1000, 1, 10000)?;
    exhausted.enqueue(&mut n, 5, 0x100240, 0)?;
    let _ = exhausted.step(
        &mut n,
        &mut image,
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut data,
            writable: true,
        }],
    )?;
    assert_eq!(exhausted.remaining_dispatches(), 0);
    assert_eq!(
        exhausted.step(
            &mut n,
            &mut image,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true
            }]
        )?,
        Step::Sleeping {
            deadline_ns: 1_000_110
        }
    );
    n.advance_sleep_clock(1_000_110)?;
    assert!(
        exhausted
            .step(
                &mut n,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )
            .unwrap_err()
            .contains("dispatch budget exhausted")
    );
    module_sleep(archive, &directory.join("module"), false)?;
    module_sleep(archive, &directory.join("parent-join"), true)?;
    println!(
        "PASS: cooperative sleep lets another worker progress, snapshots requests, preservesFP/stack/TLS ownership, costs no fuel while pending, no earlywake, actual SDL_Delay and retained module initializer; signals/EINTR unsupported."
    );
    Ok(())
}
fn module_sleep(archive: &Path, directory: &Path, joined: bool) -> Result<(), String> {
    let mut f = lifecycle_fixture(false);
    let mut c = vec![
        0x48, 0x83, 0xec, 16, 0x48, 0xc7, 0x04, 0x24, 0, 0, 0, 0, 0x48, 0xc7, 0x44, 0x24, 8, 5, 0,
        0, 0, 0x48, 0x89, 0xe7, 0x31, 0xf6,
    ];
    call(&mut c, SLEEP);
    c.extend([0x48, 0x83, 0xc4, 16, 0x31, 0xc0, 0xc3]);
    if joined {
        f[0x370..0x370 + c.len()].copy_from_slice(&c);
        c = vec![0x48, 0x83, 0xec, 16, 0x48, 0x89, 0xe7, 0x31, 0xf6];
        fixed(&mut c, 0xba, 0x100370);
        c.extend([0x31, 0xc9]);
        call(&mut c, SLEEP + 32);
        c.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
        call(&mut c, SLEEP + 48);
        c.extend([0x48, 0x83, 0xc4, 16, 0x31, 0xc0, 0xc3]);
    }
    f[0x300..0x300 + c.len()].copy_from_slice(&c);
    let mut n = NativeExecutor::new(0x20000000, 2 * 1024 * 1024, 10000, 10000)?;
    n.configure_manual_sleep_clock(0)?;
    n.register_service(SLEEP + 32, Service::ThreadCreate)?;
    n.register_service(SLEEP + 48, Service::ThreadJoin)?;
    n.register_service(SLEEP, Service::Nanosleep)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            if joined {
                BTreeSet::from([0x120, 0x130, 0x300, 0x370])
            } else {
                BTreeSet::from([0x120, 0x130, 0x300])
            },
            1000,
            archive,
            directory,
        )?;
    }
    let mut modules = Modules::new(65536);
    let h = modules.load_dynamic(
        "sleep.prx",
        &f,
        0x100000,
        EntryPoints {
            start: Some(0x300),
            stop: None,
        },
    )?;
    assert!(matches!(
        modules.start_native(h, &mut n, Arguments::default())?,
        StartEvent::ThreadRequest { thread: 1 }
    ));
    assert!(matches!(
        modules.resume_native_threads(h, &mut n, (1000, 100, 10000), 100)?,
        StartEvent::ThreadRequest { thread: 1 }
    ));
    assert_eq!(modules.native_sleep_deadline(), Some(5));
    assert_eq!(modules.native_blocked_sleep_deadline(), Some(5));
    assert!(
        modules
            .abandon_native_start(h)
            .unwrap_err()
            .contains("worker pins")
    );
    n.advance_sleep_clock(5)?;
    assert!(matches!(
        modules.resume_native_threads(h, &mut n, (1000, 100, 10000), 100)?,
        StartEvent::Started
    ));
    Ok(())
}
