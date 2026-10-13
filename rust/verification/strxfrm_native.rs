//! Actual pinned strxfrm_l tail wrapper, C/POSIX collation only.
//! FreeBSD 11.4 strxfrm.c's C fallback calls strlcpy: truncation still
//! terminates, and the return value excludes NUL and reports full source length.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_77000001;
const OUT: u64 = 0x1_78000001;
const ARGS: u64 = 0x79000000;
const ERRNO: u64 = 0x7a000000;
const SERVICE: u64 = 0x860000;
const BIAS: u64 = 0x10000000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    output: &mut [u8],
    output_writable: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut errno = 91u32.to_le_bytes();
    let mut regions = vec![
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
        DataRegion {
            address: ERRNO,
            bytes: &mut errno,
            writable: false,
        },
    ];
    if !output.is_empty() {
        regions.push(DataRegion {
            address: OUT,
            bytes: output,
            writable: output_writable,
        });
    }
    let result = n.run_worker_with_data(a, entry, ARGS, &mut [real], &mut regions);
    drop(regions);
    assert_eq!(errno, 91u32.to_le_bytes());
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("strxfrm fixture suspended".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    fixture[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    fixture[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    for (entry, frame) in [(0x100, false), (0x200, true)] {
        let mut code = vec![
            0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f,
            0x48, 0xb9,
        ];
        code.extend(0xdeadbeefu64.to_le_bytes()); // Wrapper discards locale by tail-calling plain strxfrm.
        code.extend([0x48, 0x83, 0xec, 8]);
        if frame {
            code.extend([0x48, 0x8d, 0x7c, 0x24, 0xf8]);
        }
        code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3]);
        fixture[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Strxfrm)?;
    n.register_service(SERVICE + 16, Service::Setlocale)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26faf0;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strxfrm GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SERVICE.to_le_bytes());
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
            BTreeSet::from([0x116210]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut count = 0;
    for locale in [&b"C\0"[..], &b"POSIX\0"[..]] {
        let mut name = locale.to_vec();
        assert_ne!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [1, DATA, 0, SERVICE + 16],
                &mut name,
                false,
                &mut [],
                false,
                0x100100
            )?,
            0
        );
        for source in [
            &b"\0"[..],
            &b"a\0"[..],
            &b"abcdef\0"[..],
            &b"\x80\xff\x01\0"[..],
            &b"abc\0ignored"[..],
        ] {
            let length = source.iter().position(|&b| b == 0).unwrap();
            for capacity in [0, 1, 2, length as u64, length as u64 + 1, 24, u64::MAX] {
                for target in [SERVICE, BIAS + 0x116210] {
                    let mut input = source.to_vec();
                    let before = input.clone();
                    let mut output = [0xa5; 32];
                    let allocated = n.bytes_allocated();
                    assert_eq!(
                        invoke(
                            &mut n,
                            &mut adapter,
                            &mut real,
                            [OUT + 1, DATA, capacity, target],
                            &mut input,
                            false,
                            &mut output,
                            true,
                            0x100100
                        )?,
                        length as u64
                    );
                    let mut expected = [0xa5; 32];
                    if capacity != 0 {
                        let copied = length.min((capacity - 1).min(31) as usize);
                        expected[1..1 + copied].copy_from_slice(&source[..copied]);
                        expected[1 + copied] = 0;
                    }
                    assert_eq!(output, expected);
                    assert_eq!(input, before);
                    assert_eq!(n.bytes_allocated(), allocated);
                    count += 1;
                }
            }
        }
    }
    // n=0 ignores destination, including NULL, overflow and source aliases.
    for destination in [0, u64::MAX, DATA] {
        let mut input = *b"abc\0";
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [destination, DATA, 0, BIAS + 0x116210],
                &mut input,
                false,
                &mut [],
                false,
                0x100100
            )?,
            3
        );
    }
    // Validate only bytes actually written, not the declared capacity.
    let mut input = *b"a\0";
    let mut exact = [0xa5; 2];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [OUT, DATA, u64::MAX, BIAS + 0x116210],
            &mut input,
            false,
            &mut exact,
            true,
            0x100100
        )?,
        1
    );
    assert_eq!(exact, *b"a\0");
    // Failed guards leave both source and output intact. The target's restrict
    // source/destination contract is an explicit overlap rejection policy.
    for (destination, writable, capacity, entry) in [
        (OUT, false, 4, 0x100100),
        (OUT + 7, true, 4, 0x100100),
        (0, true, 4, 0x100100),
        (u64::MAX, true, 4, 0x100100),
        (DATA, true, 4, 0x100100),
        (DATA + 1, true, 4, 0x100100),
        (OUT, true, 4, 0x100200),
    ] {
        let mut input = *b"abc\0xxxx";
        let mut output = [0xa5; 8];
        let before = input;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [destination, DATA, capacity, BIAS + 0x116210],
                &mut input,
                true,
                &mut output,
                writable,
                entry
            )
            .is_err()
        );
        assert_eq!(input, before);
        assert_eq!(output, [0xa5; 8]);
    }
    // Full source scan remains necessary when output is absent or truncated.
    for capacity in [0, 1, 2, 16] {
        let mut input = *b"abc";
        let mut output = [0xa5; 8];
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [OUT, DATA, capacity, BIAS + 0x116210],
                &mut input,
                false,
                &mut output,
                true,
                0x100100
            )
            .is_err()
        );
        assert_eq!(input, *b"abc");
        assert_eq!(output, [0xa5; 8]);
    }
    println!(
        "PASS: {count} native strxfrm/direct and actual pinned strxfrm_l C/POSIX cases; full length, truncated NUL, high bytes, n=0 invalid destination, exact output spans, readonly/unmapped/crossing/frame/overlap/source guards, unchanged errno/heap"
    );
    Ok(())
}
