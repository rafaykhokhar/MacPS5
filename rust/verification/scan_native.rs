//! Actual SDL_vsscanf tailwrapper with guest SysV va_list and guarded writes.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{memory_fixture, Execution, NativeExecutor, Service},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
const VA: u64 = 0x34000000;
const SAVE: u64 = 0x35000000;
const STACK: u64 = 0x36000000;
struct Fixture {
    data: Vec<u8>,
    va: [u8; 24],
    save: [u8; 48],
    stack: [u8; 128],
    errno: [u8; 4],
}
impl Fixture {
    fn new(input: &[u8], format: &[u8], gp: u32, pointers: &[u64]) -> Self {
        let mut f = Self {
            data: vec![0xa5; 1024],
            va: [0; 24],
            save: [0; 48],
            stack: [0; 128],
            errno: 77u32.to_le_bytes(),
        };
        f.data[..input.len()].copy_from_slice(input);
        f.data[input.len()] = 0;
        f.data[128..128 + format.len()].copy_from_slice(format);
        f.data[128 + format.len()] = 0;
        f.data[800..802].copy_from_slice(&0x037fu16.to_le_bytes());
        f.data[804..808].copy_from_slice(&0x1f80u32.to_le_bytes());
        f.va[..4].copy_from_slice(&gp.to_le_bytes());
        f.va[4..8].copy_from_slice(&48u32.to_le_bytes());
        f.va[8..16].copy_from_slice(&STACK.to_le_bytes());
        f.va[16..].copy_from_slice(&SAVE.to_le_bytes());
        let mut offset = gp as usize;
        let mut stack = 0;
        for ptr in pointers {
            if offset < 48 {
                f.save[offset..offset + 8].copy_from_slice(&ptr.to_le_bytes());
                offset += 8;
            } else {
                f.stack[stack..stack + 8].copy_from_slice(&ptr.to_le_bytes());
                stack += 8;
            }
        }
        f
    }
}
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    f: &mut Fixture,
    va_write: bool,
    data_write: bool,
    errno_len: usize,
    entry: u64,
) -> Result<u64, String> {
    let mut args: Vec<u8> = [DATA, DATA + 128, VA, BIAS + 0x1a800]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    match n.run_worker_with_data(
        a,
        entry,
        ARGS,
        &mut [r],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut args,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: &mut f.data,
                writable: data_write,
            },
            DataRegion {
                address: VA,
                bytes: &mut f.va,
                writable: va_write,
            },
            DataRegion {
                address: SAVE,
                bytes: &mut f.save,
                writable: false,
            },
            DataRegion {
                address: STACK,
                bytes: &mut f.stack,
                writable: false,
            },
            DataRegion {
                address: ERRNO,
                bytes: &mut f.errno[..errno_len],
                writable: true,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Scanf wrapper did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    // The controls adapter lies beyond the base fixture's executable extent.
    f[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Put an output pointer aliasing the future CALL return word in the
    // writable overflow argument area; fixture sets va.gp_offset48.
    let mut frame = vec![0x48, 0xb8];
    frame.extend((DATA + 512).to_le_bytes());
    frame.extend([0x48, 0x8d, 0x4c, 0x24, 0xf8, 0x48, 0x89, 0x08]);
    frame.extend(code);
    f[0x140..0x140 + frame.len()].copy_from_slice(&frame);
    let mut controlled = vec![0x48, 0xbb];
    controlled.extend((DATA + 800).to_le_bytes());
    controlled.extend([0xd9, 0x2b, 0x0f, 0xae, 0x53, 4]);
    controlled.extend(&code[..code.len() - 1]);
    controlled.extend([0xd9, 0x7b, 12, 0x0f, 0xae, 0x5b, 8, 0xc3]);
    f[0x200..0x200 + controlled.len()].copy_from_slice(&controlled);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(0x790000, Service::Vsscanf)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    let address = BIAS + 0x26f888;
    let s = real
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing scanf GOT")?;
    let off = (address - s.address) as usize;
    s.bytes[off..off + 8].copy_from_slice(&0x790000u64.to_le_bytes());
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1a800]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x200]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let ptrs: Vec<u64> = (0..8).map(|i| DATA + 256 + i * 16).collect();
    let mut f = Fixture::new(
        b"-129 65535 0xff 077 -1 0xabc",
        b"%hhd %hu %i %o %llu %p%n",
        16,
        &ptrs[..7],
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100100
        )?,
        6
    );
    for (index, expected) in [
        vec![127],
        vec![255, 255],
        255u32.to_le_bytes().to_vec(),
        63u32.to_le_bytes().to_vec(),
        u64::MAX.to_le_bytes().to_vec(),
        0xabcu64.to_le_bytes().to_vec(),
        28u32.to_le_bytes().to_vec(),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            &f.data[256 + index * 16..256 + index * 16 + expected.len()],
            expected
        );
    }
    assert_eq!(&f.va[..4], &48u32.to_le_bytes());
    assert_eq!(&f.va[8..16], &(STACK + 24).to_le_bytes());
    assert_eq!(f.errno, 77u32.to_le_bytes());
    for (input, format, result, outputs) in [
        (
            b"abc-def Z!".as_slice(),
            b"%3[a-z]%*1c%3s %2c%n".as_slice(),
            3,
            vec![
                b"abc\0".to_vec(),
                b"def\0".to_vec(),
                b"Z!".to_vec(),
                10u32.to_le_bytes().to_vec(),
            ],
        ),
        (b"x", b"%3c", 1, vec![b"x".to_vec()]),
        (b"", b"%d", u32::MAX as u64, vec![]),
        (b"x", b"%d", 0, vec![]),
        (b"1", b"%*d%d", 0, vec![]),
        (
            b"",
            b"%n%d",
            u32::MAX as u64,
            vec![0u32.to_le_bytes().to_vec()],
        ),
        (b"1 x", b"%d%d", 1, vec![1u32.to_le_bytes().to_vec()]),
        (b"]-abc!", b"%[]a-c-]", 1, vec![b"]-abc\0".to_vec()]),
        (b"abc!", b"%[^!]", 1, vec![b"abc\0".to_vec()]),
        (b"abcde!", b"%[a-c-e]", 1, vec![b"abcde\0".to_vec()]),
        (
            b"0xg",
            b"%i%n",
            1,
            vec![0u32.to_le_bytes().to_vec(), 1u32.to_le_bytes().to_vec()],
        ),
    ] {
        let mut f = Fixture::new(input, format, 40, &ptrs);
        let before = f.data.clone();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                &mut f,
                true,
                true,
                4,
                0x100100
            )?,
            result
        );
        let mut expected = before;
        for (i, bytes) in outputs.into_iter().enumerate() {
            expected[256 + i * 16..256 + i * 16 + bytes.len()].copy_from_slice(&bytes);
        }
        assert_eq!(f.data, expected);
        assert_eq!(f.errno, 77u32.to_le_bytes());
    }
    let mut f = Fixture::new(b"9223372036854775808", b"%jd", 0, &ptrs[..1]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100100
        )?,
        1
    );
    assert_eq!(f.errno, 34u32.to_le_bytes());
    assert_eq!(&f.data[256..264], &i64::MAX.to_le_bytes());
    // Rejected guard cases preserve all planned destination and cursor writes.
    for (output, va_write, data_write, errno_len) in [
        (DATA + 1022, true, true, 4),
        (DATA + 256, false, true, 4),
        (DATA + 256, true, false, 4),
        (DATA + 256, true, true, 3),
        (DATA, true, true, 4),
        (VA, true, true, 4),
        (SAVE, true, true, 4),
        (ERRNO, true, true, 4),
    ] {
        let mut f = Fixture::new(b"9223372036854775808", b"%jd", 0, &[output]);
        let data = f.data.clone();
        let va = f.va;
        let errno = f.errno;
        assert!(invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            va_write,
            data_write,
            errno_len,
            0x100100
        )
        .is_err());
        assert_eq!(f.data, data);
        assert_eq!(f.va, va);
        assert_eq!(f.errno, errno);
    }
    for errno_address in [DATA, DATA + 128, VA, SAVE, STACK] {
        n.kernel = Some(Kernel::new(4096, errno_address)?);
        // GP48 selects a used overflow slot; GP0 selects a used saved slot.
        let gp = if errno_address == STACK { 48 } else { 0 };
        let mut f = Fixture::new(b"9223372036854775808", b"%jd", gp, &ptrs[..1]);
        let before = f.data.clone();
        let va = f.va;
        assert!(invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100100
        )
        .is_err());
        assert_eq!(f.data, before);
        assert_eq!(f.va, va);
    }
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    let mut f = Fixture::new(b"1", b"%d", 48, &[]);
    f.va[8..16].copy_from_slice(&(DATA + 512).to_le_bytes());
    let va = f.va;
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        &mut f,
        true,
        true,
        4,
        0x100140
    )
    .is_err());
    assert_eq!(&f.data[256..260], &[0xa5; 4]);
    assert_eq!(f.va, va);
    // A deferred va cursor update cannot alias a loaded argument slot.
    let mut f = Fixture::new(b"1", b"%d", 0, &[]);
    f.va[16..].copy_from_slice(&VA.to_le_bytes());
    let va = f.va;
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        &mut f,
        true,
        true,
        4,
        0x100100
    )
    .unwrap_err()
    .contains("cursor aliases"));
    assert_eq!(f.va, va);
    for format in [b"%Lf".as_slice(), b"%ls", b"%2$d", b"%[a-a]"] {
        let mut f = Fixture::new(b"1", format, 0, &ptrs[..1]);
        let before = f.data.clone();
        assert!(invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100100
        )
        .is_err());
        assert_eq!(f.data, before);
    }
    for rc in 0..4u32 {
        let input = b"0.1 1e-60 1.0000000596046447753906250000000000000001";
        let mut f = Fixture::new(input, b"%f %e %g%n", 40, &ptrs[..4]);
        let cw = 0x037fu16 | ((rc as u16) << 10);
        f.data[800..802].copy_from_slice(&cw.to_le_bytes());
        f.data[804..808].copy_from_slice(&(0x1f81 | (rc << 13)).to_le_bytes());
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                &mut f,
                true,
                true,
                4,
                0x100200
            )?,
            3
        );
        let inward = rc == 1 || rc == 3;
        let first = if inward { 0x3dcc_ccccu32 } else { 0x3dcc_cccd };
        let halfway = if inward { 0x3f80_0000u32 } else { 0x3f80_0001 };
        assert_eq!(&f.data[256..260], &first.to_le_bytes());
        assert_eq!(&f.data[272..276], &0u32.to_le_bytes());
        assert_eq!(&f.data[288..292], &halfway.to_le_bytes());
        assert_eq!(&f.data[304..308], &(input.len() as u32).to_le_bytes());
        for start in [260, 276, 292, 308] {
            assert_eq!(&f.data[start..start + 4], &[0xa5; 4]);
        }
        assert_eq!(&f.data[808..812], &(0x1fa1 | (rc << 13)).to_le_bytes());
        assert_eq!(&f.data[812..814], &cw.to_le_bytes());
        assert_eq!(f.errno, 34u32.to_le_bytes());
        assert_eq!(&f.va[..4], &48u32.to_le_bytes());
        assert_eq!(&f.va[8..16], &(STACK + 24).to_le_bytes());
    }
    // Mixed widths retain both target conversions and union their effects.
    let mut f = Fixture::new(b"0x1p-149 0.1", b"%a %lf", 0, &ptrs[..2]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100200
        )?,
        2
    );
    assert_eq!(&f.data[256..260], &1u32.to_le_bytes());
    assert_eq!(&f.data[272..280], &0x3fb999999999999au64.to_le_bytes());
    assert_eq!(&f.data[808..812], &0x1fa0u32.to_le_bytes());
    assert_eq!(f.errno, 77u32.to_le_bytes());
    // Precisely three output bytes and three errno bytes must reject before
    // any output, va_list cursor, errno or architectural flag commit.
    for (pointer, errno_len) in [(DATA + 1021, 4), (ptrs[0], 3)] {
        let mut f = Fixture::new(b"1e-60", b"%f", 0, &[pointer]);
        let before = f.data.clone();
        let va = f.va;
        assert!(invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            errno_len,
            0x100200
        )
        .is_err());
        assert_eq!(f.data, before);
        assert_eq!(f.va, va);
        assert_eq!(f.errno, 77u32.to_le_bytes());
    }
    for rc in 0..4u32 {
        let mut f = Fixture::new(b"0.1 1e-400", b"%lf %le%n", 40, &ptrs[..3]);
        let cw = 0x037fu16 | ((rc as u16) << 10);
        f.data[800..802].copy_from_slice(&cw.to_le_bytes());
        f.data[804..808].copy_from_slice(&(0x1f81 | (rc << 13)).to_le_bytes());
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                &mut f,
                true,
                true,
                4,
                0x100200
            )?,
            2
        );
        let first = if rc == 1 || rc == 3 {
            0x3fb9999999999999u64
        } else {
            0x3fb999999999999a
        };
        let tiny = if rc == 1 {
            1u64 << 63
        } else if rc == 2 {
            1
        } else {
            0
        };
        assert_eq!(&f.data[256..264], &first.to_le_bytes());
        assert_eq!(&f.data[272..280], &tiny.to_le_bytes());
        assert_eq!(&f.data[288..292], &10u32.to_le_bytes());
        assert_eq!(&f.data[808..812], &(0x1fb1 | (rc << 13)).to_le_bytes());
        assert_eq!(&f.data[812..814], &cw.to_le_bytes());
        assert_eq!(f.errno, 34u32.to_le_bytes());
        assert_eq!(&f.va[..4], &48u32.to_le_bytes());
        assert_eq!(&f.va[8..16], &(STACK + 16).to_le_bytes());
    }
    // Suppression performs only tokenization, even with mismatched guest RC.
    let mut f = Fixture::new(b"1e400", b"%*f%n", 0, &ptrs[..1]);
    f.data[800..802].copy_from_slice(&0x077fu16.to_le_bytes());
    f.data[804..808].copy_from_slice(&0x1f81u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100200
        )?,
        0
    );
    assert_eq!(&f.data[256..260], &5u32.to_le_bytes());
    assert_eq!(&f.data[808..812], &0x1f81u32.to_le_bytes());
    assert_eq!(f.errno, 77u32.to_le_bytes());
    let mut f = Fixture::new(b"1e+", b"%2lf%c%n", 0, &ptrs[..3]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            4,
            0x100200
        )?,
        2
    );
    assert_eq!(&f.data[256..264], &0x3ff0000000000000u64.to_le_bytes());
    assert_eq!(f.data[272], b'e');
    assert_eq!(&f.data[288..292], &2u32.to_le_bytes());
    for (pointer, errno_len) in [(DATA + 1017, 4), (ptrs[0], 3)] {
        let mut f = Fixture::new(b"1e-400", b"%lf", 0, &[pointer]);
        let before = f.data.clone();
        let va = f.va;
        assert!(invoke(
            &mut n,
            &mut adapter,
            &mut real,
            &mut f,
            true,
            true,
            errno_len,
            0x100200
        )
        .is_err());
        assert_eq!(f.data, before);
        assert_eq!(f.va, va);
        assert_eq!(f.errno, 77u32.to_le_bytes());
    }
    println!(
        "PASS: actual SDL_vsscanf GP/overflow va_list, integer/narrowtext/scanset/count, binary32/binary64 floats with4RC/generalflags/errno and direct24bit rounding, mixed widths, incomplete-exponent rollback, suppression withoutFP effects, cursor advancement, guarded4/8byte outputs/va/errno/returnframe and explicit wide/longdouble boundaries."
    );
    Ok(())
}
