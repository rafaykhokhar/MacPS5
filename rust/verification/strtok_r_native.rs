//! Native bounded strtok_r and actual SDL_strtokr_REAL forwarding wrapper.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const S: u64 = 0x1_8d000001;
const D: u64 = 0x2_8e000003;
const P: u64 = 0x3_8f000005;
const TABLE: u64 = 0x90000000;
const SERVICE: u64 = 0x910000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    source: &mut [u8],
    source_write: bool,
    delim: &mut [u8],
    last: &mut [u8],
    last_write: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let before = delim.to_vec();
    let allocated = n.bytes_allocated();
    let mut regions = vec![DataRegion {
        address: TABLE,
        bytes: &mut table,
        writable: false,
    }];
    if !source.is_empty() {
        regions.push(DataRegion {
            address: S,
            bytes: source,
            writable: source_write,
        });
    }
    if !delim.is_empty() {
        regions.push(DataRegion {
            address: D,
            bytes: delim,
            writable: false,
        });
    }
    if !last.is_empty() {
        regions.push(DataRegion {
            address: P,
            bytes: last,
            writable: last_write,
        });
    }
    let result = n.run_worker_with_data(a, entry, TABLE, &mut [r], &mut regions);
    drop(regions);
    assert_eq!(delim, before);
    assert_eq!(n.bytes_allocated(), allocated);
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("strtok_r fixture suspended".into()),
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
            0x48, 0x83, 0xec, 8,
        ];
        if frame {
            code.extend([0x48, 0x8d, 0x54, 0x24, 0xf8]);
        }
        code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3]);
        fixture[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.register_service(SERVICE, Service::StrtokR)?;
    let mut a = Image::load(&fixture, 0x100000, 65536)?;
    let mut r = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb20;
    let segment = r
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strtok_r GOT")?;
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
            BTreeSet::from([0x1166e0]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    for target in [SERVICE, BIAS + 0x1166e0] {
        let mut source = b",,alpha::beta,gamma\0".to_vec();
        let original = source.clone();
        let mut delim = b",:\0".to_vec();
        let mut last = u64::MAX.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut r,
                [S, D, P, target],
                &mut source,
                true,
                &mut delim,
                &mut last,
                true,
                0x100100
            )?,
            S + 2
        );
        assert_eq!(u64::from_le_bytes(last), S + 8);
        let mut expected = original;
        expected[7] = 0;
        assert_eq!(source, expected);
        // Changed delimiter set in continuation consumes no comma inside final token.
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut r,
                [0, D + 1, P, target],
                &mut source,
                false,
                &mut delim,
                &mut last,
                true,
                0x100100
            )?,
            S + 9
        );
        assert_eq!(last, [0; 8]);
        assert_eq!(source, expected);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut r,
                [0, u64::MAX, P, target],
                &mut [],
                false,
                &mut [],
                &mut last,
                false,
                0x100100
            )?,
            0
        );
        for (input, delimiters, token, cut, next) in [
            (vec![0], vec![0], 0, None, 0),
            (vec![255, 128, 0], vec![0], S, None, 0),
            (vec![255, 255, 0], vec![255, 0], 0, None, 0),
            (vec![128, 255, 7], vec![255, 0], S, Some(1), S + 2),
        ] {
            let mut source = input.clone();
            let mut delim = delimiters;
            let mut last = [0xa5; 8];
            assert_eq!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut r,
                    [S, D, P, target],
                    &mut source,
                    true,
                    &mut delim,
                    &mut last,
                    true,
                    0x100100
                )?,
                token
            );
            assert_eq!(u64::from_le_bytes(last), next);
            let mut expected = input;
            if let Some(at) = cut {
                expected[at] = 0;
            }
            assert_eq!(source, expected);
        }
        // Two independent caller-owned continuation slots do not share hidden state.
        for initial in [S + 2, S] {
            let mut source = b"a,b\0".to_vec();
            let mut delim = b",\0".to_vec();
            let mut last = initial.to_le_bytes();
            let returned = invoke(
                &mut n,
                &mut a,
                &mut r,
                [0, D, P, target],
                &mut source,
                true,
                &mut delim,
                &mut last,
                true,
                0x100100,
            )?;
            assert_eq!(returned, initial);
            assert_eq!(
                u64::from_le_bytes(last),
                if initial == S { S + 2 } else { 0 }
            );
        }
        // All planned outputs must be valid before the first separator byte changes.
        for (source_write, length, last_write, slot, entry) in [
            (false, 8, true, P, 0x100100),
            (true, 7, true, P, 0x100100),
            (true, 8, false, P, 0x100100),
            (true, 0, true, P, 0x100100),
            (true, 8, true, S, 0x100100),
            (true, 8, true, D, 0x100100),
            (true, 8, true, u64::MAX, 0x100100),
            (true, 8, true, P, 0x100200),
        ] {
            let mut source = b"a,b\0".to_vec();
            let mut delim = b",\0".to_vec();
            let mut last = vec![0xa5; length];
            assert!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut r,
                    [S, D, slot, target],
                    &mut source,
                    source_write,
                    &mut delim,
                    &mut last,
                    last_write,
                    entry
                )
                .is_err()
            );
            assert_eq!(source, b"a,b\0");
            assert_eq!(last, vec![0xa5; length]);
        }
        for (input, delimiter, source) in [
            (u64::MAX, D, vec![b'a', 0]),
            (S, u64::MAX, vec![b'a', 0]),
            (S, D, vec![b'a']),
        ] {
            let mut source = source;
            let before = source.clone();
            let mut delim = b",\0".to_vec();
            let mut last = [0xa5; 8];
            assert!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut r,
                    [input, delimiter, P, target],
                    &mut source,
                    true,
                    &mut delim,
                    &mut last,
                    true,
                    0x100100
                )
                .is_err()
            );
            assert_eq!(source, before);
            assert_eq!(last, [0xa5; 8]);
        }
        let mut source = vec![b'x'; 65536];
        let mut delim = [0];
        let mut last = [0xa5; 8];
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut r,
                [S, D, P, target],
                &mut source,
                true,
                &mut delim,
                &mut last,
                true,
                0x100100
            )
            .is_err()
        );
        assert!(source.iter().all(|b| *b == b'x'));
        assert_eq!(last, [0xa5; 8]);
    }
    println!(
        "PASS: native strtok_r direct/actualSDLwrapper, full64 token/save pointers, repeated/changed delimiters, highbytes/emptysets, sourceNUL savesNULL, read-only exhaustion shortcut, exactseparator+8Bslot writes and transactional source/delimiter/slot/frame/alias/bound guards; no errno/kernel needed"
    );
    Ok(())
}
