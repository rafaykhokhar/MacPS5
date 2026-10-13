//! Actual pinned wcscoll_l tail wrapper, current C/POSIX locale profile only.
//! Guest wchar_t is unsigned16 (pinned SDK __SCE__ x86/_types.h), not host wchar_t.
//! FreeBSD 11.4 wcscoll.c falls back to wcscmp in the C locale.
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
        _ => Err("wcscoll fixture suspended".into()),
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
    n.register_service(SERVICE, Service::Wcscoll)?;
    n.register_service(SERVICE + 16, Service::Setlocale)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26faf8;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing wcscoll GOT")?;
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
            BTreeSet::from([0x116220]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut errno = 91u32.to_le_bytes();
    let mut data = vec![0xa5; 256];
    // Both supported spellings select the owned C collation. The pinned wrapper
    // tail-calls plain wcscoll and does not dereference its third locale argument.
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
            (&[0u16][..], &[0u16][..], 0i32),
            (&[65, 0][..], &[97, 0][..], -32),
            (&[65, 0][..], &[65, 100, 0][..], -100),
            (&[0xffff, 0][..], &[1, 0][..], 65534),
            (&[1, 0][..], &[0xffff, 0][..], -65534),
            (&[0x8000, 0][..], &[0x7fff, 0][..], 1),
            (&[0xd800, 0][..], &[0xdc00, 0][..], -1024),
            (&[65, 0, 0xffff][..], &[65, 0, 1][..], 0),
        ] {
            let left: Vec<u8> = left.iter().flat_map(|x| x.to_le_bytes()).collect();
            let right: Vec<u8> = right.iter().flat_map(|x| x.to_le_bytes()).collect();
            data[..32].fill(0xa5);
            data[32..64].fill(0xa5);
            data[..left.len()].copy_from_slice(&left);
            data[32..32 + right.len()].copy_from_slice(&right);
            let before = data.clone();
            let allocated = n.bytes_allocated();
            for target in [SERVICE, BIAS + 0x116220] {
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
    // Exact same address is not a shortcut: it must scan readable whole units.
    data[..6].copy_from_slice(&[65, 0, 66, 0, 0, 0]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x116220],
            &mut data,
            None
        )?,
        0
    );
    // A differing first unit needs no terminator; unsigned16 produces full int.
    let mut tiny = [255, 255, 1, 0];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA + 2, 0, BIAS + 0x116220],
            &mut tiny,
            None
        )?,
        65534
    );
    // Terminator at the final complete word succeeds without an extra read.
    let mut empty = [0, 0];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x116220],
            &mut empty,
            None
        )?,
        0
    );
    // Odd byte spans cannot supply a wchar; equal unterminated units must fault.
    for mut input in [vec![0], vec![65, 0], vec![65, 0, 0]] {
        let before = input.clone();
        let allocated = n.bytes_allocated();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, DATA, 0, BIAS + 0x116220],
                &mut input,
                Some(&mut errno)
            )
            .is_err()
        );
        assert_eq!(input, before);
        assert_eq!(n.bytes_allocated(), allocated);
    }
    for bad in [0, u64::MAX] {
        let mut input = [0, 0];
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, bad, 0, BIAS + 0x116220],
                &mut input,
                Some(&mut errno)
            )
            .is_err()
        );
        assert_eq!(input, [0, 0]);
    }
    // The last allowed code unit is examined, but scanning beyond the explicit
    // prototype budget must fail even when a later terminator is mapped.
    let mut boundary = vec![1u8; 2 * 65_537];
    boundary[2 * 65_535..2 * 65_536].fill(0);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x116220],
            &mut boundary,
            None
        )?,
        0
    );
    boundary[2 * 65_535..2 * 65_536].fill(1);
    boundary[2 * 65_536..].fill(0);
    let before = boundary.clone();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x116220],
            &mut boundary,
            Some(&mut errno)
        )
        .is_err()
    );
    assert_eq!(boundary, before);
    assert_eq!(errno, 91u32.to_le_bytes());
    println!(
        "PASS: 32 native C/POSIX wcscoll/direct and actual pinned wcscoll_l comparisons; guest unsigned16 full32 signed difference, high/unaligned readonly pointers, NUL/surrogate code units, identical pointers, mismatch-before-unmapped success, partial-unit/unmapped/scan-limit guards, unchanged errno/heap"
    );
    Ok(())
}
