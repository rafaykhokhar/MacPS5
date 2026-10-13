//! Pinned strftime_l tail wrapper: C-locale bytes and guest tm layout. No %s,
//! arbitrary timezone/locale or full time-library compatibility claim.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_7a000001;
const ARGS: u64 = 0x7b000000;
const SERVICE: u64 = 0x870000;
const BIAS: u64 = 0x10000000;
const DEST: usize = 1024;
const TM: usize = 512;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 6],
    data: &mut [u8],
    writable: bool,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut regions = [
        DataRegion {
            address: ARGS,
            bytes: &mut table,
            writable: false,
        },
        DataRegion {
            address: DATA,
            bytes: data,
            writable,
        },
    ];
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [real], &mut regions)? {
        Execution::WorkerReturned(value) => Ok(value),
        _ => Err("strftime fixture suspended".into()),
    }
}
fn arguments(capacity: u64, format: u64, tm: u64) -> [u64; 6] {
    [
        DATA + DEST as u64,
        capacity,
        format,
        tm,
        u64::MAX,
        BIAS + 0x1161c0,
    ]
}
fn reset(data: &mut [u8], format: &[u8]) {
    data.fill(0xa5);
    data[..format.len()].copy_from_slice(format);
    data[TM..TM + 56].fill(0);
    for (i, value) in [56i32, 34, 13, 13, 9, 126, 2, 285, 0]
        .into_iter()
        .enumerate()
    {
        data[TM + i * 4..TM + i * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 40, 0x4c, 0x8b, 0x47, 32, 0x48, 0x8b, 0x4f, 24, 0x48, 0x8b, 0x57, 16,
        0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48, 0x83, 0xec, 8, 0xff, 0xd0, 0x48, 0x83, 0xc4,
        8, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.guest_time.configure_utc();
    n.register_service(SERVICE, Service::Strftime)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fad8;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strftime GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SERVICE.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1161c0]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut data = vec![0xa5; 2048];
    for (format, expected) in [
        (
            b"%c|%F|%r|%j|%G-%V-%g|%z %Z\0".as_slice(),
            b"Tue Oct 13 13:34:56 2026|2026-10-13|01:34:56 PM|286|2026-42-26|+0000 UTC".as_slice(),
        ),
        (
            b"%_d|%-m|%0e|%OB|%EY|%EE|%q|%%|%\0",
            b"13|10|13|October|2026|E|q|%|%",
        ),
    ] {
        reset(&mut data, format);
        let before = data.clone();
        let allocated = n.bytes_allocated();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                arguments(512, DATA, DATA + TM as u64),
                &mut data,
                true
            )?,
            expected.len() as u64
        );
        assert_eq!(&data[DEST..DEST + expected.len()], expected);
        assert_eq!(data[DEST + expected.len()], 0);
        assert_eq!(data[DEST - 1], 0xa5);
        assert_eq!(data[DEST + expected.len() + 1], 0xa5);
        assert_eq!(&data[..DEST], &before[..DEST]);
        assert_eq!(n.bytes_allocated(), allocated);
    }
    for cap in 0..=5u64 {
        reset(&mut data, b"abcd\0");
        let got = invoke(
            &mut n,
            &mut adapter,
            &mut real,
            arguments(cap, DATA, u64::MAX),
            &mut data,
            true,
        )?;
        assert_eq!(got, if cap == 5 { 4 } else { 0 });
        let expected = if cap == 5 {
            b"abcd\0".as_slice()
        } else {
            &b"abcd"[..cap as usize]
        };
        assert_eq!(&data[DEST..DEST + expected.len()], expected);
        assert_eq!(data[DEST + expected.len()], 0xa5);
    }
    reset(&mut data, b"\0");
    let mut args = arguments(0, DATA, u64::MAX);
    args[0] = u64::MAX;
    assert_eq!(
        invoke(&mut n, &mut adapter, &mut real, args, &mut data, false)?,
        0
    );
    reset(&mut data, b"unused\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            arguments(512, 0, DATA + TM as u64),
            &mut data,
            true
        )?,
        24
    );
    assert_eq!(&data[DEST..DEST + 25], b"Tue Oct 13 13:34:56 2026\0");
    reset(&mut data, b"%z|%Z\0");
    data[TM + 40..TM + 48].copy_from_slice(&(-19800i64).to_le_bytes());
    data[TM + 48..TM + 56].copy_from_slice(&(DATA + 256).to_le_bytes());
    data[256..263].copy_from_slice(b"CUSTOM\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            arguments(512, DATA, DATA + TM as u64),
            &mut data,
            true
        )?,
        12
    );
    assert_eq!(&data[DEST..DEST + 13], b"-0530|CUSTOM\0");
    reset(&mut data, b"%Y-%m-%d\0");
    let before = data.clone();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            arguments(512, DATA, DATA + TM as u64),
            &mut data,
            false
        )
        .is_err()
    );
    assert_eq!(data, before);
    reset(&mut data, b"%Y\0");
    let mut args = arguments(512, DATA, DATA + TM as u64);
    args[0] = DATA;
    let before = data.clone();
    assert!(invoke(&mut n, &mut adapter, &mut real, args, &mut data, true).is_err());
    assert_eq!(data, before);
    reset(&mut data, b"abc%s\0");
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            arguments(512, DATA, DATA + TM as u64),
            &mut data,
            true
        )
        .is_err()
    );
    assert_eq!(&data[DEST..DEST + 4], b"abc\xa5");
    println!(
        "PASS: actual strftime_l tail wrapper, C names/composites/ISO week and modifiers, lazy tm and NULL-format, full64 arguments/ignored locale, exact-capacity prefix/noNUL, tm_gmtoff@40/zone@48 with UTC fallback, output canaries/readonly/alias guards, unsupported%s prefix retained; mktime/tzset/globalwarning and arbitrarylocale remain gaps"
    );
    Ok(())
}
