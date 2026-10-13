//! Native byte output for the explicit owned, unbuffered fd1 stdout profile.
//! Exercises the pinned I_PrintBanner inline _w decrement/__swbuf branch.
use native_ps5::{
    guest_memory::{DataRegion, Memory},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    stdout_file::{FILE_ADDR, STDOUT_PTR_ADDR},
};
use std::{collections::BTreeSet, path::Path};
const ARGS: u64 = 0x31000000;
const DATA: u64 = 0x32000000;
const BIAS: u64 = 0x10000000;
const FPUTC: u64 = 0x760000;
const PUTCHAR: u64 = 0x760010;
const SWBUF: u64 = 0x760020;
const PUTS: u64 = 0x760030;
const FLUSH: u64 = 0x760040;
const SUSPEND: u64 = 0x760050;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 3],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        0x100100,
        ARGS,
        &mut [real],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut table,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable: false,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Byte stdout fixture suspended".into()),
    }
}
fn inspect(
    n: &mut NativeExecutor,
    a: &mut Image,
    f: impl FnOnce(&mut Memory<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let Execution::Dlsym {
        mut continuation, ..
    } = n.run(a, 0x100180, Arguments::default(), &mut [])?
    else {
        return Err("Expected stdout inspection suspension".into());
    };
    n.with_suspended_memory(&mut continuation, a, &mut [], &mut [], f)
}
fn snapshot(n: &mut NativeExecutor, a: &mut Image) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    inspect(n, a, |m| {
        bytes = m.read(FILE_ADDR, 312)?.to_vec();
        Ok(())
    })?;
    Ok(bytes)
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48, 0x83, 0xec, 8, 0xff,
        0xd0, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut suspend = vec![0x48, 0xb8];
    suspend.extend(SUSPEND.to_le_bytes());
    suspend.extend([0xff, 0xd0, 0xc3]);
    fixture[0x180..0x180 + suspend.len()].copy_from_slice(&suspend);
    // Preserve the observed instruction bytes and RIP-relative GOT call at
    // their original guest address. Only loop exits become RET for isolation.
    let mut pinned = file.to_vec();
    let pattern = [
        0x8b, 0x46, 0x0c, 0x8d, 0x48, 0xff, 0x89, 0x4e, 0x0c, 0x85, 0xc0, 0x7f, 5, 0x3b, 0x46,
        0x28, 0x7e, 0x0f, 0x48, 0x8b, 6, 0x48, 0x8d, 0x48, 1, 0x48, 0x89, 0x0e, 0xc6, 0, 0x20,
        0xeb, 0xd0, 0xbf, 0x20, 0, 0, 0, 0xff, 0x15, 0x5d, 0x86, 0x15, 0, 0xeb, 0xc3,
    ];
    let sites: Vec<_> = pinned
        .windows(pattern.len())
        .enumerate()
        .filter_map(|(i, b)| (b == pattern).then_some(i))
        .collect();
    assert_eq!(sites.len(), 1, "Pinned banner byte sequence changed");
    let offset = sites[0];
    pinned[offset + 31] = 0xc3;
    pinned[offset + 32] = 0x90;
    pinned[offset + 44] = 0xc3;
    pinned[offset + 45] = 0x90;
    let mut n = NativeExecutor::new(0x900000, 65536, 10000, 10000)?;
    for (address, service) in [
        (FPUTC, Service::Fputc),
        (PUTCHAR, Service::Putchar),
        (SWBUF, Service::Swbuf),
        (PUTS, Service::Puts),
        (FLUSH, Service::Fflush),
        (SUSPEND, Service::Dlsym),
    ] {
        n.register_service(address, service)?;
    }
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(&pinned, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb40;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing __swbuf GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SWBUF.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            &pinned,
            BIAS,
            BTreeSet::from([0x1174b7]),
            1000,
            archive,
            &directory.join("banner"),
        )?;
    }
    n.stdout.configure_capture(16)?;
    inspect(&mut n, &mut adapter, |m| {
        assert_eq!(m.load(STDOUT_PTR_ADDR, 8)?, FILE_ADDR);
        assert_eq!(m.load(FILE_ADDR + 18, 2)?, 1);
        assert_eq!(m.load(FILE_ADDR + 12, 4)?, 0);
        assert_eq!(m.load(FILE_ADDR + 172, 4)?, 0);
        Ok(())
    })?;
    // stdout is an assignable FILE* object. Redirection persists in guest
    // memory, but this explicitly owned stream profile rejects its use.
    let before = snapshot(&mut n, &mut adapter)?;
    inspect(&mut n, &mut adapter, |m| m.store(STDOUT_PTR_ADDR, DATA, 8))?;
    inspect(&mut n, &mut adapter, |m| {
        assert_eq!(m.load(STDOUT_PTR_ADDR, 8)?, DATA);
        Ok(())
    })?;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [65, 0, PUTCHAR],
            &mut [0; 312]
        )
        .is_err()
    );
    assert!(n.stdout.bytes().is_empty());
    assert_eq!(snapshot(&mut n, &mut adapter)?, before);
    inspect(&mut n, &mut adapter, |m| {
        m.store(STDOUT_PTR_ADDR, FILE_ADDR, 8)
    })?;
    for target in [FPUTC, PUTCHAR, SWBUF] {
        for value in [
            -256i32,
            -1,
            0,
            1,
            127,
            128,
            255,
            256,
            511,
            i32::MIN,
            i32::MAX,
        ] {
            assert_eq!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [value as u32 as u64, FILE_ADDR, target],
                    &mut [0]
                )?,
                u64::from(value as u8)
            );
            assert_eq!(n.stdout.take_bytes(), [value as u8]);
            inspect(&mut n, &mut adapter, |m| {
                assert_eq!(m.load(FILE_ADDR + 12, 4)?, 0);
                assert_eq!(m.load(FILE_ADDR, 8)?, FILE_ADDR + 119);
                assert_eq!(m.load(FILE_ADDR + 119, 1)?, u64::from(value as u8));
                assert_eq!(m.load(FILE_ADDR + 172, 4)?, u32::MAX as u64);
                Ok(())
            })?;
        }
    }
    // Exact observed decrement from 0 to -1 reaches __swbuf, which restores _w.
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, FILE_ADDR, BIAS + 0x1174b7],
            &mut [0]
        )?,
        32
    );
    assert_eq!(n.stdout.take_bytes(), b" ");
    assert_eq!(&snapshot(&mut n, &mut adapter)?[12..16], &[0; 4]);
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [65, FILE_ADDR, FPUTC],
        &mut [0],
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, PUTS],
            &mut b"ok\0".to_vec()
        )?,
        10
    );
    invoke(&mut n, &mut adapter, &mut real, [255, 0, PUTCHAR], &mut [0])?;
    assert_eq!(n.stdout.bytes(), b"Aok\n\xff");
    for stream in [FILE_ADDR, 0] {
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [stream, 0, FLUSH],
                &mut [0]
            )?,
            0
        );
    }
    assert_eq!(n.stdout.take_bytes(), b"Aok\n\xff");
    for stream in [0, DATA, u64::MAX] {
        let before = snapshot(&mut n, &mut adapter)?;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [65, stream, FPUTC],
                &mut [0; 312]
            )
            .is_err()
        );
        assert!(n.stdout.bytes().is_empty());
        assert_eq!(snapshot(&mut n, &mut adapter)?, before);
    }
    // Guest corruption is rejected rather than silently normalizing FILE state.
    for off in [0u64, 12, 16, 18, 24, 32, 172] {
        let before = snapshot(&mut n, &mut adapter)?;
        inspect(&mut n, &mut adapter, |m| {
            m.store(FILE_ADDR + off, u64::from(before[off as usize] ^ 0x55), 1)
        })?;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [65, FILE_ADDR, FPUTC],
                &mut [0]
            )
            .is_err()
        );
        assert!(n.stdout.bytes().is_empty());
        inspect(&mut n, &mut adapter, |m| m.write(FILE_ADDR, &before))?;
    }
    for _ in 0..16 {
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [65, FILE_ADDR, FPUTC],
            &mut [0],
        )?;
    }
    let before = snapshot(&mut n, &mut adapter)?;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [66, FILE_ADDR, FPUTC],
            &mut [0]
        )
        .is_err()
    );
    assert_eq!(n.stdout.bytes(), &[65; 16]);
    assert_eq!(snapshot(&mut n, &mut adapter)?, before);
    n.stdout.take_bytes();
    println!(
        "PASS: translated fputc/putchar/__swbuf unsigned-byte returns, exact pinned banner inline decrement branch, ordered puts capture, unbuffered fd1 metadata/orientation, flush, writable stdout pointer with explicit redirection rejection, foreign stream/corrupt FILE guards, capture-capacity atomicity; explicit stdout profile only"
    );
    Ok(())
}
