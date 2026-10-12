//! Actual SDL C-locale import wrappers. Expected sets are explicit test data.
use native_ps5::{
    c_locale::Ctype,
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        0x100100,
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
                writable: false,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("C-locale wrapper did not return".into()),
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
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    let operations = [
        (0x1ae10, 0x26f898, Ctype::Isalpha),
        (0x1ae20, 0x26f8a0, Ctype::Isalnum),
        (0x1ae40, 0x26f8a8, Ctype::Isxdigit),
        (0x1ae50, 0x26f8b0, Ctype::Ispunct),
        (0x1ae60, 0x26f8b8, Ctype::Isspace),
        (0x1ae70, 0x26f8c0, Ctype::Isupper),
        (0x1ae80, 0x26f8c8, Ctype::Islower),
        (0x1ae90, 0x26f8d0, Ctype::Isprint),
        (0x1aea0, 0x26f8d8, Ctype::Isgraph),
        (0x1aeb0, 0x26f8e0, Ctype::Iscntrl),
        (0x1aec0, 0x26f8e8, Ctype::Toupper),
        (0x1aed0, 0x26f8f0, Ctype::Tolower),
        (0x1af00, 0x26f900, Ctype::Isblank),
    ];
    let mut entries = BTreeSet::from([0x1a760]);
    for (index, (entry, got, service)) in operations
        .iter()
        .map(|(e, g, o)| (*e, *g, Service::Ctype(*o)))
        .chain([(0x1a760, 0x26f880, Service::Strncasecmp)])
        .enumerate()
    {
        entries.insert(entry);
        let target = 0x790000 + index as u64 * 16;
        n.register_service(target, service)?;
        let address = BIAS + got;
        let s = real
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing ctype GOT")?;
        let offset = (address - s.address) as usize;
        s.bytes[offset..offset + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        n.compile_image(file, BIAS, entries, 1000, archive, &directory.join("real"))?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let digits = b"0123456789";
    let punct = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
    let mut data = [0u8; 16];
    for (entry, _, op) in operations {
        for c in -1i32..=255 {
            let has = |s: &[u8]| c >= 0 && s.contains(&(c as u8));
            let expected = match op {
                Ctype::Isalpha => i32::from(has(alpha)),
                Ctype::Isalnum => i32::from(has(alpha) || has(digits)),
                Ctype::Isxdigit => i32::from(has(b"0123456789ABCDEFabcdef")),
                Ctype::Ispunct => i32::from(has(punct)),
                Ctype::Isspace => i32::from(has(b"\t\n\x0b\x0c\r ")),
                Ctype::Isupper => i32::from(has(&alpha[..26])),
                Ctype::Islower => i32::from(has(&alpha[26..])),
                Ctype::Isprint => i32::from(has(alpha) || has(digits) || has(punct) || c == 32),
                Ctype::Isgraph => i32::from(has(alpha) || has(digits) || has(punct)),
                Ctype::Iscntrl => i32::from((0..32).chain([127]).any(|v| v == c)),
                Ctype::Isblank => i32::from(has(b"\t ")),
                Ctype::Toupper => alpha
                    .iter()
                    .position(|v| i32::from(*v) == c)
                    .map_or(c, |i| i32::from(alpha[i % 26])),
                Ctype::Tolower => alpha
                    .iter()
                    .position(|v| i32::from(*v) == c)
                    .map_or(c, |i| i32::from(alpha[26 + i % 26])),
            };
            assert_eq!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [0x123400000000 | u64::from(c as u32), 0, 0, BIAS + entry],
                    &mut data
                )?,
                expected as u32 as u64,
                "{op:?} {c}"
            );
        }
        for c in [-2i32, 256, i32::MIN, i32::MAX] {
            assert!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [c as u32 as u64, 0, 0, BIAS + entry],
                    &mut data
                )
                .is_err()
            );
        }
    }
    data[..10].copy_from_slice(b"Ab\0aB\0\xff\0z\0");
    let before = data;
    for (left, right, count, result) in [
        (DATA, DATA + 3, u64::MAX, 0),
        (DATA, DATA + 8, 1, -25),
        (DATA + 6, DATA + 8, 1, 133),
        (DATA + 8, DATA + 6, 1, -133),
        (0, u64::MAX, 0, 0),
        (DATA + 6, DATA + 6, 1u64 << 32, 0),
    ] {
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [left, right, count, BIAS + 0x1a760],
                &mut data
            )?,
            result as i32 as u32 as u64
        );
        assert_eq!(data, before);
    }
    let mut short = [b'a'];
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 2, BIAS + 0x1a760],
            &mut short
        )
        .is_err()
    );
    let mut long = vec![b'a'; 65536];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 65536, BIAS + 0x1a760],
            &mut long
        )?,
        0
    );
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 1u64 << 32, BIAS + 0x1a760],
            &mut long
        )
        .is_err()
    );
    assert!(long.iter().all(|b| *b == b'a'));
    println!(
        "PASS: actual13 SDL ctype wrappers across256 bytes+EOF, low32 ABI and explicit invaliddomain; actual strncasecmp unsignedfold/sign/full64count/earlyNUL/zero/guard/budget/unchanged sources."
    );
    Ok(())
}
