//! Actual pinned asprintf_l AL/XMM-save and stack-FP path with owned hdtoa.
//! Binary64 a/A only; no decimal, extended80 or complete printf/gameplay claim.
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
const POOL: u64 = 0x7fff00002000;
struct Observed {
    result: u64,
    gp: u32,
    fp: u32,
    overflow: u64,
    sp: u64,
    mxcsr: u32,
}
fn invoke(
    n: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    args: [u64; 10],
    fp: &[u64],
    control: u32,
    al: u32,
) -> Result<Observed, String> {
    let mut table = vec![0; 256];
    for (i, v) in args.iter().enumerate() {
        table[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    for (i, v) in fp.iter().take(8).enumerate() {
        table[128 + i * 16..136 + i * 16].copy_from_slice(&v.to_le_bytes());
    }
    table[120..124].copy_from_slice(&control.to_le_bytes());
    table[124..128].copy_from_slice(&al.to_le_bytes());
    let mut errno = 77u32.to_le_bytes();
    let result = n.run_worker_with_data(
        adapter,
        0x100100,
        TABLE,
        &mut [real],
        &mut [
            DataRegion {
                address: TABLE,
                bytes: &mut table,
                writable: true,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable: true,
            },
            DataRegion {
                address: ERRNO,
                bytes: &mut errno,
                writable: false,
            },
        ],
    )?;
    assert_eq!(errno, 77u32.to_le_bytes());
    let Execution::WorkerReturned(result) = result else {
        return Err("Hex wrapper unexpectedly suspended".into());
    };
    Ok(Observed {
        result,
        gp: u32::from_le_bytes(table[80..84].try_into().unwrap()),
        fp: u32::from_le_bytes(table[84..88].try_into().unwrap()),
        overflow: u64::from_le_bytes(table[88..96].try_into().unwrap()),
        sp: u64::from_le_bytes(table[96..104].try_into().unwrap()),
        mxcsr: u32::from_le_bytes(table[104..108].try_into().unwrap()),
    })
}
fn args(format: u64) -> [u64; 10] {
    let mut args = [0; 10];
    args[0] = DATA;
    args[1] = 0xdeadbeef;
    args[2] = format;
    args[9] = BIAS + 0x115fd0;
    args
}
fn success(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    data: &mut [u8],
    format: &[u8],
    fp: &[u64],
    control: u32,
    expected: &[u8],
    raised: u32,
) -> Result<(), String> {
    data.fill(0xa5);
    data[64..64 + format.len()].copy_from_slice(format);
    let mut arguments = args(DATA + 64);
    for (i, bits) in fp.iter().skip(8).enumerate() {
        arguments[6 + i] = *bits;
    }
    let observed = invoke(
        n,
        a,
        real,
        data,
        arguments,
        fp,
        control,
        fp.len().min(8) as u32,
    )?;
    assert_eq!(observed.result, expected.len() as u64);
    assert_eq!(observed.gp, 24);
    assert_eq!(observed.fp, 48 + 16 * fp.len().min(8) as u32);
    assert_eq!(
        observed.overflow,
        observed.sp + 8 * fp.len().saturating_sub(8) as u64
    );
    assert_eq!(observed.mxcsr, control | raised);
    let pointer = u64::from_le_bytes(data[..8].try_into().unwrap());
    let mut copy = [0; 10];
    copy[0] = DATA + 4096;
    copy[1] = pointer;
    copy[2] = expected.len() as u64 + 1;
    copy[9] = SERVICE + 32;
    invoke(n, a, real, data, copy, &[], 0x1f80, 0)?;
    assert_eq!(&data[4096..4096 + expected.len()], expected);
    assert_eq!(data[4096 + expected.len()], 0);
    let mut free = [0; 10];
    free[0] = pointer;
    free[9] = SERVICE + 16;
    invoke(n, a, real, data, free, &[], 0x1f80, 0)?;
    Ok(())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    fixture.resize(0x600, 0);
    fixture[56..58].copy_from_slice(&1u16.to_le_bytes());
    fixture[96..104].copy_from_slice(&0x600u64.to_le_bytes());
    fixture[104..112].copy_from_slice(&0x600u64.to_le_bytes());
    let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 0x20]; // RBX=table; aligned caller stack.
    for (source, destination) in [(48u8, 0u8), (56, 8), (64, 16)] {
        code.extend_from_slice(&[
            0x48,
            0x8b,
            0x43,
            source,
            0x48,
            0x89,
            0x44,
            0x24,
            destination,
        ]);
    }
    code.extend_from_slice(&[0x48, 0x89, 0x63, 0x60]); // saved caller RSP
    code.extend_from_slice(&[0x0f, 0xae, 0x5b, 112, 0x0f, 0xae, 0x53, 120]); // STMXCSR old; LDMXCSR requested
    for register in 0..8u8 {
        // Actual translated XMM loads, no Cpu field injection.
        code.extend_from_slice(&[0xc5, 0xfa, 0x6f, 0x83 + register * 8]);
        code.extend_from_slice(&(128u32 + u32::from(register) * 16).to_le_bytes());
    }
    code.extend_from_slice(&[
        0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x53, 16, 0x48, 0x8b, 0x4b, 24, 0x4c,
        0x8b, 0x43, 32, 0x4c, 0x8b, 0x4b, 40, 0x8b, 0x43, 124, 0xff, 0x53,
        72, // EAX supplies actual AL1 or AL8 to asprintf_l
        0x0f, 0xae, 0x5b, 104, // observed post-call MXCSR
        0x0f, 0xae, 0x53, 112, // restore caller controls after observation
        0x8b, 0x4c, 0x24, 0xd0, 0x89, 0x4b, 80, 0x8b, 0x4c, 0x24, 0xd4, 0x89, 0x4b, 84, 0x48, 0x8b,
        0x4c, 0x24, 0xd8, 0x48, 0x89, 0x4b, 88, 0x48, 0x83, 0xc4, 0x20, 0x5b, 0xc3,
    ]);
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    fixture[0x400..0x40a]
        .copy_from_slice(&[0x48, 0x8b, 0x07, 0xc6, 0x00, 1, 0x31, 0xc0, 0xc3, 0x90]); // translated protected store
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
    let offset = (got - segment.address) as usize;
    segment.bytes[offset..offset + 8].copy_from_slice(&SERVICE.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100, 0x400]),
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
    let mut data = vec![0xa5; 8192];
    let baseline = n.bytes_allocated();
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        b"%+012.1a\0",
        &[1.5f64.to_bits()],
        0x1f80,
        b"+0x0001.8p+0",
        0,
    )?;
    assert_eq!(n.bytes_allocated(), baseline);
    for (control, expected) in [
        (0x1f80, b"0X1P+1".as_slice()),
        (0x3f80, b"0X1P+0"),
        (0x5f80, b"0X1P+1"),
        (0x7f80, b"0X1P+0"),
    ] {
        success(
            &mut n,
            &mut adapter,
            &mut real,
            &mut data,
            b"%.0A\0",
            &[1.5f64.to_bits()],
            control,
            expected,
            32,
        )?;
    }
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        b"%.0a\0",
        &[3],
        0x1f80,
        b"0x1p-1072",
        34,
    )?;
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        b"%+A\0",
        &[0xfff0000000000001],
        0x1f80,
        b"NAN",
        0,
    )?;
    let values: Vec<u64> = (1..=10).map(|v| (v as f64).to_bits()).collect();
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        b"%a|%a|%a|%a|%a|%a|%a|%a|%a|%a\0",
        &values,
        0x1f80,
        b"0x1p+0|0x1p+1|0x1.8p+1|0x1p+2|0x1.4p+2|0x1.8p+2|0x1.cp+2|0x1p+3|0x1.2p+3|0x1.4p+3",
        0,
    )?;
    assert_eq!(n.bytes_allocated(), baseline);
    // Faulting %n after successful hdtoa must clean private output/retained text.
    data.fill(0xa5);
    data[64..69].copy_from_slice(b"%a%n\0");
    let mut arguments = args(DATA + 64);
    arguments[3] = POOL + 4;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut data,
            arguments,
            &[1.5f64.to_bits()],
            0x1f80,
            1
        )
        .is_err()
    );
    assert_eq!(n.bytes_allocated(), baseline);
    assert_eq!(&data[..8], &[0xa5; 8]);
    data[64..67].copy_from_slice(b"%a\0");
    let mut arguments = args(DATA + 64);
    arguments[0] = POOL;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut data,
            arguments,
            &[1.0f64.to_bits()],
            0x1f80,
            1
        )
        .is_err()
    );
    assert_eq!(n.bytes_allocated(), baseline);
    let mut pointer = POOL.to_le_bytes();
    assert!(
        n.run_worker_with_data(
            &mut adapter,
            0x100400,
            TABLE,
            &mut [&mut real],
            &mut [DataRegion {
                address: TABLE,
                bytes: &mut pointer,
                writable: true
            }]
        )
        .is_err()
    );
    // Check normal operation after rejected writes and retained-buffer cleanup.
    success(
        &mut n,
        &mut adapter,
        &mut real,
        &mut data,
        b"%a\0",
        &[2.0f64.to_bits()],
        0x1f80,
        b"0x1p+1",
        0,
    )?;
    println!(
        "Native asprintf_l hex verified: AL1/AL8 XMM saves, 10 FP arguments/spills, four RC modes, DE/PE, NaN, cursor and owned-pool guards"
    );
    Ok(())
}
