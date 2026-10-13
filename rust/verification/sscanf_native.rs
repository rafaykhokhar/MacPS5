//! Direct SysV sscanf plus complete pinned M_StrToInt caller; import verified.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_31000001;
const TABLE: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
const SERVICE: u64 = 0x790000;
const BIAS: u64 = 0x10000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    data: &mut [u8],
    args: [u64; 9],
    writable: bool,
    frame: bool,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut errno = 77u32.to_le_bytes();
    let result = n.run_worker_with_data(
        a,
        if frame { 0x100200 } else { 0x100100 },
        TABLE,
        &mut [r],
        &mut [
            DataRegion {
                address: TABLE,
                bytes: &mut table,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable,
            },
            DataRegion {
                address: ERRNO,
                bytes: &mut errno,
                writable: false,
            },
        ],
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("sscanf fixture suspended".into()),
    }
}
fn prepare(data: &mut [u8], input: &[u8], format: &[u8]) -> [u64; 9] {
    data.fill(0xa5);
    data[..input.len()].copy_from_slice(input);
    data[input.len()] = 0;
    data[128..128 + format.len()].copy_from_slice(format);
    data[128 + format.len()] = 0;
    let mut a = [0; 9];
    a[0] = DATA;
    a[1] = DATA + 128;
    for (i, v) in a[2..8].iter_mut().enumerate() {
        *v = DATA + 512 + i as u64 * 8;
    }
    a[8] = SERVICE;
    a
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut inventory = Image::load(file, 0, 64 * 1024 * 1024)?;
    inventory.relocate(|_| None)?;
    assert!(
        inventory
            .unresolved
            .iter()
            .any(|x| x.name == "sscanf" && x.address == 0x26fb78)
    );
    let mut fixture = memory_fixture();
    fixture[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    fixture[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    for (entry, frame) in [(0x100, false), (0x200, true)] {
        let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 16];
        for (source, dest) in [(48u8, 0u8), (56, 8)] {
            code.extend([0x48, 0x8b, 0x43, source, 0x48, 0x89, 0x44, 0x24, dest]);
        }
        code.extend([
            0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x53, 16, 0x48, 0x8b, 0x4b, 24,
            0x4c, 0x8b, 0x43, 32, 0x4c, 0x8b, 0x4b, 40,
        ]);
        if frame {
            code.extend([0x48, 0x8d, 0x54, 0x24, 0xf8]);
        }
        code.extend([0x31, 0xc0, 0xff, 0x53, 64, 0x48, 0x83, 0xc4, 16, 0x5b, 0xc3]);
        fixture[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Sscanf)?;
    let mut a = Image::load(&fixture, 0x100000, 65536)?;
    let mut r = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb78;
    let segment = r
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing sscanf GOT")?;
    let off = (got - segment.address) as usize;
    segment.bytes[off..off + 8].copy_from_slice(&SERVICE.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x118b70]),
            1000,
            archive,
            &directory.join("M_StrToInt"),
        )?;
    }
    let mut data = vec![0xa5; 1024];
    let args = prepare(&mut data, b"1 2 3 4 5 6", b"%d %d %d %d %d %d");
    assert_eq!(
        invoke(&mut n, &mut a, &mut r, &mut data, args, true, false)?,
        6
    );
    for i in 0..6 {
        assert_eq!(
            &data[512 + 8 * i..516 + 8 * i],
            &((i + 1) as u32).to_le_bytes()
        );
        assert_eq!(&data[516 + 8 * i..520 + 8 * i], &[0xa5; 4]);
    }
    let args = prepare(&mut data, b"12 34 xyz", b"%*d %d %n%3s");
    assert_eq!(
        invoke(&mut n, &mut a, &mut r, &mut data, args, true, false)?,
        2
    );
    assert_eq!(&data[512..516], &34u32.to_le_bytes());
    assert_eq!(&data[520..524], &6u32.to_le_bytes());
    assert_eq!(&data[528..532], b"xyz\0");
    for (input, expected) in [(b"".as_slice(), u32::MAX as u64), (b"no", 0)] {
        let args = prepare(&mut data, input, b"%d");
        assert_eq!(
            invoke(&mut n, &mut a, &mut r, &mut data, args, true, false)?,
            expected
        );
        assert_eq!(&data[512..516], &[0xa5; 4]);
    }
    for (input, expected, result) in [
        (b"42".as_slice(), 42u32, 1),
        (b"0x2a", 42, 1),
        (b"no", 0xa5a5a5a5, 0),
    ] {
        let mut args = prepare(&mut data, input, b"");
        args[1] = DATA + 512;
        args[8] = BIAS + 0x118b70;
        assert_eq!(
            invoke(&mut n, &mut a, &mut r, &mut data, args, true, false)?,
            result
        );
        assert_eq!(&data[512..516], &expected.to_le_bytes());
    }
    for (mode, writable, frame) in [
        (0, false, false),
        (1, true, false),
        (2, true, false),
        (3, true, true),
    ] {
        let mut args = prepare(&mut data, b"123", b"%d");
        if mode == 1 {
            args[2] = DATA + 1022;
        }
        if mode == 2 {
            args[2] = DATA;
        }
        let before = data.clone();
        assert!(invoke(&mut n, &mut a, &mut r, &mut data, args, writable, frame).is_err());
        assert_eq!(data, before);
    }
    // Planner validates later assignments before publishing any earlier writes.
    let mut args = prepare(&mut data, b"1 2", b"%d %d");
    args[3] = u64::MAX;
    let before = data.clone();
    assert!(invoke(&mut n, &mut a, &mut r, &mut data, args, true, false).is_err());
    assert_eq!(data, before);
    println!(
        "PASS: native sscanf four GP plus two stack pointers, suppression/count/string/EOF/mismatch, actual named-import M_StrToInt caller, full64 pointers, readonly/short/source-alias/live-frame/later-write guards, unchanged errno"
    );
    Ok(())
}
