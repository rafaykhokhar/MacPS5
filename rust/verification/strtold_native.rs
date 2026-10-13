//! Actual pinned strtold_l wrapper; adapter snapshots architectural return state.
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
const STATE: u64 = 0x34000000;
const SENTINEL: u64 = 0x123456789abcdef0;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: &mut [u8],
    state: &mut [u8],
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
            DataRegion {
                address: STATE,
                bytes: state,
                writable: true,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Float wrapper did not return".into()),
    }
}
fn controls(rc: u32) -> [u8; 384] {
    let mut s = [0xa5; 384];
    s[..2].copy_from_slice(&(0x037fu16 | ((rc as u16) << 10)).to_le_bytes());
    s[4..8].copy_from_slice(&(0x1f81u32 | (rc << 13)).to_le_bytes());
    s[32..64].fill(0x5a);
    s[64..96].fill(0x69);
    s
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x1000u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x1000u64.to_le_bytes());
    f.resize(0x1000, 0);
    f[56..58].copy_from_slice(&1u16.to_le_bytes()); // Code-only adapter.
    for (entry, frame, repeats) in [(0x100, false, 1), (0x300, true, 1), (0x500, false, 12)] {
        let mut c = vec![0x48, 0xbb];
        c.extend(STATE.to_le_bytes());
        c.extend([
            0xd9, 0x2b, 0x0f, 0xae, 0x53, 4, 0xc5, 0xfe, 0x6f, 0x43, 32, 0xc5, 0xfe, 0x6f, 0x4b, 64,
        ]);
        c.extend([0x4c, 0x8b, 0x5f, 24, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f]);
        if frame {
            c.extend([0x48, 0x8d, 0x74, 0x24, 0xf8]);
        }
        c.extend([0x48, 0xb8]);
        c.extend(SENTINEL.to_le_bytes());
        c.extend([0x49, 0xb8]);
        c.extend((!SENTINEL).to_le_bytes());
        for index in 0..repeats {
            c.extend([0x41, 0xff, 0xd3, 0xdb, 0xbb]); // CALL r11; FSTP tbyte [rbx+offset]
            c.extend((192u32 + 10 * index).to_le_bytes());
        }
        c.extend([
            0xc5, 0xfe, 0x7f, 0x43, 96, 0xc5, 0xfe, 0x7f, 0x8b, 128, 0, 0, 0,
        ]);
        c.extend([
            0xd9, 0xbb, 160, 0, 0, 0, 0x0f, 0xae, 0x9b, 164, 0, 0, 0, 0x48, 0x89, 0x83, 168, 0, 0,
            0, 0x4c, 0x89, 0x83, 176, 0, 0, 0, 0xc3,
        ]);
        f[entry..entry + c.len()].copy_from_slice(&c);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    for (got, service, target) in [(0x26fae8, Service::Strtold, 0x790000u64)] {
        n.register_service(target, service)?;
        let address = BIAS + got;
        let s = real
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing float GOT")?;
        let off = (address - s.address) as usize;
        s.bytes[off..off + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1161f0]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x300, 0x500]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    // Raw constants sampled from unmodified FreeBSD 11.4 strtorx/strtodg,
    // compiled for x86_64 and executed through Rosetta. This is source-policy
    // evidence, not a measurement of proprietary PS5 libc or physical x86.
    // Retained harness output: work/strtold-native/source-constants.log.
    for target in [0x790000, BIAS + 0x1161f0] {
        for rc in 0..4 {
            let cases: [(&[u8], u64, u16, usize, u32, u32); 12] = [
                (b"1.5!", 0xc000000000000000, 0x3fff, 3, 0, 77),
                (
                    b"0.1!",
                    if rc == 1 || rc == 3 {
                        0xcccccccccccccccc
                    } else {
                        0xcccccccccccccccd
                    },
                    0x3ffb,
                    3,
                    32,
                    77,
                ),
                (b"-0", 0, 0x8000, 2, 0, 77),
                (b"inf!", 1 << 63, 0x7fff, 3, 0, 77),
                (b"no", 0, 0, 0, 0, 77),
                (b"nan(0x123456789)", 0xc000000123456789, 0x7fff, 16, 0, 77),
                (b"0x1p-16446", if rc == 2 { 1 } else { 0 }, 0, 10, 0, 34),
                (b"0x1p-16445", 1, 0, 10, 0, 77),
                (b"0x1p16384", 1 << 63, 0x7fff, 9, 0, 34),
                (b"9007199254740993", 0x8000000000000400, 0x4034, 16, 32, 77),
                // Source's directed adjustment carries an exactly representable
                // integer under upward RC; preserve the measured source quirk.
                (
                    b"18446744073709551615",
                    if rc == 2 { 1 << 63 } else { u64::MAX },
                    if rc == 2 { 0x403f } else { 0x403e },
                    20,
                    32,
                    77,
                ),
                (
                    b"0x1.0000000000000002p0",
                    0x8000000000000001,
                    0x3fff,
                    22,
                    0,
                    77,
                ),
            ];
            for (text, sig, exp, end, flags, error) in cases {
                let mut data = [0xa5; 128];
                data[..text.len()].copy_from_slice(text);
                data[text.len()] = 0;
                let before = data;
                let mut state = controls(rc);
                let mut errno = 77u32.to_le_bytes();
                assert_eq!(
                    invoke(
                        &mut n,
                        &mut adapter,
                        &mut real,
                        [DATA, DATA + 96, 0, target],
                        &mut data,
                        &mut errno,
                        &mut state,
                        true,
                        0x100100
                    )?,
                    SENTINEL
                );
                assert_eq!(&state[192..200], &sig.to_le_bytes(), "{text:?} rc{rc}");
                assert_eq!(&state[200..202], &exp.to_le_bytes(), "{text:?} rc{rc}");
                // Upper lanes are a deterministic runtime preservation policy,
                // not an extra promise made by the C ABI for caller-saved XMM0.
                assert_eq!(&state[96..128], &[0x5a; 32]);
                assert_eq!(&state[128..160], &[0x69; 32]);
                assert_eq!(&state[160..162], &state[..2]);
                assert_eq!(
                    &state[164..168],
                    &(0x1f81 | (rc << 13) | flags).to_le_bytes()
                );
                assert_eq!(&state[168..176], &SENTINEL.to_le_bytes());
                assert_eq!(&state[176..184], &(!SENTINEL).to_le_bytes());
                assert_eq!(errno, error.to_le_bytes());
                assert_eq!(&data[..96], &before[..96]);
                assert_eq!(&data[96..104], &(DATA + end as u64).to_le_bytes());
            }
        }
    }
    for space in [9, 10, 11, 12, 13, 32] {
        let mut data = [space, b'1', b'.', b'5', 0];
        let mut state = controls(0);
        let mut errno = 77u32.to_le_bytes();
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 0, BIAS + 0x1161f0],
            &mut data,
            &mut errno,
            &mut state,
            false,
            0x100100,
        )?;
        assert_eq!(&state[192..200], &0xc000000000000000u64.to_le_bytes());
        assert_eq!(&state[200..202], &0x3fffu16.to_le_bytes());
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // Snapshot then publish aliased endptr, including error/endptr alias ordering.
    let mut data = [0u8; 128];
    data[..4].copy_from_slice(b"1.5\0");
    let mut errno = 77u32.to_le_bytes();
    let mut state = controls(0);
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, DATA, 0, BIAS + 0x1161f0],
        &mut data,
        &mut errno,
        &mut state,
        true,
        0x100100,
    )?;
    assert_eq!(&data[..8], &(DATA + 3).to_le_bytes());
    assert_eq!(&state[192..200], &0xc000000000000000u64.to_le_bytes());
    assert_eq!(&state[200..202], &0x3fffu16.to_le_bytes());
    let mut data = *b"0x1p16384\0xxxx";
    let mut shared = [0xa5; 8];
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, ERRNO, 0, BIAS + 0x1161f0],
        &mut data,
        &mut shared,
        &mut controls(0),
        false,
        0x100100,
    )?;
    assert_eq!(shared, (DATA + 9).to_le_bytes());
    // Failure guards: readonly/crossing/unmapped output, protected return slot,
    // undersized errno and unterminated input cannot publish partial state.
    for (out, writable, entry) in [
        (DATA, false, 0x100100),
        (DATA + 10, true, 0x100100),
        (u64::MAX, true, 0x100100),
        (DATA, true, 0x100300),
    ] {
        let before = data;
        let mut state = controls(0);
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, out, 0, BIAS + 0x1161f0],
                &mut data,
                &mut errno,
                &mut state,
                writable,
                entry
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
        assert_eq!(&state[96..], &[0xa5; 288]);
    }
    let before = data;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x1161f0],
            &mut data,
            &mut errno[..3],
            &mut controls(0),
            true,
            0x100100
        )
        .is_err()
    );
    assert_eq!(data, before);
    let mut data = *b"123";
    let before = data;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 0, BIAS + 0x1161f0],
            &mut data,
            &mut errno,
            &mut controls(0),
            false,
            0x100100
        )
        .is_err()
    );
    assert_eq!(data, before);
    for control in [0x1fc0u32, 0x9f80, 0x0f80, 0x3f80] {
        let mut data = *b"0.1\0";
        let mut state = controls(0);
        state[4..8].copy_from_slice(&control.to_le_bytes());
        let before = data;
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, 0, 0, BIAS + 0x1161f0],
                &mut data,
                &mut errno,
                &mut state,
                false,
                0x100100
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
        assert_eq!(&state[96..], &[0xa5; 288]);
    }
    // Twelve returns consumed within one translated caller exceed the eight
    // physical x87 slots; a missing pop or wrong TOP fails the payload sequence.
    for rc in 0..4 {
        let mut data = *b"0x1.0000000000000002p0\0";
        let mut state = controls(rc);
        let mut errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, 0, 0, BIAS + 0x1161f0],
                &mut data,
                &mut errno,
                &mut state,
                false,
                0x100500
            )?,
            SENTINEL
        );
        for value in state[192..312].chunks_exact(10) {
            assert_eq!(&value[..8], &0x8000000000000001u64.to_le_bytes());
            assert_eq!(&value[8..], &0x3fffu16.to_le_bytes());
        }
        assert_eq!(&state[96..128], &[0x5a; 32]);
        assert_eq!(&state[168..176], &SENTINEL.to_le_bytes());
        assert_eq!(errno, 77u32.to_le_bytes());
        assert_eq!(&state[164..168], &(0x1f81u32 | (rc << 13)).to_le_bytes());
    }
    println!(
        "PASS: actual strtold_l/direct ST(0) raw80 return consumed by native FSTP m80, 96 source-constant cases across four RCs, exact values beyond binary64, 12 returns in one caller, XMM0/RAX preservation, end/errno aliases and output/frame/errno/input/control guards"
    );
    Ok(())
}
