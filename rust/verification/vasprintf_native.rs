//! Execute the pinned asprintf_l wrapper with AL=0, real GP saves and stack args.
//! Integer/text/count surface only; no floating/positional/full-printf claim.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_67000001;
const TABLE: u64 = 0x68000000;
const ERRNO: u64 = 0x69000000;
const SERVICE: u64 = 0x850000;
const BIAS: u64 = 0x10000000;
struct Observed {
    result: u64,
    gp: u32,
    fp: u32,
    overflow: u64,
    caller_sp: u64,
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 10],
    data: &mut [u8],
    writable: bool,
    errno: Option<(&mut [u8], bool)>,
) -> Result<Observed, String> {
    let mut table = vec![0xa5; 112];
    for (i, v) in args.iter().enumerate() {
        table[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    let mut regions = vec![
        DataRegion {
            address: TABLE,
            bytes: &mut table,
            writable: true,
        },
        DataRegion {
            address: DATA,
            bytes: data,
            writable,
        },
    ];
    if let Some((bytes, writable)) = errno {
        regions.push(DataRegion {
            address: ERRNO,
            bytes,
            writable,
        });
    }
    let result = match n.run_worker_with_data(a, 0x100100, TABLE, &mut [real], &mut regions)? {
        Execution::WorkerReturned(v) => v,
        _ => return Err("vasprintf fixture unexpectedly suspended".into()),
    };
    drop(regions);
    Ok(Observed {
        result,
        gp: u32::from_le_bytes(table[80..84].try_into().unwrap()),
        fp: u32::from_le_bytes(table[84..88].try_into().unwrap()),
        overflow: u64::from_le_bytes(table[88..96].try_into().unwrap()),
        caller_sp: u64::from_le_bytes(table[96..104].try_into().unwrap()),
    })
}
fn format_args(strp: u64, format: u64, values: &[u64]) -> [u64; 10] {
    assert!(values.len() <= 6);
    let mut args = [0; 10];
    args[0] = strp;
    args[1] = 0xdeadbeef;
    args[2] = format;
    args[3..3 + values.len()].copy_from_slice(values);
    args[9] = BIAS + 0x115fd0;
    args
}
fn success(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    strp: u64,
    format: u64,
    values: &[u64],
    expected: &[u8],
    errno: Option<(&mut [u8], bool)>,
) -> Result<(), String> {
    let before = n.bytes_allocated();
    let observed = invoke(
        n,
        a,
        real,
        format_args(strp, format, values),
        data,
        true,
        errno,
    )?;
    assert_eq!(observed.result, expected.len() as u64);
    assert_eq!(observed.gp, 24 + 8 * values.len().min(3) as u32);
    assert_eq!(observed.fp, 48);
    assert_eq!(
        observed.overflow,
        observed.caller_sp + 8 * values.len().saturating_sub(3) as u64
    );
    let at = (strp - DATA) as usize;
    let pointer = u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    assert!(pointer > u32::MAX as u64);
    let mut copy = [0; 10];
    copy[0] = DATA + 2048;
    copy[1] = pointer;
    copy[2] = expected.len() as u64 + 1;
    copy[9] = SERVICE + 32;
    invoke(n, a, real, copy, data, true, None)?;
    assert_eq!(&data[2048..2048 + expected.len()], expected);
    assert_eq!(data[2048 + expected.len()], 0);
    assert!(n.bytes_allocated() > before);
    let mut args = [0; 10];
    args[0] = pointer;
    args[9] = SERVICE + 16;
    invoke(n, a, real, args, data, true, None)?;
    assert_eq!(n.bytes_allocated(), before);
    Ok(())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    // SysV call site: 3 varargs in RCX/R8/R9, 3 at [RSP+0/8/16]. The
    // real wrapper leaves its descriptor at caller RSP-48; read it after return
    // before reusing this still-owned stack, recording actual cursor updates.
    let code: [u8; 105] = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 0x20, 0x48, 0x8b, 0x43, 0x30, 0x48, 0x89, 0x04,
        0x24, 0x48, 0x8b, 0x43, 0x38, 0x48, 0x89, 0x44, 0x24, 0x08, 0x48, 0x8b, 0x43, 0x40, 0x48,
        0x89, 0x44, 0x24, 0x10, 0x48, 0x89, 0x63, 0x60, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 0x08,
        0x48, 0x8b, 0x53, 0x10, 0x48, 0x8b, 0x4b, 0x18, 0x4c, 0x8b, 0x43, 0x20, 0x4c, 0x8b, 0x4b,
        0x28, 0x31, 0xc0, 0xff, 0x53, 0x48, 0x8b, 0x4c, 0x24, 0xd0, 0x89, 0x4b, 0x50, 0x8b, 0x4c,
        0x24, 0xd4, 0x89, 0x4b, 0x54, 0x48, 0x8b, 0x4c, 0x24, 0xd8, 0x48, 0x89, 0x4b, 0x58, 0x48,
        0x83, 0xc4, 0x20, 0x5b, 0xc3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Vasprintf)?;
    n.register_service(SERVICE + 16, Service::Free)?;
    n.register_service(SERVICE + 32, Service::Memcpy)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fac0;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing vasprintf GOT")?;
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
            BTreeSet::from([0x115fd0]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut data = vec![0xa5; 4096];
    let mut errno = 77u32.to_le_bytes();
    for (format, args, expected) in [
        (&b"\0"[..], vec![], &b""[..]),
        (
            &b"[%d][%#x][%s][%lld][%c][%u]\0"[..],
            vec![
                (-17i32) as u32 as u64,
                42,
                DATA + 256,
                0x8000000000000000,
                0,
                42,
            ],
            &b"[-17][0x2a][abcdef][-9223372036854775808][\0][42]"[..],
        ),
        (
            &b"%*.*s|%hhd|%hd|%zu\0"[..],
            vec![(-6i32) as u32 as u64, 3, DATA + 256, 255, 65535, 123],
            &b"abc   |-1|-1|123"[..],
        ),
    ] {
        data.fill(0xa5);
        data[64..64 + format.len()].copy_from_slice(format);
        data[256..263].copy_from_slice(b"abcdef\0");
        success(
            &mut n,
            &mut adapter,
            &mut real,
            &mut data,
            DATA,
            DATA + 64,
            &args,
            expected,
            Some((&mut errno, false)),
        )?;
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // Growth crosses the initial logical127-byte payload capacity. No errno map.
    data.fill(0xa5);
    data[64..67].copy_from_slice(b"%s\0");
    data[256..1256].fill(b'Q');
    data[1256] = 0;
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        DATA,
        DATA + 64,
        &[DATA + 256],
        &vec![b'Q'; 1000],
        None,
    )?;
    // %n writes before flushing the earlier literal: live guest span changes.
    data.fill(0xa5);
    data[64..72].copy_from_slice(b"abc%n%s\0");
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        DATA,
        DATA + 64,
        &[DATA + 64, DATA + 64],
        &[3, 0, 0, 3],
        None,
    )?;
    assert_eq!(&data[64..68], &3u32.to_le_bytes());
    // Every accepted count width, including values arriving on the stack.
    data.fill(0xa5);
    let format = b"A%hhnB%hnC%nD%lnE%llnF%jn\0";
    data[64..64 + format.len()].copy_from_slice(format);
    let args: Vec<_> = (0..6).map(|i| DATA + 512 + 16 * i).collect();
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        DATA,
        DATA + 64,
        &args,
        b"ABCDEF",
        None,
    )?;
    for (i, size) in [1, 2, 4, 8, 8, 8].into_iter().enumerate() {
        let at = 512 + i * 16;
        assert_eq!(&data[at..at + size], &(i as u64 + 1).to_le_bytes()[..size]);
        assert_eq!(data[at + size], 0xa5);
    }
    data.fill(0xa5);
    let format = b"X%znY%tn\0";
    data[64..64 + format.len()].copy_from_slice(format);
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        DATA,
        DATA + 64,
        &[DATA + 512, DATA + 528],
        b"XY",
        None,
    )?;
    assert_eq!(&data[512..520], &1u64.to_le_bytes());
    assert_eq!(&data[528..536], &2u64.to_le_bytes());
    // Publication may overwrite consumed format storage only after rendering.
    data.fill(0xa5);
    data[64..70].copy_from_slice(b"alias\0");
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        DATA + 64,
        DATA + 64,
        &[],
        b"alias",
        None,
    )?;
    // Guard failure stays an explicit runtime error and does not leak output.
    data.fill(0xa5);
    data[64..69].copy_from_slice(b"safe\0");
    let before = n.bytes_allocated();
    let original = data.clone();
    let error = match invoke(
        &mut n,
        &mut adapter,
        &mut real,
        format_args(DATA, DATA + 64, &[]),
        &mut data,
        false,
        None,
    ) {
        Err(e) => e,
        Ok(_) => return Err("Readonly strp unexpectedly succeeded".into()),
    };
    assert!(error.contains("vasprintf"), "{error}");
    assert_eq!(data, original);
    assert_eq!(n.bytes_allocated(), before);
    println!(
        "PASS: actual asprintf_l wrapper AL0 GP/overflow va_list advancement, integer/text/star/length formatting, embeddedNUL, allocated output/growth/free, %n widths and deferred-literal alias, finalpointer/format alias, readonly/unmapped errno success and readonly output guard; allocationfailure and floating paths not nativeverified"
    );
    Ok(())
}
