//! Actual pinned SDL integer conversion wrappers, not replacements for their code.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: &mut [u8],
    writable: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        entry,
        ARGS,
        &mut [r],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut descriptor,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable,
            },
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Integer wrapper did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Same adapter except endptr aliases the live CALL return frame.
    let frame = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8d, 0x74, 0x24, 0xf8, 0x48, 0x8b, 0x3f,
        0xff, 0xd0, 0xc3,
    ];
    f[0x140..0x140 + frame.len()].copy_from_slice(&frame);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    for (idx, (got, service)) in [
        (0x26f838, Service::Atoi),
        (0x26f848, Service::Strtol),
        (0x26f850, Service::Strtoul),
        (0x26f858, Service::Strtoll),
        (0x26f860, Service::Strtoull),
    ]
    .into_iter()
    .enumerate()
    {
        let target = 0x780000 + idx as u64 * 16;
        n.register_service(target, service)?;
        let address = BIAS + got;
        let s = real
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing numeric GOT")?;
        let off = (address - s.address) as usize;
        s.bytes[off..off + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1a6c0, 0x1a6e0, 0x1a6f0, 0x1a700, 0x1a710]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    for target in [0x1a6e0, 0x1a6f0, 0x1a700, 0x1a710] {
        let signed = target == 0x1a6e0 || target == 0x1a700;
        let cases: [(&[u8], i32, u64, u64, u32); 9] = [
            (b" \t-12!", 10, (-12i64) as u64, 5, 77),
            (b"0x", 0, 0, 1, 77),
            (b"0xg", 16, 0, 1, 77),
            (b"0Xf!", 0, 15, 3, 77),
            (b"09", 0, 0, 1, 77),
            (b"0b11", 0, 0, 1, 77),
            (b"Zz!", 36, 1295, 2, 77),
            (b" -!", 10, 0, 0, 22),
            (b"123", 1, 0, 0, 22),
        ];
        let mut cases = cases.to_vec();
        if signed {
            cases.extend([
                (&b"9223372036854775807!"[..], 10, i64::MAX as u64, 19, 77),
                (&b"-9223372036854775808!"[..], 10, i64::MIN as u64, 20, 77),
                (&b"9223372036854775808999!"[..], 10, i64::MAX as u64, 22, 34),
                (&b"-9223372036854775809!"[..], 10, i64::MIN as u64, 20, 34),
            ]);
        } else {
            cases.extend([
                (&b"18446744073709551615!"[..], 10, u64::MAX, 20, 77),
                (&b"18446744073709551616!"[..], 10, u64::MAX, 20, 34),
                (&b"-18446744073709551615!"[..], 10, 1, 21, 77),
                (&b"-18446744073709551616!"[..], 10, u64::MAX, 21, 34),
            ]);
        }
        for (s, base, bits, end, err) in cases {
            let mut data = [0xa5; 128];
            data[..s.len()].copy_from_slice(s);
            data[s.len()] = 0;
            let before = data;
            let mut errno = 77u32.to_le_bytes();
            // Upper base bits deliberately set; guest int uses only EDX.
            assert_eq!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [
                        DATA,
                        DATA + 96,
                        0x123400000000 | u64::from(base as u32),
                        BIAS + target
                    ],
                    &mut data,
                    &mut errno,
                    true,
                    0x100100
                )?,
                bits
            );
            assert_eq!(&data[96..104], &(DATA + end).to_le_bytes());
            assert_eq!(&data[..96], &before[..96]);
            assert_eq!(&data[104..], &before[104..]);
            assert_eq!(errno, err.to_le_bytes());
        }
    }
    for (s, expected, err) in [
        (&b"2147483648!"[..], 0x80000000, 77u32),
        (&b"4294967297!"[..], 1, 77),
        (&b"-1!"[..], u32::MAX as u64, 77),
        (&b"9223372036854775808!"[..], u32::MAX as u64, 34),
        (&b"!"[..], 0, 22),
    ] {
        let mut data = s.to_vec();
        let mut errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, u64::MAX, u64::MAX, BIAS + 0x1a6c0],
                &mut data,
                &mut errno,
                false,
                0x100100
            )?,
            expected
        );
        assert_eq!(errno, err.to_le_bytes());
        assert_eq!(data, s);
    }
    // Output aliases parsed input: parse completes before endptr write.
    let mut data = [0u8; 32];
    data[..4].copy_from_slice(b"123!");
    let mut errno = 77u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 10, BIAS + 0x1a6e0],
            &mut data,
            &mut errno,
            true,
            0x100100
        )?,
        123
    );
    assert_eq!(&data[..8], &(DATA + 3).to_le_bytes());
    // NULL output succeeds with readonly source; output failures cannot change errno.
    let mut data = *b"!xxxxxxx";
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 10, BIAS + 0x1a6e0],
            &mut data,
            &mut errno,
            false,
            0x100100
        )?,
        0
    );
    for (out, writable, entry) in [
        (DATA, false, 0x100100),
        (DATA + 1, true, 0x100100),
        (u64::MAX, true, 0x100100),
        (DATA, true, 0x100140),
    ] {
        errno = 77u32.to_le_bytes();
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, out, 10, BIAS + 0x1a6e0],
                &mut data,
                &mut errno,
                writable,
                entry
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // When errno and endptr alias, the reference's final endptr write wins.
    let mut shared = [0xa5; 8];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, ERRNO, 10, BIAS + 0x1a6e0],
            &mut data,
            &mut shared,
            false,
            0x100100
        )?,
        0
    );
    assert_eq!(shared, DATA.to_le_bytes());
    // Invalid errno range leaves an otherwise valid endptr untouched.
    errno = 77u32.to_le_bytes();
    let before = data;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 10, BIAS + 0x1a6e0],
            &mut data,
            &mut errno[..3],
            true,
            0x100100
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert_eq!(errno, 77u32.to_le_bytes());
    // Guarded input and bounded overflow scan fail before any output commits.
    for mut data in [vec![b'9'; 65536], vec![b'1']] {
        let before = data.clone();
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, DATA, 10, BIAS + 0x1a6e0],
                &mut data,
                &mut errno,
                true,
                0x100100
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    println!(
        "PASS: actual five SDL integer wrappers; LP64 signed/unsigned bounds, atoi low32, qualified prefixes, errno/endptr, overflow fullscan, full64 pointers, base low32, alias ordering and atomic guarded outputs."
    );
    Ok(())
}
