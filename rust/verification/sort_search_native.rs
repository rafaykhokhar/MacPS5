//! Guest comparator execution through actual SDL qsort/bsearch wrappers.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x100500;
const ARGS: u64 = 0x42000000;
const ERRNO: u64 = 0x43000000;
const SERVICE: u64 = 0x800000;
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing sort fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn fixed(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn call(c: &mut Vec<u8>, target: u64) {
    fixed(c, 0xb8, target);
    c.extend([0xff, 0xd0]);
}
fn regions<'a>(args: &'a mut [u8], errno: &'a mut [u8]) -> Vec<DataRegion<'a>> {
    vec![
        DataRegion {
            address: ARGS,
            bytes: args,
            writable: false,
        },
        DataRegion {
            address: ERRNO,
            bytes: errno,
            writable: true,
        },
    ]
}
fn stage(a: &mut Image, data: &[u8], writable: bool) {
    let segment = a.segments.iter_mut().find(|s| s.address == DATA).unwrap();
    segment.bytes[..data.len()].copy_from_slice(data);
    segment.flags = if writable { 6 } else { 4 };
}
fn snapshot(a: &Image, data: &mut [u8]) {
    let segment = a.segments.iter().find(|s| s.address == DATA).unwrap();
    data.copy_from_slice(&segment.bytes[..data.len()]);
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 6],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<(u64, usize), String> {
    stage(a, data, writable);
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let run = (|| {
        let mut result = n.run_worker_with_data(
            a,
            0x100100,
            ARGS,
            &mut [&mut *real],
            &mut regions(&mut descriptor, errno),
        )?;
        let mut yields = 0;
        loop {
            match result {
                Execution::WorkerReturned(v) => return Ok((v, yields)),
                Execution::QuantumYield(mut wait) => {
                    yields += 1;
                    if yields > 100000 {
                        return Err("Comparator native fixture quantum bound".into());
                    }
                    result = n.resume_quantum(
                        a,
                        &mut [&mut *real],
                        &mut wait,
                        &mut regions(&mut descriptor, errno),
                    )?
                }
                _ => return Err("Unexpected comparator continuation".into()),
            }
        }
    })();
    snapshot(a, data);
    run
}
fn log(data: &[u8]) -> Vec<u8> {
    let count = u64::from_le_bytes(data[256..264].try_into().unwrap()) as usize;
    assert!(count < 400);
    data[512..512 + 8 * count].to_vec()
}
fn integers(data: &mut [u8], values: &[i32]) {
    for (chunk, value) in data.chunks_exact_mut(4).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
}
fn read_ints(data: &[u8], count: usize) -> Vec<i32> {
    data[..count * 4]
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f.resize(0x1500, 0);
    f[152..160].copy_from_slice(&0x1000u64.to_le_bytes());
    f[160..168].copy_from_slice(&0x1000u64.to_le_bytes());
    f[96..104].copy_from_slice(&0x480u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x480u64.to_le_bytes());
    let code = [
        0x48, 0x8b, 0x47, 40, 0x4c, 0x8b, 0x47, 32, 0x48, 0x8b, 0x4f, 24, 0x48, 0x8b, 0x57, 16,
        0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff, 0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Comparator increments a visible counter, calls real errno service, and
    // preserves both argument pointers while returning a signed32 comparison.
    let mut comparator = vec![0x57, 0x56];
    fixed(&mut comparator, 0xb8, DATA + 416);
    comparator.extend([0x80, 0x38, 0, 0x74, 0]);
    let jump = comparator.len() - 1;
    fixed(&mut comparator, 0xbf, DATA + 400);
    comparator.extend([0x31, 0xf6]);
    call(&mut comparator, SERVICE + 96);
    comparator[jump] = (comparator.len() - jump - 1) as u8;
    call(&mut comparator, SERVICE + 32);
    comparator.extend([0x83, 0x00, 1, 0x5e, 0x5f]);
    fixed(&mut comparator, 0xb9, DATA + 256);
    comparator.extend([0x48, 0x8b, 0x01, 0x48, 0xc1, 0xe0, 3]);
    fixed(&mut comparator, 0xba, DATA + 512);
    comparator.extend([
        0x48, 0x01, 0xc2, 0x8b, 0x07, 0x89, 0x02, 0x8b, 0x06, 0x89, 0x42, 4, 0x48, 0xff, 0x01,
        0x8b, 0x07, 0x3b, 0x06, 0x0f, 0x9f, 0xc0, 0x0f, 0x9c, 0xc2, 0x0f, 0xb6, 0xc0, 0x0f, 0xb6,
        0xd2, 0x29, 0xd0, 0xc3,
    ]);
    f[0x200..0x200 + comparator.len()].copy_from_slice(&comparator);
    let pure = [
        0x8b, 0x07, 0x3b, 0x06, 0x0f, 0x9f, 0xc0, 0x0f, 0x9c, 0xc2, 0x0f, 0xb6, 0xc0, 0x0f, 0xb6,
        0xd2, 0x29, 0xd0, 0xc3,
    ];
    f[0x400..0x400 + pure.len()].copy_from_slice(&pure);
    // A guest comparator recursively performs a separate real SDL bsearch.
    let mut nested = vec![0x57, 0x56];
    fixed(&mut nested, 0xbf, DATA + 304);
    fixed(&mut nested, 0xbe, DATA + 320);
    fixed(&mut nested, 0xba, 3);
    fixed(&mut nested, 0xb9, 4);
    nested.extend([0x49, 0xb8]);
    nested.extend(0x100400u64.to_le_bytes());
    call(&mut nested, BIAS + 0xc51e0);
    nested.extend([0x5e, 0x5f]);
    fixed(&mut nested, 0xb8, 0x100200);
    nested.extend([0xff, 0xe0]);
    f[0x300..0x300 + nested.len()].copy_from_slice(&nested);
    // An adversarial callback tries to release the active array allocation.
    let mut release = Vec::new();
    fixed(&mut release, 0xb9, DATA + 392);
    release.extend([0x48, 0xff, 0x01]);
    fixed(&mut release, 0xb8, DATA + 384);
    release.extend([0x48, 0x8b, 0x38]);
    call(&mut release, SERVICE + 64);
    release.extend([0x31, 0xc0, 0xc3]);
    f[0x440..0x440 + release.len()].copy_from_slice(&release);
    let mut n = NativeExecutor::new(0x20000000, 65536, 1000000, 1000000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (target, service) in [
        (SERVICE, Service::Qsort),
        (SERVICE + 16, Service::Bsearch),
        (SERVICE + 32, Service::Errno),
        (SERVICE + 48, Service::Malloc),
        (SERVICE + 64, Service::Free),
        (SERVICE + 80, Service::Memcpy),
        (SERVICE + 96, Service::Nanosleep),
    ] {
        n.register_service(target, service)?;
    }
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    patch(&mut real, 0x26fa28, SERVICE)?;
    patch(&mut real, 0x26fa30, SERVICE + 16)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200, 0x300, 0x400, 0x440]),
            10000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xc51d0, 0xc51e0, 0x366c0]),
            10000,
            archive,
            &directory.join("sdl"),
        )?;
    }
    n.configure_manual_sleep_clock(0)?;
    let mut data = [0u8; 4096];
    let mut errno = 77u32.to_le_bytes();
    let values = [i32::MAX, -1, 0, 7, i32::MIN, 7, 3];
    integers(&mut data, &values);
    n.set_worker_quantum(None)?;
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 7, 4, 0x100200, 0, BIAS + 0xc51d0],
        &mut data,
        true,
        &mut errno,
    )?;
    let expected_log = log(&data);
    assert!(!expected_log.is_empty());
    data.fill(0);
    integers(&mut data, &values);
    errno = 77u32.to_le_bytes();
    n.set_worker_quantum(Some(3))?;
    let (_, yields) = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 7, 4, 0x100200, 0, BIAS + 0xc51d0],
        &mut data,
        true,
        &mut errno,
    )?;
    assert!(yields > 0);
    assert_eq!(
        log(&data),
        expected_log,
        "Comparator replay/reordering across quanta"
    );
    assert_eq!(
        read_ints(&data, 7),
        vec![i32::MIN, -1, 0, 3, 7, 7, i32::MAX]
    );
    let comparisons = u64::from_le_bytes(data[256..264].try_into().unwrap());
    assert!(comparisons > 0);
    assert_eq!(u32::from_le_bytes(errno), 77 + comparisons as u32);
    for key in [i32::MIN, -1, 0, 3, 7, i32::MAX, 8] {
        integers(&mut data[128..], &[key]);
        let (result, _) = invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 128, DATA, 7, 4, 0x100200, BIAS + 0xc51e0],
            &mut data,
            true,
            &mut errno,
        )?;
        if key == 8 {
            assert_eq!(result, 0)
        } else {
            assert!((DATA..DATA + 28).contains(&result));
            assert_eq!((result - DATA) % 4, 0);
            assert_eq!(read_ints(&data[(result - DATA) as usize..], 1), vec![key]);
        }
    }
    integers(&mut data, &[3, 1, 2]);
    integers(&mut data[304..], &[2]);
    integers(&mut data[320..], &[1, 2, 3]);
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 3, 4, 0x100300, 0, BIAS + 0xc51d0],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(read_ints(&data, 3), vec![1, 2, 3]);
    // Real 24-byte SDL display modes exercise width, height, format extraction
    // (including BEXTR), and refresh tie breaking in the original cmpmodes.
    data[..120].fill(0);
    for (index, (format, width, height, refresh)) in [
        (0u32, 1280i32, 720i32, 60i32),
        (0, 1920, 1080, 60),
        (0x12345678, 1920, 1200, 60),
        (0x12355678, 1920, 1200, 60),
        (0x12355678, 1920, 1200, 75),
    ]
    .into_iter()
    .enumerate()
    {
        let at = index * 24;
        data[at..at + 4].copy_from_slice(&format.to_le_bytes());
        data[at + 4..at + 8].copy_from_slice(&width.to_le_bytes());
        data[at + 8..at + 12].copy_from_slice(&height.to_le_bytes());
        data[at + 12..at + 16].copy_from_slice(&refresh.to_le_bytes());
    }
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 5, 24, BIAS + 0x366c0, 0, BIAS + 0xc51d0],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(&data[..4], &0x12355678u32.to_le_bytes());
    assert_eq!(&data[12..16], &75i32.to_le_bytes());
    assert_eq!(&data[36..40], &60i32.to_le_bytes());
    assert_eq!(&data[48..52], &0x12345678u32.to_le_bytes());
    assert_eq!(&data[100..104], &1280i32.to_le_bytes());
    // Zero searches do not call invalid comparators or access array/key pointers.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [u64::MAX, u64::MAX, 0, u64::MAX, u64::MAX, BIAS + 0xc51e0],
            &mut data,
            true,
            &mut errno
        )?
        .0,
        0
    );
    data.fill(0);
    integers(&mut data, &values);
    data[408..416].copy_from_slice(&1i64.to_le_bytes());
    data[416] = 1;
    errno = 77u32.to_le_bytes();
    blocked(&mut n, &mut a, &mut real, &mut data, &mut errno)?;
    assert_eq!(
        log(&data),
        expected_log,
        "Comparator replay/reordering across sleeps"
    );
    assert_eq!(
        read_ints(&data, 7),
        vec![i32::MIN, -1, 0, 3, 7, 7, i32::MAX]
    );
    data[416] = 0;
    n.select_guest_thread(1)?;
    // Guarded runtime-owned array cannot be freed while its callback is live.
    let heap = invoke(
        &mut n,
        &mut a,
        &mut real,
        [16, 0, 0, 0, 0, SERVICE + 48],
        &mut data,
        true,
        &mut errno,
    )?
    .0;
    assert_ne!(heap, 0);
    integers(&mut data, &[4, 3, 2, 1]);
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [heap, DATA, 16, 0, 0, SERVICE + 80],
        &mut data,
        true,
        &mut errno,
    )?;
    data[384..392].copy_from_slice(&heap.to_le_bytes());
    data[392..400].fill(0);
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [heap, 4, 4, 0x100440, 0, BIAS + 0xc51d0],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(&data[392..400], &1u64.to_le_bytes());
    println!(
        "Actual SDL qsort/bsearch guest callbacks, services, forced quanta and cmpmodes passed"
    );
    Ok(())
}

fn blocked(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    errno: &mut [u8],
) -> Result<(), String> {
    stage(a, data, true);
    let mut descriptor: Vec<u8> = [DATA, 7, 4, 0x100200, 0, BIAS + 0xc51d0]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    let mut scheduler = Scheduler::new(3, 100000, 1000000)?;
    scheduler.enqueue(n, 2, 0x100100, ARGS)?;
    let mut sleeps = 0;
    for _ in 0..100000 {
        match scheduler.step(
            n,
            a,
            &mut [&mut *real],
            &mut regions(&mut descriptor, errno),
        )? {
            Step::Sleeping { deadline_ns } => {
                sleeps += 1;
                let before = (
                    scheduler.remaining_dispatches(),
                    scheduler.remaining_instructions(),
                    n.consumed_instruction_fuel(),
                );
                for _ in 0..3 {
                    assert_eq!(
                        scheduler.step(
                            n,
                            a,
                            &mut [&mut *real],
                            &mut regions(&mut descriptor, errno)
                        )?,
                        Step::Sleeping { deadline_ns }
                    );
                }
                assert_eq!(
                    before,
                    (
                        scheduler.remaining_dispatches(),
                        scheduler.remaining_instructions(),
                        n.consumed_instruction_fuel()
                    )
                );
                n.advance_sleep_clock(deadline_ns)?;
            }
            Step::Finished => break,
            _ => {}
        }
    }
    assert!(sleeps > 0);
    assert!(scheduler.result(2).is_some());
    snapshot(a, data);
    assert_eq!(sleeps, log(data).len() / 8);
    Ok(())
}
