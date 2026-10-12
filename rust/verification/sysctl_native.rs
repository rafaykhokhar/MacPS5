//! Explicit virtual sysctl profile and actual SDL fallback paths.
use native_ps5::{
    guest_memory::DataRegion,
    guest_system::GuestSystemProfile,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_37000000;
const ARGS: u64 = 0x38000000;
const ERRNO: u64 = 0x39000000;
const SERVICE: u64 = 0x7d0000;
fn patch(image: &mut Image, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| {
            address >= s.address && address + bytes.len() as u64 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing sysctl fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 7],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
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
            writable: true,
        });
    }
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [r], &mut regions)? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("sysctl did not return".into()),
    }
}
fn capacity(data: &mut [u8], value: u64) {
    data[64..72].copy_from_slice(&value.to_le_bytes());
}
fn mib(data: &mut [u8], leaf: i32) {
    data[..4].copy_from_slice(&6i32.to_le_bytes());
    data[4..8].copy_from_slice(&leaf.to_le_bytes());
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 48, 0x4c, 0x8b, 0x4f, 40, 0x4c, 0x8b, 0x47, 32, 0x48, 0x8b, 0x4f, 24,
        0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff, 0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Verification-only failed sysconf makes the actual SDL fallback reachable.
    f[0x180..0x188].copy_from_slice(&[0x48, 0xc7, 0xc0, 0xff, 0xff, 0xff, 0xff, 0xc3]);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.configure_system_profile(GuestSystemProfile::new(16u64 << 30, 1, 1)?)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Sysctl)?;
    n.register_service(SERVICE + 16, Service::SysctlByName)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut a = Image::load(&f, 0x100000, 65536)?;
    for (got, target) in [
        (0x26f9c8, 0x100180u64),
        (0x26f9d0, SERVICE + 16),
        (0x26f9d8, SERVICE),
    ] {
        patch(&mut real, got, &target.to_le_bytes())?;
    }
    patch(&mut real, 0x591ff8, &[0; 8])?;
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x50930, 0x510a0]),
            10000,
            archive,
            &directory.join("sdl"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut data = [0xa5; 256];
    let mut errno = 77u32.to_le_bytes();
    for (entry, expected) in [(0x50930, 1), (0x510a0, 16384)] {
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [0, 0, 0, 0, 0, 0, BIAS + entry],
                &mut data,
                true,
                &mut errno
            )?,
            expected
        );
    }
    assert_eq!(errno, 77u32.to_le_bytes());
    mib(&mut data, 12);
    for (limit, expected_errno) in [(0, 12u32), (3, 12), (8, 77), (16, 77)] {
        data[128..144].fill(0xa5);
        capacity(&mut data, limit);
        errno = 77u32.to_le_bytes();
        let result = invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 128, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno,
        )?;
        let copied = limit.min(8) as usize;
        assert_eq!(result, if limit < 8 { u32::MAX as u64 } else { 0 });
        assert_eq!(errno, expected_errno.to_le_bytes());
        assert_eq!(
            &data[128..128 + copied],
            &(16u64 << 30).to_le_bytes()[..copied]
        );
        assert!(data[128 + copied..144].iter().all(|b| *b == 0xa5));
        assert_eq!(&data[64..72], &(copied as u64).to_le_bytes());
    }
    capacity(&mut data, 0);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, 0, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[64..72], &8u64.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, 0, 0, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    data[16..24].copy_from_slice(b"hw.ncpu\0");
    capacity(&mut data, 4);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, DATA + 128, DATA + 64, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[128..132], &1u32.to_le_bytes());
    // Complete planned ranges are guarded before any output/length/errno writes.
    for (output, writable) in [(DATA + 252, true), (DATA + 128, false), (u64::MAX, true)] {
        capacity(&mut data, 8);
        let before = data;
        errno = 77u32.to_le_bytes();
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 2, output, DATA + 64, 0, 0, SERVICE],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // Invalid MIB length is classified before touching the MIB pointer.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [u64::MAX, 1, 0, 0, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 22u32.to_le_bytes());
    // Input and name bytes are snapshotted before overlapping output commits.
    mib(&mut data, 12);
    capacity(&mut data, 8);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..8], &(16u64 << 30).to_le_bytes());
    data[16..24].copy_from_slice(b"hw.ncpu\0");
    capacity(&mut data, 4);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, DATA + 16, DATA + 64, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..20], &1u32.to_le_bytes());
    // Unterminated boundary and unknown/recognized names are distinct cases.
    data[253..].copy_from_slice(b"abc");
    let before = data;
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 253, 0, 0, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert_eq!(errno, 77u32.to_le_bytes());
    data[16..27].copy_from_slice(b"hw.machine\0");
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, 0, 0, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    data[16..27].copy_from_slice(b"no.unknown\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, 0, 0, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 2u32.to_le_bytes());
    // Numeric unsigned int namelen consumes only its low 32 bits.
    mib(&mut data, 12);
    capacity(&mut data, 8);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [
                DATA,
                0x1234567800000002,
                DATA + 128,
                DATA + 64,
                0,
                0,
                SERVICE
            ],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    // Alias commit order is payload, oldlen, then errno.
    capacity(&mut data, 3);
    errno = 77u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 64, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(&data[64..72], &3u64.to_le_bytes());
    assert_eq!(errno, 12u32.to_le_bytes());
    n.kernel = Some(Kernel::new(4096, DATA + 64)?);
    capacity(&mut data, 3);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 64, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(&data[64..72], &12u64.to_le_bytes());
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    // Read-only writes reject a nonnull new pointer even with newlen zero,
    // without reading that pointer, and preserve payload/length.
    capacity(&mut data, 8);
    let before = data;
    errno = 77u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 128, DATA + 64, u64::MAX, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 1u32.to_le_bytes());
    assert_eq!(data, before);
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, 0, u64::MAX, u64::MAX, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    // Numeric input length copy precedes name resolution, whereas named
    // lookup fails before entering the numeric oldlen-copy boundary.
    mib(&mut data, 99);
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, 0, u64::MAX, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    data[16..27].copy_from_slice(b"no.unknown\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, 0, u64::MAX, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 2u32.to_le_bytes());
    mib(&mut data, 12);
    data[8..12].copy_from_slice(&1i32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 3, 0, 0, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 20u32.to_le_bytes());
    data[16..26].copy_from_slice(b"hw.ncpu.x\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, 0, 0, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 2u32.to_le_bytes());
    data[16..25].copy_from_slice(b"hw.ncpu.\0");
    capacity(&mut data, 4);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA + 16, DATA + 128, DATA + 64, 0, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[128..132], &1u32.to_le_bytes());
    // Error destinations are also preguarded before partial payload commits.
    capacity(&mut data, 3);
    let before = data;
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 128, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut short
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert_eq!(short, [0xa5; 3]);
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 2, DATA + 128, DATA + 64, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(data, before);
    println!(
        "Actual SDL sysctl fallbacks, bounded profile queries, sizing/partial copies and guards passed"
    );
    Ok(())
}
