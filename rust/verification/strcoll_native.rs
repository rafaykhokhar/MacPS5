//! Actual pinned strcoll_l tail wrapper, current C/POSIX locale profile only.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_77000001;
const ARGS: u64 = 0x78000000;
const ERRNO: u64 = 0x79000000;
const SERVICE: u64 = 0x860000;
const BIAS: u64 = 0x10000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: Option<&mut [u8]>,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut regions = vec![
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
    ];
    if let Some(bytes) = errno {
        regions.push(DataRegion {
            address: ERRNO,
            bytes,
            writable: false,
        });
    }
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [real], &mut regions)? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("strcoll fixture suspended".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48,
        0x83, 0xec, 8, 0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Strcoll)?;
    n.register_service(SERVICE + 16, Service::Setlocale)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fad0;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strcoll GOT")?;
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
            BTreeSet::from([0x1161b0]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut errno = 91u32.to_le_bytes();
    let mut data = vec![0xa5; 256];
    // Both supported spellings select the owned C collation. The pinned wrapper
    // tail-calls plain strcoll and does not dereference its third locale argument.
    for locale in [&b"C\0"[..], &b"POSIX\0"[..]] {
        data[128..128 + locale.len()].copy_from_slice(locale);
        assert_ne!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [1, DATA + 128, 0, SERVICE + 16],
                &mut data,
                None
            )?,
            0
        );
        for (left, right, expected) in [
            (&b"\0"[..], &b"\0"[..], 0i32),
            (&b"A\0"[..], &b"a\0"[..], -32),
            (&b"abc\0"[..], &b"abcd\0"[..], -100),
            (&b"same\0"[..], &b"same\0"[..], 0),
            (&b"\xff\0"[..], &b"\x01\0"[..], 254),
            (&b"\x80\0"[..], &b"\xff\0"[..], -127),
            (&b"a\0ignored"[..], &b"a\0different"[..], 0),
        ] {
            data[..32].fill(0xa5);
            data[32..64].fill(0xa5);
            data[..left.len()].copy_from_slice(left);
            data[32..32 + right.len()].copy_from_slice(right);
            let before = data.clone();
            let allocated = n.bytes_allocated();
            for target in [SERVICE, BIAS + 0x1161b0] {
                assert_eq!(
                    invoke(
                        &mut n,
                        &mut adapter,
                        &mut real,
                        [DATA, DATA + 32, 0xdeadbeef, target],
                        &mut data,
                        Some(&mut errno)
                    )?,
                    expected as u32 as u64
                );
            }
            assert_eq!(data, before);
            assert_eq!(n.bytes_allocated(), allocated);
            assert_eq!(errno, 91u32.to_le_bytes());
        }
    }
    // Identical addresses still read through the terminator; no pointer shortcut.
    data[..5].copy_from_slice(b"same\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x1161b0],
            &mut data,
            None
        )?,
        0
    );
    // A mismatch succeeds without looking for a terminator outside either input.
    let mut tiny = [255, 1];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA + 1, 0, BIAS + 0x1161b0],
            &mut tiny,
            None
        )?,
        254
    );
    // Equal nonzero bytes followed by unmapped input must fault, preserving input.
    let mut short = [1];
    let before = n.bytes_allocated();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x1161b0],
            &mut short,
            None
        )
        .is_err()
    );
    assert_eq!(short, [1]);
    assert_eq!(n.bytes_allocated(), before);
    println!(
        "PASS: actual pinned strcoll_l tail and strcoll C/POSIX profile; unsignedbyte exactdifference/low32 return, prefixes/NUL/alias/highaddresses, mismatch-before-unmapped success and equal-unmapped fault, unchanged sources/errno/heap; no nonC collation claim"
    );
    Ok(())
}
