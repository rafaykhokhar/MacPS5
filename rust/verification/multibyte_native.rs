//! Native C-byte/wchar16 conversions, including pinned homebrew _l tail wrappers.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_3d000001;
const ARGS: u64 = 0x3e000000;
const ERRNO: u64 = 0x3f000000;
const MB: u64 = 0x7f0000;
const WC: u64 = MB + 16;
const MBS: u64 = MB + 32;
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing multibyte GOT")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    target: u64,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
    errno_writable: bool,
) -> Result<u64, String> {
    // Invalid locale is passed through actual _l thunks, which discard it.
    let mut descriptor: Vec<u8> = args
        .into_iter()
        .chain([u64::MAX, target])
        .flat_map(u64::to_le_bytes)
        .collect();
    let mut regions = vec![DataRegion {
        address: ARGS,
        bytes: &mut descriptor,
        writable: false,
    }];
    if !data.is_empty() {
        regions.push(DataRegion {
            address: DATA,
            bytes: data,
            writable,
        });
    }
    if !errno.is_empty() {
        regions.push(DataRegion {
            address: ERRNO,
            bytes: errno,
            writable: errno_writable,
        });
    }
    match n.run_worker_with_data(adapter, 0x100100, ARGS, &mut [real], &mut regions)? {
        Execution::WorkerReturned(value) => Ok(value),
        _ => Err("Multibyte invocation did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    // r8=arg4, rcx=arg3, rdx=arg2, rsi=arg1, rax=target, rdi=arg0.
    let code = [
        0x4c, 0x8b, 0x47, 32, 0x48, 0x8b, 0x4f, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8,
        0x48, 0x8b, 0x47, 40, 0x48, 0x8b, 0x3f, 0xff, 0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (at, service) in [
        (MB, Service::Mbrtowc),
        (WC, Service::Wcrtomb),
        (MBS, Service::Mbsrtowcs),
    ] {
        n.register_service(at, service)?;
    }
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    for (offset, target) in [(0x26faa8, MB), (0x26fab8, WC), (0x26fac8, MBS)] {
        patch(&mut real, offset, target)?;
    }
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1162f0, 0x116330, 0x116080]),
            1000,
            archive,
            &directory.join("wrappers"),
        )?;
    }
    let mut data = [0xa5u8; 2048];
    let mut errno = 77u32.to_le_bytes();
    // All 256 byte values, exact two-byte wchar writes and exact one-byte inverse.
    // Invalid state pointer and readonly errno must never be consulted on success.
    for byte in 0..=255u16 {
        data[64] = byte as u8;
        data[1024..1028].fill(0xa5);
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                MB,
                [DATA + 1024, DATA + 64, u64::MAX, u64::MAX],
                &mut data,
                true,
                &mut errno,
                false
            )?,
            u64::from(byte != 0)
        );
        assert_eq!(&data[1024..1028], &[byte as u8, 0, 0xa5, 0xa5]);
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                WC,
                [
                    DATA + 1024,
                    0xdead_beef_cafe_0000 | u64::from(byte),
                    u64::MAX,
                    0
                ],
                &mut data,
                true,
                &mut errno,
                false
            )?,
            1
        );
        assert_eq!(data[1024], byte as u8);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            MB,
            [u64::MAX, 0, 0, u64::MAX],
            &mut [],
            false,
            &mut [],
            false
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            MB,
            [u64::MAX, u64::MAX, 0, u64::MAX],
            &mut [],
            false,
            &mut [],
            false
        )?,
        u64::MAX - 1
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            WC,
            [0, u64::MAX, u64::MAX, 0],
            &mut [],
            false,
            &mut [],
            false
        )?,
        1
    );
    data[64] = 0xff;
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            MB,
            [0, DATA + 64, 1, u64::MAX],
            &mut data,
            false,
            &mut [],
            false
        )?,
        1
    );
    for invalid in [0x100u64, 0xd800, 0xffff] {
        data[1024] = 0xa5;
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                WC,
                [DATA + 1024, invalid, u64::MAX, 0],
                &mut data,
                true,
                &mut errno,
                true
            )?,
            u64::MAX
        );
        assert_eq!(errno, 86u32.to_le_bytes());
        assert_eq!(data[1024], 0xa5);
    }
    for (writable, length) in [(false, 4usize), (true, 3usize), (false, 0usize)] {
        let before = data;
        let mut error_bytes = vec![0xa5; length];
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                WC,
                [DATA + 1024, 0x100, u64::MAX, 0],
                &mut data,
                true,
                &mut error_bytes,
                writable
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(error_bytes, vec![0xa5; length]);
    }
    // Caller return slot aliases errno: EILSEQ must fail without committing output.
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            WC,
            [DATA + 1024, 0x100, 0, 0],
            &mut data,
            true,
            &mut [],
            false
        )
        .is_err()
    );
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for byte in 1..=255 {
        data[63 + byte] = byte as u8;
    }
    data[319] = 0;
    for count in [0, 3, 255, u64::MAX] {
        data[32..40].copy_from_slice(&(DATA + 64).to_le_bytes());
        data[1024..].fill(0xa5);
        let converted = count.min(255);
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                MBS,
                [DATA + 1024, DATA + 32, count, u64::MAX],
                &mut data,
                true,
                &mut [],
                false
            )?,
            converted
        );
        for i in 0..converted as usize {
            assert_eq!(&data[1024 + i * 2..1026 + i * 2], &[(i + 1) as u8, 0]);
        }
        let next = if count > 255 {
            0
        } else {
            DATA + 64 + converted
        };
        assert_eq!(&data[32..40], &next.to_le_bytes());
        if count > 255 {
            assert_eq!(&data[1534..1536], &[0, 0]);
        } else {
            assert_eq!(data[1024 + converted as usize * 2], 0xa5);
        }
    }
    data[32..40].copy_from_slice(&(DATA + 64).to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            MBS,
            [0, DATA + 32, 0, u64::MAX],
            &mut data,
            false,
            &mut [],
            false
        )?,
        255
    );
    assert_eq!(&data[32..40], &(DATA + 64).to_le_bytes());
    data[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            MBS,
            [u64::MAX, DATA + 32, 0, u64::MAX],
            &mut data,
            true,
            &mut [],
            false
        )?,
        0
    );
    assert_eq!(&data[32..40], &u64::MAX.to_le_bytes());
    // Invalid output/source guards never publish partial writes or source updates.
    for (target, args, writable) in [
        (MB, [DATA + 2047, DATA + 64, 1, u64::MAX], true),
        (MB, [DATA + 1024, u64::MAX, 1, u64::MAX], true),
        (MB, [DATA + 1024, DATA + 64, 1, u64::MAX], false),
        (WC, [u64::MAX, 65, u64::MAX, 0], true),
        (MBS, [DATA + 2047, DATA + 32, 3, u64::MAX], true),
        (MBS, [DATA + 1024, DATA + 32, 3, u64::MAX], false),
    ] {
        data[32..40].copy_from_slice(&(DATA + 64).to_le_bytes());
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                target,
                args,
                &mut data,
                writable,
                &mut [],
                false
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    // Exact pinned executable tail wrappers exercise the real GOT bindings.
    data[64..68].copy_from_slice(&[0x80, 0xff, b'A', 0]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            BIAS + 0x1162f0,
            [DATA + 1024, DATA + 64, 1, u64::MAX],
            &mut data,
            true,
            &mut [],
            false
        )?,
        1
    );
    assert_eq!(&data[1024..1026], &[0x80, 0]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            BIAS + 0x116330,
            [DATA + 1024, 0xff, u64::MAX, 0],
            &mut data,
            true,
            &mut [],
            false
        )?,
        1
    );
    data[32..40].copy_from_slice(&(DATA + 64).to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            BIAS + 0x116080,
            [DATA + 1024, DATA + 32, u64::MAX, u64::MAX],
            &mut data,
            true,
            &mut [],
            false
        )?,
        3
    );
    assert_eq!(&data[1024..1032], &[0x80, 0, 0xff, 0, b'A', 0, 0, 0]);
    assert_eq!(&data[32..40], &0u64.to_le_bytes());
    println!(
        "Native C-byte mbrtowc/wcrtomb/mbsrtowcs, wchar16/full64/errno guards and actual pinned _l tail wrappers passed; no UTF8 locale claimed"
    );
    Ok(())
}
