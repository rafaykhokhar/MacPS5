//! Actual SDL atof/strtod wrappers; adapter snapshots architectural return state.
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
fn controls(rc: u32) -> [u8; 192] {
    let mut s = [0xa5; 192];
    s[..2].copy_from_slice(&(0x037fu16 | ((rc as u16) << 10)).to_le_bytes());
    s[4..8].copy_from_slice(&(0x1f81u32 | (rc << 13)).to_le_bytes());
    s[32..64].fill(0x5a);
    s[64..96].fill(0x69);
    s
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    for (entry, frame) in [(0x100, false), (0x200, true)] {
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
        c.extend([
            0x41, 0xff, 0xd3, 0xc5, 0xfe, 0x7f, 0x43, 96, 0xc5, 0xfe, 0x7f, 0x8b, 128, 0, 0, 0,
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
    for (got, service, target) in [
        (0x26f840, Service::Atof, 0x790000u64),
        (0x26f868, Service::Strtod, 0x790010),
    ] {
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
            BTreeSet::from([0x1a6d0, 0x1a720]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    // Constants reflect audited gdtoa fast paths: decimal0.1 sets PE; integer-only
    // hex construction can be inexact without raising an SSE arithmetic flag.
    for target in [0x1a6d0, 0x1a720] {
        for rc in 0..4u32 {
            let cases: [(&[u8], u64, u64, u32, u32); 6] = [
                (b"1.5!", 0x3ff8000000000000, 3, 0, 77),
                (
                    b"0.1!",
                    if rc == 1 || rc == 3 {
                        0x3fb9999999999999
                    } else {
                        0x3fb999999999999a
                    },
                    3,
                    0x20,
                    77,
                ),
                (b"-0", 0x8000000000000000, 2, 0, 77),
                (b"inf!", 0x7ff0000000000000, 3, 0, 77),
                (b"no", 0, 0, 0, 77),
                (b"0x1p-1075!", if rc == 2 { 1 } else { 0 }, 9, 0, 34),
            ];
            for (text, bits, end, flags, err) in cases {
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
                        [DATA, DATA + 96, 0, BIAS + target],
                        &mut data,
                        &mut errno,
                        &mut state,
                        true,
                        0x100100
                    )?,
                    SENTINEL
                );
                assert_eq!(&state[96..104], &bits.to_le_bytes(), "{text:?} rc{rc}");
                assert_eq!(&state[104..128], &[0x5a; 24]);
                assert_eq!(&state[128..160], &[0x69; 32]);
                assert_eq!(&state[160..162], &state[..2]);
                assert_eq!(
                    &state[164..168],
                    &(0x1f81 | (rc << 13) | flags).to_le_bytes()
                );
                assert_eq!(&state[168..176], &SENTINEL.to_le_bytes());
                assert_eq!(&state[176..184], &(!SENTINEL).to_le_bytes());
                assert_eq!(errno, err.to_le_bytes());
                assert_eq!(&data[..96], &before[..96]);
                if target == 0x1a720 {
                    assert_eq!(&data[96..104], &(DATA + end).to_le_bytes());
                } else {
                    assert_eq!(data, before);
                }
            }
        }
    }
    for space in [9, 10, 11, 12, 13, 32] {
        let mut data = [space, b'0', b'.', b'1', 0];
        let mut state = controls(0);
        let mut errno = 77u32.to_le_bytes();
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 0, BIAS + 0x1a720],
            &mut data,
            &mut errno,
            &mut state,
            false,
            0x100100,
        )?;
        assert_eq!(&state[96..104], &0x3fb999999999999au64.to_le_bytes());
        assert_eq!(&state[164..168], &0x1fa1u32.to_le_bytes());
    }
    // Source/output alias: snapshot input before overwriting its first eight bytes.
    let mut data = [0u8; 128];
    data[..4].copy_from_slice(b"1.5\0");
    let mut state = controls(0);
    let mut errno = 77u32.to_le_bytes();
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, DATA, 0, BIAS + 0x1a720],
        &mut data,
        &mut errno,
        &mut state,
        true,
        0x100100,
    )?;
    assert_eq!(&data[..8], &(DATA + 3).to_le_bytes());
    // NULL endptr with readonly input; atof ignores RSI altogether.
    for (target, out) in [(0x1a720, 0), (0x1a6d0, u64::MAX)] {
        let mut data = *b"1.5\0";
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, out, 0, BIAS + target],
            &mut data,
            &mut errno,
            &mut controls(0),
            false,
            0x100100,
        )?;
    }
    // Both outputs must be guarded before error, including the live return frame.
    let mut data = *b"0x1p-1075\0xxxxxx";
    for (out, writable, entry) in [
        (DATA, false, 0x100100),
        (DATA + 9, true, 0x100100),
        (u64::MAX, true, 0x100100),
        (DATA, true, 0x100200),
    ] {
        let before = data;
        errno = 77u32.to_le_bytes();
        let mut state = controls(0);
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, out, 0, BIAS + 0x1a720],
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
        assert_eq!(&state[96..], &[0xa5; 96]);
    }
    let before = data;
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x1a720],
            &mut data,
            &mut errno[..3],
            &mut controls(0),
            true,
            0x100100
        )
        .is_err()
    );
    assert_eq!(data, before);
    let mut shared = [0xa5; 8];
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, ERRNO, 0, BIAS + 0x1a720],
        &mut data,
        &mut shared,
        &mut controls(0),
        false,
        0x100100,
    )?;
    assert_eq!(shared, (DATA + 9).to_le_bytes());
    for text in [
        b"1e-400\0".as_slice(),
        b"1234567890123456\0",
        b"1e37\0",
        b"1e-23\0",
        b"1",
    ] {
        let mut data = text.to_vec();
        let before = data.clone();
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, DATA, 0, BIAS + 0x1a720],
                &mut data,
                &mut errno,
                &mut controls(0),
                true,
                0x100100
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    for (mxcsr, cw) in [
        (0x1fc0u32, 0x037fu16),
        (0x9f80, 0x037f),
        (0x0f80, 0x037f),
        (0x1f80, 0x077f),
        (0x1f80, 0x037e),
        (0x1f80, 0x013f),
    ] {
        let mut data = *b"0.1\0xxxxxxxx";
        let before = data;
        let mut state = controls(0);
        state[..2].copy_from_slice(&cw.to_le_bytes());
        state[4..8].copy_from_slice(&mxcsr.to_le_bytes());
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, DATA, 0, BIAS + 0x1a720],
                &mut data,
                &mut errno,
                &mut state,
                true,
                0x100100
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    println!(
        "PASS: actual SDL atof/strtod binary64 XMM0 return, upperYMM/otherlane/RAX/R8 preservation;4 rounding modes, audited decimalPE vs integerhex flags, stickyMXCSR/CW, endptr/errno aliases and guards; unsupported controls/refinement reject. Existing x87SW is not guest-seeded by this fixture."
    );
    Ok(())
}
