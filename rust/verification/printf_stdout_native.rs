//! Native SysV printf and caller-owned vprintf va_list to captured stdout.
//! Explicit C profile: integer/text/count and binary64 a/A only.
use native_ps5::{
    guest_memory::{DataRegion, Memory},
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    stdout_file::{FILE_ADDR, STDOUT_PTR_ADDR},
};
use std::{collections::BTreeSet, path::Path};
const TABLE: u64 = 0x31000000;
const DATA: u64 = 0x1_32000001;
const ERRNO: u64 = 0x33000000;
const PRINTF: u64 = 0x760000;
const VPRINTF: u64 = 0x760010;
const SUSPEND: u64 = 0x760020;
fn invoke(
    n: &mut NativeExecutor,
    image: &mut Image,
    data: &mut [u8],
    args: [u64; 10],
    fp: &[u64],
    al: u32,
    frame: bool,
) -> Result<u64, String> {
    let mut table = [0u8; 256];
    for (i, v) in args.iter().enumerate() {
        table[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    table[80..88]
        .copy_from_slice(&(if args[0] == u64::MAX { VPRINTF } else { PRINTF }).to_le_bytes());
    if args[0] == u64::MAX {
        table[..8].copy_from_slice(&DATA.to_le_bytes());
    }
    table[88..92].copy_from_slice(&al.to_le_bytes());
    for (i, v) in fp.iter().take(8).enumerate() {
        table[128 + i * 16..136 + i * 16].copy_from_slice(&v.to_le_bytes());
    }
    let mut errno = 77u32.to_le_bytes();
    let result = n.run_worker_with_data(
        image,
        if frame { 0x100300 } else { 0x100100 },
        TABLE,
        &mut [],
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
                writable: true,
            },
        ],
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("printf fixture suspended".into()),
    }
}
fn inspect(
    n: &mut NativeExecutor,
    i: &mut Image,
    f: impl FnOnce(&mut Memory<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let Execution::Dlsym {
        mut continuation, ..
    } = n.run(i, 0x100500, Arguments::default(), &mut [])?
    else {
        return Err("Expected printf inspection suspension".into());
    };
    n.with_suspended_memory(&mut continuation, i, &mut [], &mut [], f)
}
fn format(data: &mut [u8], text: &[u8]) {
    data.fill(0xa5);
    data[..text.len()].copy_from_slice(text);
    data[text.len()] = 0;
}
fn args() -> [u64; 10] {
    let mut a = [0; 10];
    a[0] = DATA;
    a
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    file.resize(0x600, 0);
    file[56..58].copy_from_slice(&1u16.to_le_bytes());
    file[96..104].copy_from_slice(&0x600u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x600u64.to_le_bytes());
    for (entry, frame) in [(0x100, false), (0x300, true)] {
        let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 0x20];
        for (source, destination) in [(48u8, 0u8), (56, 8), (64, 16), (72, 24)] {
            code.extend([
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
        for r in 0..8u8 {
            code.extend([0xc5, 0xfa, 0x6f, 0x83 + r * 8]);
            code.extend((128u32 + 16 * u32::from(r)).to_le_bytes());
        }
        code.extend([
            0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x53, 16, 0x48, 0x8b, 0x4b, 24,
            0x4c, 0x8b, 0x43, 32, 0x4c, 0x8b, 0x4b, 40,
        ]);
        if frame {
            code.extend([0x48, 0x8d, 0x74, 0x24, 0xf8]);
        }
        code.extend([
            0x8b, 0x43, 88, 0xff, 0x53, 80, 0x48, 0x83, 0xc4, 0x20, 0x5b, 0xc3,
        ]);
        assert!(code.len() < 0x200);
        file[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut suspend = vec![0x48, 0xb8];
    suspend.extend(SUSPEND.to_le_bytes());
    suspend.extend([0xff, 0xd0, 0xc3]);
    file[0x500..0x500 + suspend.len()].copy_from_slice(&suspend);
    let mut n = NativeExecutor::new(0x900000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (address, service) in [
        (PRINTF, Service::Printf),
        (VPRINTF, Service::Vprintf),
        (SUSPEND, Service::Dlsym),
    ] {
        n.register_service(address, service)?;
    }
    n.stdout.configure_capture(2048)?;
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x300, 0x500]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = vec![0; 2048];
    inspect(&mut n, &mut image, |m| {
        assert_eq!(m.load(FILE_ADDR + 172, 4)?, 0);
        Ok(())
    })?;
    format(&mut data, b"");
    assert_eq!(
        invoke(&mut n, &mut image, &mut data, args(), &[], 0, false)?,
        0
    );
    inspect(&mut n, &mut image, |m| {
        assert_eq!(m.load(FILE_ADDR + 172, 4)?, u32::MAX as u64);
        Ok(())
    })?;
    format(&mut data, b"%d %d %d %d %d %d %d");
    let mut a = args();
    a[1..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 13);
    assert_eq!(n.stdout.take_bytes(), b"1 2 3 4 5 6 7");
    format(&mut data, b"%a");
    assert_eq!(
        invoke(
            &mut n,
            &mut image,
            &mut data,
            args(),
            &[0x3ff8000000000000],
            1,
            false
        )?,
        8
    );
    assert_eq!(n.stdout.take_bytes(), b"0x1.8p+0");
    format(&mut data, b"%d|%a %a %a %a %a %a %a %a %a|%d");
    let mut a = args();
    a[1] = 3;
    a[2] = 4;
    a[6] = 0x3ff8000000000000;
    let expected =
        b"3|0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0 0x1.8p+0|4";
    assert_eq!(
        invoke(
            &mut n,
            &mut image,
            &mut data,
            a,
            &[0x3ff8000000000000; 8],
            8,
            false
        )?,
        expected.len() as u64
    );
    assert_eq!(n.stdout.take_bytes(), expected);
    // Explicit caller va_list: one saved GP, one saved FP, then GP overflow.
    format(&mut data, b"%d:%a:%d");
    data[1024..1028].copy_from_slice(&40u32.to_le_bytes());
    data[1028..1032].copy_from_slice(&48u32.to_le_bytes());
    data[1032..1040].copy_from_slice(&(DATA + 1279).to_le_bytes());
    data[1040..1048].copy_from_slice(&(DATA + 1088).to_le_bytes());
    data[1128..1136].copy_from_slice(&7u64.to_le_bytes());
    data[1136..1144].copy_from_slice(&0x3ff8000000000000u64.to_le_bytes());
    data[1279..1287].copy_from_slice(&9u64.to_le_bytes());
    let mut a = args();
    a[0] = u64::MAX;
    a[1] = DATA + 1024;
    assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 12);
    assert_eq!(n.stdout.take_bytes(), b"7:0x1.8p+0:9");
    assert_eq!(&data[1024..1028], &48u32.to_le_bytes());
    assert_eq!(&data[1028..1032], &64u32.to_le_bytes());
    assert_eq!(&data[1032..1040], &(DATA + 1287).to_le_bytes());
    format(&mut data, b"ab%d%nZ");
    let mut a = args();
    a[1] = 123;
    a[2] = DATA + 512;
    assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 6);
    assert_eq!(n.stdout.take_bytes(), b"ab123Z");
    assert_eq!(&data[512..516], &5u32.to_le_bytes());
    // %n does not flush deferred literal spans: observe alias before final read.
    format(&mut data, b"abc%hhn");
    let mut a = args();
    a[1] = DATA;
    assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 3);
    assert_eq!(n.stdout.take_bytes(), [3, b'b', b'c']);
    format(&mut data, b"%n");
    assert!(invoke(&mut n, &mut image, &mut data, args(), &[], 0, true).is_err());
    assert!(n.stdout.bytes().is_empty());
    // Output aliases must not corrupt the active stream or its pointer cell.
    for destination in [FILE_ADDR, STDOUT_PTR_ADDR] {
        format(&mut data, b"%n");
        let mut a = args();
        a[1] = destination;
        assert!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false).is_err());
        assert!(n.stdout.bytes().is_empty());
    }
    inspect(&mut n, &mut image, |m| m.store(STDOUT_PTR_ADDR, DATA, 8))?;
    format(&mut data, b"redirected");
    assert!(invoke(&mut n, &mut image, &mut data, args(), &[], 0, false).is_err());
    assert!(n.stdout.bytes().is_empty());
    inspect(&mut n, &mut image, |m| {
        assert_eq!(m.load(STDOUT_PTR_ADDR, 8)?, DATA);
        m.store(STDOUT_PTR_ADDR, FILE_ADDR, 8)
    })?;
    // Small successful vectors stay in __sbprintf's 1024-byte buffer and
    // are discarded if a later conversion fails.
    format(&mut data, b"%d%f");
    let mut a = args();
    a[1] = 7;
    assert!(invoke(&mut n, &mut image, &mut data, a, &[0], 1, false).is_err());
    assert!(n.stdout.take_bytes().is_empty());
    // A full source vector bypasses the empty buffer; accepted output survives
    // a later conversion failure.
    format(&mut data, b"%s%f");
    data[512..1536].fill(b'x');
    data[1536] = 0;
    let mut a = args();
    a[1] = DATA + 512;
    assert!(invoke(&mut n, &mut image, &mut data, a, &[0], 1, false).is_err());
    assert_eq!(n.stdout.take_bytes(), vec![b'x'; 1024]);
    // One final four-byte flush fails atomically with only two bytes available.
    n.stdout.write_bytes(&[b'x'; 2046])?;
    format(&mut data, b"%s%s");
    data[512..515].copy_from_slice(b"ab\0");
    data[520..523].copy_from_slice(b"cd\0");
    let mut a = args();
    a[1] = DATA + 512;
    a[2] = DATA + 520;
    assert!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false).is_err());
    assert_eq!(n.stdout.take_bytes(), vec![b'x'; 2046]);
    // A successful direct 1024-byte write precedes a failing final tail flush.
    n.stdout.write_bytes(&[b'p'; 1023])?;
    format(&mut data, b"%s%s");
    data[512..1536].fill(b'x');
    data[1536] = 0;
    data[1600..1603].copy_from_slice(b"ab\0");
    let mut a = args();
    a[1] = DATA + 512;
    a[2] = DATA + 1600;
    assert!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false).is_err());
    let bytes = n.stdout.take_bytes();
    assert_eq!(&bytes[..1023], &[b'p'; 1023]);
    assert_eq!(&bytes[1023..], &[b'x'; 1024]);
    println!(
        "PASS: native printf SysV five-GP/overflow and AL1/AL8 FP/overflow, mixed hex, caller-owned vprintf cursors, count/deferred-alias effects, empty byte orientation, partial flush failures and live return-frame guard; bounded C integer/text/hex profile"
    );
    Ok(())
}
