//! Translated SDL condition wrappers and cooperative recursive-mutex waits.
//! Caller supplies the matching archive; passing these fixtures is not a game boot.
use native_ps5::{
    guest_memory::DataRegion,
    guest_time::{TimeSource, Timeval},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const BASE: u64 = 0x7d0000;
const ARGS: u64 = 0x38000000;
const IMAGE: u64 = 0x100000;
const INIT: u64 = BASE;
const DESTROY: u64 = BASE + 16;
const SIGNAL: u64 = BASE + 32;
const BROADCAST: u64 = BASE + 48;
const WAIT: u64 = BASE + 64;
const TIMED: u64 = BASE + 80;
const ATTRINIT: u64 = BASE + 96;
const ATTRTYPE: u64 = BASE + 112;
const MUTEXINIT: u64 = BASE + 128;
const LOCK: u64 = BASE + 144;
const TRY: u64 = BASE + 160;
const UNLOCK: u64 = BASE + 176;
const MALLOC: u64 = BASE + 192;
const FREE: u64 = BASE + 208;
const REALTIME: u64 = BASE + 224;
fn fixed(c: &mut Vec<u8>, op: u8, v: u64) {
    c.extend([0x48, op]);
    c.extend(v.to_le_bytes());
}
fn call(c: &mut Vec<u8>, v: u64) {
    fixed(c, 0xb8, v);
    c.extend([0xff, 0xd0]);
}
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing condition GOT")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
) -> Result<u64, String> {
    n.set_worker_quantum(None)?;
    let mut data: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut errno = 77u32.to_le_bytes();
    match n
        .run_worker_with_data(
            a,
            IMAGE + 0x100,
            ARGS,
            &mut [r],
            &mut [
                DataRegion {
                    address: ARGS,
                    bytes: &mut data,
                    writable: false,
                },
                DataRegion {
                    address: ARGS + 128,
                    bytes: &mut errno,
                    writable: true,
                },
            ],
        )
        .map_err(|e| {
            format!(
                "Condition fixture target 0x{:x}, args {:x?}: {e}",
                args[3],
                &args[..3]
            )
        })? {
        Execution::WorkerReturned(v) => Ok(v),
        Execution::QuantumYield(_) => Err(format!(
            "Condition adapter target0x{:x} unexpectedly yielded",
            args[3]
        )),
        Execution::MutexWait(_) => Err(format!(
            "Condition adapter target0x{:x} blocked on mutex",
            args[3]
        )),
        Execution::ThreadRequest(_) => Err(format!(
            "Condition adapter target0x{:x} requested thread service",
            args[3]
        )),
        _ => Err(format!(
            "Expected immediate condition fixture return target0x{:x}",
            args[3]
        )),
    }
}
fn status(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    target: u64,
    x: u64,
    y: u64,
    z: u64,
    expected: u64,
) -> Result<(), String> {
    assert_eq!(
        invoke(n, a, r, [x, y, z, target])?,
        expected,
        "condition fixture target {target:x}"
    );
    Ok(())
}
fn worker(timed: bool) -> Vec<u8> {
    let mut c = vec![0x53, 0x55, 0x48, 0x89, 0xfb]; // preserve base and selected result
    for _ in 0..3 {
        c.extend([0x48, 0x8d, 0x7b, 8]);
        call(&mut c, LOCK);
    }
    c.extend([0x48, 0x89, 0xdf, 0x48, 0x8d, 0x73, 8]);
    if timed {
        fixed(&mut c, 0xba, 1);
        call(&mut c, BIAS + 0x1011b0);
    } else {
        call(&mut c, BIAS + 0x1012a0);
    }
    c.extend([0x48, 0x89, 0xc5]);
    for _ in 0..3 {
        c.extend([0x48, 0x8d, 0x7b, 8]);
        call(&mut c, UNLOCK);
        c.extend([0x48, 0xc1, 0xe0, 32, 0x48, 0x09, 0xc5]); // preserve any unlock error in high bits
    }
    c.extend([0x48, 0x89, 0xe8, 0x5d, 0x5b, 0xc3]);
    c
}
fn step(
    s: &mut Scheduler,
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
) -> Result<Step, String> {
    s.step(n, a, &mut [r], &mut [])
}
fn reach_wait(
    s: &mut Scheduler,
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    count: usize,
) -> Result<(), String> {
    for _ in 0..1000 {
        step(s, n, a, r)?;
        if n.conditions.pending_count() == count {
            return Ok(());
        }
    }
    Err("Condition workers did not reach wait".into())
}
fn finish(
    s: &mut Scheduler,
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    ids: &[u64],
    expected: u64,
) -> Result<(), String> {
    for _ in 0..2000 {
        if step(s, n, a, r)? == Step::Finished {
            break;
        }
    }
    for &id in ids {
        assert_eq!(s.result(id), Some(expected));
    }
    assert_eq!(n.conditions.pending_count(), 0);
    Ok(())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f.resize(0x1000, 0);
    f[56..58].copy_from_slice(&1u16.to_le_bytes()); // fixture uses code only, no overlapping data segment
    f[96..104].copy_from_slice(&0x1000u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x1000u64.to_le_bytes());
    let adapter = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + adapter.len()].copy_from_slice(&adapter);
    for (offset, timed) in [(0x200, false), (0x400, true)] {
        let c = worker(timed);
        f[offset..offset + c.len()].copy_from_slice(&c);
    }
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x31, 0xf6];
    call(&mut alias, INIT);
    alias.push(0xc3);
    f[0x600..0x600 + alias.len()].copy_from_slice(&alias);
    let mut n = NativeExecutor::new(0x20000000, 2 * 1024 * 1024, 200000, 200000)?;
    n.kernel = Some(native_ps5::kernel::Kernel::new(4096, ARGS + 128)?);
    n.configure_manual_sleep_clock(100)?;
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 100,
        microseconds: 0,
    }));
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut a = Image::load(&f, IMAGE, 65536)?;
    for (i, s) in [
        Service::CondInit,
        Service::CondDestroy,
        Service::CondSignal,
        Service::CondBroadcast,
        Service::CondWait,
        Service::CondTimedWait,
        Service::MutexAttrInit,
        Service::MutexAttrSetType,
        Service::MutexInit,
        Service::MutexLock,
        Service::MutexTryLock,
        Service::MutexUnlock,
        Service::Malloc,
        Service::Free,
        Service::Gettimeofday,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(BASE + 16 * i as u64, s)?;
    }
    for (got, target) in [
        (0x26fa78, INIT),
        (0x26fa80, DESTROY),
        (0x26fa88, SIGNAL),
        (0x26fa90, BROADCAST),
        (0x26fa98, TIMED),
        (0x26faa0, WAIT),
        (0x531b88, MALLOC),
        (0x531ba0, FREE),
        (0x26f908, REALTIME),
    ] {
        patch(&mut real, got, target)?;
    }
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([
                0x1010a0, 0x101100, 0x101130, 0x101170, 0x1011b0, 0x1012a0, 0x18d00, 0x18de0,
            ]),
            20000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            IMAGE,
            BTreeSet::from([0x100, 0x200, 0x400, 0x600]),
            2000,
            archive,
            &directory.join("workers"),
        )?;
    }
    println!("Condition fixture: SDL lifecycle");
    // Actual SDK-linked SDL allocation, initialization, signalling and destruction.
    let c = invoke(&mut n, &mut a, &mut real, [0, 0, 0, BIAS + 0x1010a0])?;
    assert_ne!(c, 0);
    status(&mut n, &mut a, &mut real, BIAS + 0x101130, c, 0, 0, 0)?;
    status(&mut n, &mut a, &mut real, BIAS + 0x101170, c, 0, 0, 0)?;
    invoke(&mut n, &mut a, &mut real, [c, 0, 0, BIAS + 0x101100])?;
    assert_eq!(n.conditions.object_count(), 0);
    // One owned allocation holds condition, recursive mutex, and attribute slots.
    let shared = invoke(&mut n, &mut a, &mut real, [64, 0, 0, MALLOC])?;
    assert_ne!(shared, 0);
    status(&mut n, &mut a, &mut real, INIT, shared, 0, 0, 0)?;
    status(&mut n, &mut a, &mut real, ATTRINIT, shared + 16, 0, 0, 0)?;
    status(&mut n, &mut a, &mut real, ATTRTYPE, shared + 16, 2, 0, 0)?;
    status(
        &mut n,
        &mut a,
        &mut real,
        MUTEXINIT,
        shared + 8,
        shared + 16,
        0,
        0,
    )?;
    // Signal before wait cannot create a stored permit. Two translated workers
    // suspend on the same recursive mutex; broadcast selects both while thread1
    // still owns the mutex, so neither may return yet.
    status(&mut n, &mut a, &mut real, SIGNAL, shared, 0, 0, 0)?;
    println!("Condition fixture: broadcast and recursive reacquisition");
    let mut s = Scheduler::new(3, 20000, 200000)?;
    s.enqueue(&mut n, 2, IMAGE + 0x200, shared)?;
    reach_wait(&mut s, &mut n, &mut a, &mut real, 1)?;
    assert_eq!(s.result(2), None);
    s.enqueue(&mut n, 3, IMAGE + 0x200, shared)?;
    reach_wait(&mut s, &mut n, &mut a, &mut real, 2)?;
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, LOCK, shared + 8, 0, 0, 0)?;
    status(&mut n, &mut a, &mut real, BIAS + 0x101170, shared, 0, 0, 0)?;
    assert_eq!(n.conditions.pending_count(), 2);
    assert_eq!(s.result(2), None);
    assert_eq!(s.result(3), None);
    // Retained ownership must protect the entire heap allocation from free.
    assert!(invoke(&mut n, &mut a, &mut real, [shared, 0, 0, FREE]).is_err());
    assert_eq!(n.conditions.pending_count(), 2);
    status(&mut n, &mut a, &mut real, UNLOCK, shared + 8, 0, 0, 0)?;
    finish(&mut s, &mut n, &mut a, &mut real, &[2, 3], 0)?;
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, TRY, shared + 8, 0, 0, 0)?;
    status(&mut n, &mut a, &mut real, UNLOCK, shared + 8, 0, 0, 0)?;
    // A single signal selects exactly one of two waiters; the other remains
    // queued until a later broadcast, independent of which worker wins.
    println!("Condition fixture: single signal selects one waiter");
    let mut single = Scheduler::new(3, 20000, 200000)?;
    single.enqueue(&mut n, 4, IMAGE + 0x200, shared)?;
    reach_wait(&mut single, &mut n, &mut a, &mut real, 1)?;
    single.enqueue(&mut n, 5, IMAGE + 0x200, shared)?;
    reach_wait(&mut single, &mut n, &mut a, &mut real, 2)?;
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, BIAS + 0x101130, shared, 0, 0, 0)?;
    for _ in 0..20000 {
        if single.result(4).is_some() || single.result(5).is_some() {
            break;
        }
        step(&mut single, &mut n, &mut a, &mut real)?;
    }
    assert_ne!(single.result(4).is_some(), single.result(5).is_some());
    assert_eq!(n.conditions.pending_count(), 1);
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, BIAS + 0x101170, shared, 0, 0, 0)?;
    finish(&mut single, &mut n, &mut a, &mut real, &[4, 5], 0)?;
    // Actual SDL timeout arithmetic: clock advances while another worker owns the
    // mutex. Timeout is selected, but the call still cannot return before unlock.
    println!("Condition fixture: timeout while mutex held");
    let mut t = Scheduler::new(3, 20000, 200000)?;
    t.enqueue(&mut n, 6, IMAGE + 0x400, shared)?;
    reach_wait(&mut t, &mut n, &mut a, &mut real, 1)?;
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, LOCK, shared + 8, 0, 0, 0)?;
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 101,
        microseconds: 0,
    }));
    // Scheduler should report no runnable timed completion until mutex release.
    let outcome = step(&mut t, &mut n, &mut a, &mut real)?;
    assert!(!matches!(outcome, Step::Completed { .. } | Step::Finished));
    assert_eq!(t.result(6), None);
    assert_eq!(n.conditions.pending_count(), 1);
    n.select_guest_thread(1)?;
    status(&mut n, &mut a, &mut real, UNLOCK, shared + 8, 0, 0, 0)?;
    finish(&mut t, &mut n, &mut a, &mut real, &[6], 1)?;
    // Aliased condition/mutex slots must fail before recursive ownership is
    // detached; all three original unlocks still succeed afterwards.
    n.select_guest_thread(1)?;
    for _ in 0..3 {
        status(&mut n, &mut a, &mut real, LOCK, shared + 8, 0, 0, 0)?;
    }
    assert!(invoke(&mut n, &mut a, &mut real, [shared + 8, shared + 8, 0, WAIT]).is_err());
    assert_eq!(n.conditions.pending_count(), 0);
    for _ in 0..3 {
        status(&mut n, &mut a, &mut real, UNLOCK, shared + 8, 0, 0, 0)?;
    }
    // Invalid timed input precedes both object addresses; ABI returns status, not errno.
    n.set_worker_quantum(None)?;
    let mut invalid = [0u8; 16];
    invalid[8..].copy_from_slice(&1_000_000_000i64.to_le_bytes());
    let mut descriptor: Vec<u8> = [u64::MAX, u64::MAX, ARGS + 64, TIMED]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    assert!(matches!(
        n.run_worker_with_data(
            &mut a,
            IMAGE + 0x100,
            ARGS,
            &mut [&mut real],
            &mut [
                DataRegion {
                    address: ARGS,
                    bytes: &mut descriptor,
                    writable: false
                },
                DataRegion {
                    address: ARGS + 64,
                    bytes: &mut invalid,
                    writable: false
                }
            ]
        )?,
        Execution::WorkerReturned(22)
    ));
    let before = n.conditions.object_count();
    assert!(
        n.run_worker_with_data(&mut a, IMAGE + 0x600, 0, &mut [&mut real], &mut [])
            .is_err()
    );
    assert_eq!(n.conditions.object_count(), before);
    println!(
        "Native SDL condition init/destroy/signal/broadcast/wait/timedwait, two quantum-sliced workers, recursive mutex reacquisition and lifetime guards passed"
    );
    Ok(())
}
