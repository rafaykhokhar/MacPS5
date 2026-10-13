//! Native SysV fprintf/vfprintf for explicitly owned stdout and stderr.
//! Explicit C profile: integer/text/count and binary64 a/A only.
use native_ps5::{
    guest_memory::{DataRegion, Memory},
    guest_stdio::{FILE_ADDR as STDERR_FILE, STDERR_PTR_ADDR},
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
const FPRINTF: u64 = 0x760000;
const VFPRINTF: u64 = 0x760010;
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
    table[80..88].copy_from_slice(
        &(if args[9] == u64::MAX {
            VFPRINTF
        } else {
            FPRINTF
        })
        .to_le_bytes(),
    );
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
fn args(stream: u64) -> [u64; 10] {
    let mut a = [0; 10];
    a[0] = stream;
    a[1] = DATA;
    a
}
fn take(n: &mut NativeExecutor, stream: u64) -> Vec<u8> {
    if stream == FILE_ADDR {
        n.stdout.take_bytes()
    } else {
        n.stdio.take_bytes()
    }
}
fn isolated(n: &NativeExecutor, stream: u64) {
    if stream == FILE_ADDR {
        assert!(n.stdio.bytes().is_empty());
    } else {
        assert!(n.stdout.bytes().is_empty());
    }
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
            code.extend([0x48, 0x8d, 0x54, 0x24, 0xf8]);
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
        (FPRINTF, Service::Fprintf),
        (VFPRINTF, Service::Vfprintf),
        (SUSPEND, Service::Dlsym),
    ] {
        n.register_service(address, service)?;
    }
    n.stdout.configure_capture(2048)?;
    n.stdio.configure_capture(2048)?;
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
    for stream in [FILE_ADDR, STDERR_FILE] {
        inspect(&mut n, &mut image, |m| {
            assert_eq!(m.load(stream + 172, 4)?, 0);
            Ok(())
        })?;
        format(&mut data, b"");
        assert_eq!(
            invoke(&mut n, &mut image, &mut data, args(stream), &[], 0, false)?,
            0
        );
        inspect(&mut n, &mut image, |m| {
            assert_eq!(m.load(stream + 172, 4)?, u32::MAX as u64);
            Ok(())
        })?;
        format(&mut data, b"%d %d %d %d %d|%a");
        let mut a = args(stream);
        a[2..7].copy_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(
            invoke(
                &mut n,
                &mut image,
                &mut data,
                a,
                &[0x3ff8000000000000],
                1,
                false
            )?,
            18
        );
        isolated(&n, stream);
        assert_eq!(take(&mut n, stream), b"1 2 3 4 5|0x1.8p+0");
        // Caller va_list, one final saved GP then aligned stack overflow.
        format(&mut data, b"%d:%a:%d");
        data[1024..1028].copy_from_slice(&40u32.to_le_bytes());
        data[1028..1032].copy_from_slice(&48u32.to_le_bytes());
        data[1032..1040].copy_from_slice(&(DATA + 1279).to_le_bytes());
        data[1040..1048].copy_from_slice(&(DATA + 1088).to_le_bytes());
        data[1128..1136].copy_from_slice(&7u64.to_le_bytes());
        data[1136..1144].copy_from_slice(&0x3ff8000000000000u64.to_le_bytes());
        data[1279..1287].copy_from_slice(&9u64.to_le_bytes());
        let mut a = args(stream);
        a[2] = DATA + 1024;
        a[9] = u64::MAX;
        assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 12);
        isolated(&n, stream);
        assert_eq!(take(&mut n, stream), b"7:0x1.8p+0:9");
        assert_eq!(&data[1024..1028], &48u32.to_le_bytes());
        assert_eq!(&data[1028..1032], &64u32.to_le_bytes());
        assert_eq!(&data[1032..1040], &(DATA + 1287).to_le_bytes());
        format(&mut data, b"ab%d%nZ");
        let mut a = args(stream);
        a[2] = 123;
        a[3] = DATA + 512;
        assert_eq!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?, 6);
        assert_eq!(take(&mut n, stream), b"ab123Z");
        assert_eq!(&data[512..516], &5u32.to_le_bytes());
        format(&mut data, b"%n");
        assert!(invoke(&mut n, &mut image, &mut data, args(stream), &[], 0, true).is_err());
        assert!(take(&mut n, stream).is_empty());
        for destination in [
            stream,
            if stream == FILE_ADDR {
                STDOUT_PTR_ADDR
            } else {
                STDERR_PTR_ADDR
            },
        ] {
            format(&mut data, b"%n");
            let mut a = args(stream);
            a[2] = destination;
            assert!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false).is_err());
            assert!(take(&mut n, stream).is_empty());
        }
        format(&mut data, b"%d%f");
        let mut a = args(stream);
        a[2] = 7;
        assert!(invoke(&mut n, &mut image, &mut data, a, &[0], 1, false).is_err());
        assert!(take(&mut n, stream).is_empty());
        format(&mut data, b"%s%f");
        data[512..1536].fill(b'x');
        data[1536] = 0;
        let mut a = args(stream);
        a[2] = DATA + 512;
        assert!(invoke(&mut n, &mut image, &mut data, a, &[0], 1, false).is_err());
        isolated(&n, stream);
        assert_eq!(take(&mut n, stream), vec![b'x'; 1024]);
        // Existing accepted output plus one direct chunk, then a final tail
        // exceeding the remaining capacity; only successful batches survive.
        format(&mut data, b"%1023s");
        data[512] = 0;
        let mut a = args(stream);
        a[2] = DATA + 512;
        assert_eq!(
            invoke(&mut n, &mut image, &mut data, a, &[], 0, false)?,
            1023
        );
        format(&mut data, b"%s%s");
        data[512..1536].fill(b'x');
        data[1536] = 0;
        data[1600..1603].copy_from_slice(b"ab\0");
        let mut a = args(stream);
        a[2] = DATA + 512;
        a[3] = DATA + 1600;
        assert!(invoke(&mut n, &mut image, &mut data, a, &[], 0, false).is_err());
        let bytes = take(&mut n, stream);
        assert_eq!(&bytes[..1023], &[b' '; 1023]);
        assert_eq!(&bytes[1023..], &[b'x'; 1024]);
        isolated(&n, stream);
    }
    for stream in [0, DATA, u64::MAX] {
        format(&mut data, b"foreign");
        assert!(invoke(&mut n, &mut image, &mut data, args(stream), &[], 0, false).is_err());
        assert!(n.stdout.bytes().is_empty());
        assert!(n.stdio.bytes().is_empty());
    }
    println!(
        "PASS: native fprintf four GP plus overflow/mixed hex, vfprintf caller cursors, isolated owned stdout/stderr, empty orientation, count/frame/FILE/pointer guards, buffered discard and accepted 1024-byte prefix preservation, foreign FILE rejection"
    );
    Ok(())
}
