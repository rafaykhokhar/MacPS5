//! Explicit virtual guest capacity via actual SDL CPU/RAM queries; no host probing.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const SERVICE: u64 = 0x7c0000;
const ARGS: u64 = 0x34000000;
const ERRNO: u64 = 0x35000000;
fn patch(image: &mut Image, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let address = BIAS + offset;
    let segment = image
        .segments
        .iter_mut()
        .find(|s| {
            address >= s.address && address + bytes.len() as u64 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing sysconf fixture range")?;
    let at = (address - segment.address) as usize;
    segment.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    selector: u64,
    target: u64,
    errno: &mut [u8],
    writable: bool,
) -> Result<u64, String> {
    let mut args = Vec::from(selector.to_le_bytes());
    args.extend(target.to_le_bytes());
    let mut regions = vec![DataRegion {
        address: ARGS,
        bytes: &mut args,
        writable: false,
    }];
    if !errno.is_empty() {
        regions.push(DataRegion {
            address: ERRNO,
            bytes: errno,
            writable,
        });
    }
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [real], &mut regions)? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("sysconf query did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let code = [0x48, 0x8b, 0x47, 8, 0x48, 0x8b, 0x3f, 0xff, 0xd0, 0xc3];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Sysconf)?;
    configure(&mut n, 16 * 1024 * 1024 * 1024)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    patch(&mut real, 0x26f9c8, &SERVICE.to_le_bytes())?;
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
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut errno = 77u32.to_le_bytes();
    for (selector, expected) in [
        (58, 1),
        (57, 1),
        (47, 16384),
        (121, 1048576),
        (0x123456780000003a, 1),
    ] {
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                selector,
                SERVICE,
                &mut errno,
                true
            )?,
            expected
        );
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            0,
            BIAS + 0x50930,
            &mut errno,
            true
        )?,
        1
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            0,
            BIAS + 0x510a0,
            &mut errno,
            true
        )?,
        16384
    );
    // Poison the import after first calls: cached paths must not query it again.
    patch(&mut real, 0x26f9c8, &0xdeadbeefu64.to_le_bytes())?;
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            0,
            BIAS + 0x50930,
            &mut errno,
            true
        )?,
        1
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            0,
            BIAS + 0x510a0,
            &mut errno,
            true
        )?,
        16384
    );
    patch(&mut real, 0x26f9c8, &SERVICE.to_le_bytes())?;
    // Invalid signed C int names set EINVAL and return full-width long -1.
    for selector in [u64::MAX, 0x12345678ffffffff, 53, 54, 55, 123] {
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                selector,
                SERVICE,
                &mut errno,
                true
            )?,
            u64::MAX
        );
        assert_eq!(errno, 22u32.to_le_bytes());
    }
    for writable in [false, true] {
        let mut short = 77u32.to_le_bytes();
        let length = if writable { 3 } else { 4 };
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                u64::MAX,
                SERVICE,
                &mut short[..length],
                writable
            )
            .is_err()
        );
        assert_eq!(short, 77u32.to_le_bytes());
    }
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            49,
            SERVICE,
            &mut errno,
            true
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    // Successful queries do not access errno, even if the errno mapping is absent.
    assert_eq!(
        invoke(&mut n, &mut adapter, &mut real, 58, SERVICE, &mut [], false)?,
        1
    );
    // At this adapter's CALL the return address is stack_end-16; errno may not overwrite it.
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            u64::MAX,
            SERVICE,
            &mut errno,
            true
        )
        .is_err()
    );
    boundary(&f, archive, &directory.join("capacity"))?;
    println!(
        "Actual SDL sysconf CPU/RAM queries, caches, explicit virtual profile and guarded errno passed"
    );
    Ok(())
}

fn configure(n: &mut NativeExecutor, bytes: u64) -> Result<(), String> {
    n.configure_system_profile(native_ps5::guest_system::GuestSystemProfile::new(
        bytes, 1, 1,
    )?)
}
fn boundary(f: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Sysconf)?;
    let bytes = u64::MAX & !16383;
    configure(&mut n, bytes)?;
    unsafe {
        n.compile_image(
            f,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut a = Image::load(f, 0x100000, 65536)?;
    let mut args = Vec::from(121u64.to_le_bytes());
    args.extend(SERVICE.to_le_bytes());
    assert!(
        matches!(n.run_worker_with_data(&mut a,0x100100,ARGS,&mut[],&mut[DataRegion{address:ARGS,bytes:&mut args,writable:false}])?,Execution::WorkerReturned(v)if v==bytes/16384)
    );
    assert!(configure(&mut n, 16 * 1024 * 1024 * 1024).is_err());
    assert!(native_ps5::guest_system::GuestSystemProfile::new(u64::MAX, 1, 1).is_err());
    Ok(())
}
