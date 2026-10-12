//! Signal disposition registration/query through actual SDL init/quit, no delivery.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_3a000000;
const ARGS: u64 = 0x3b000000;
const ERRNO: u64 = 0x3c000000;
const SERVICE: u64 = 0x7e0000;
fn patch(image: &mut Image, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| {
            address >= s.address && address + bytes.len() as u64 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing signal fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
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
        _ => Err("sigaction did not return".into()),
    }
}
fn query(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    signal: u64,
    data: &mut [u8],
    errno: &mut [u8],
) -> Result<[u8; 32], String> {
    assert_eq!(
        invoke(n, a, r, [signal, 0, DATA + 64, SERVICE], data, true, errno)?,
        0
    );
    Ok(data[64..96].try_into().unwrap())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Sigaction)?;
    n.register_service(SERVICE + 16, Service::Getenv)?;
    n.register_service(SERVICE + 32, Service::Signal)?;
    n.environment = 0;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut a = Image::load(&f, 0x100000, 65536)?;
    patch(&mut real, 0x26f9e0, &SERVICE.to_le_bytes())?;
    patch(&mut real, 0x26f7d0, &(SERVICE + 16).to_le_bytes())?;
    patch(&mut real, 0x590168, &[0; 8])?;
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xc1a20, 0xc1ac0, 0x19650, 0x19b00]),
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
    let mut data = [0xa5; 160];
    let mut errno = 77u32.to_le_bytes();
    for signal in [2, 15] {
        assert_eq!(
            &query(&mut n, &mut a, &mut real, signal, &mut data, &mut errno)?[..8],
            &0u64.to_le_bytes()
        );
    }
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [0, 0, 0, BIAS + 0xc1a20],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    for signal in [2, 15] {
        let old = query(&mut n, &mut a, &mut real, signal, &mut data, &mut errno)?;
        assert_eq!(&old[..8], &(BIAS + 0xc1b80).to_le_bytes());
    }
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [0, 0, 0, BIAS + 0xc1ac0],
        &mut data,
        true,
        &mut errno,
    )?;
    for signal in [2, 15] {
        assert_eq!(
            &query(&mut n, &mut a, &mut real, signal, &mut data, &mut errno)?[..8],
            &0u64.to_le_bytes()
        );
    }
    assert_eq!(errno, 77u32.to_le_bytes());
    // Preserve an application disposition through SDL init and quit.
    data[..32].fill(0);
    data[..8].copy_from_slice(&1u64.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [2, DATA, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [0, 0, 0, BIAS + 0xc1a20],
        &mut data,
        true,
        &mut errno,
    )?;
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [0, 0, 0, BIAS + 0xc1ac0],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(
        &query(&mut n, &mut a, &mut real, 2, &mut data, &mut errno)?[..8],
        &1u64.to_le_bytes()
    );
    // Complete old-action output must be guarded before changing disposition.
    for (output, writable) in [(DATA + 144, true), (DATA + 64, false), (u64::MAX, true)] {
        data[..32].fill(0);
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [2, DATA, output, SERVICE],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(
            &query(&mut n, &mut a, &mut real, 2, &mut data, &mut errno)?[..8],
            &1u64.to_le_bytes()
        );
    }
    // Exact input/output alias snapshots the incoming action before replacing it.
    data[..32].fill(0);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [2, DATA, DATA, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..8], &1u64.to_le_bytes());
    assert_eq!(
        &query(&mut n, &mut a, &mut real, 2, &mut data, &mut errno)?[..8],
        &0u64.to_le_bytes()
    );
    // Libthr-visible caught dispositions preserve caller flags and padding,
    // retaining SIGKILL/SIGSTOP mask bits but removing reserved signal32.
    data[..32].fill(0xff);
    let handler = 0x123456789abcdef0u64;
    data[..8].copy_from_slice(&handler.to_le_bytes());
    data[8..12].copy_from_slice(&0x7fu32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [10, DATA, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    let got = query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?;
    assert_eq!(&got[..8], &handler.to_le_bytes());
    assert_eq!(&got[8..12], &0x7fu32.to_le_bytes());
    assert_eq!(&got[12..16], &0x7fffffffu32.to_le_bytes());
    assert_eq!(&got[16..], [0xff; 16]);
    // Low32 C-int signum, and invalid/reserved numbers before pointer access.
    assert_eq!(
        &query(
            &mut n,
            &mut a,
            &mut real,
            0x123456780000000a,
            &mut data,
            &mut errno
        )?[..8],
        &handler.to_le_bytes()
    );
    for signum in [0, u64::MAX, 129, 32] {
        errno = 77u32.to_le_bytes();
        let before = data;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [signum, u64::MAX, u64::MAX, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, 22u32.to_le_bytes());
        assert_eq!(data, before);
    }
    // Uncatchable signals reject nondefault dispositions and preserve state.
    for signum in [9, 17] {
        data[..32].fill(0);
        data[..8].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [signum, DATA, 0, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            u32::MAX as u64
        );
        assert_eq!(
            &query(&mut n, &mut a, &mut real, signum, &mut data, &mut errno)?[..8],
            &0u64.to_le_bytes()
        );
    }
    // Caught unknown flags fail without replacing the existing action.
    data[..32].fill(0);
    data[..8].copy_from_slice(&handler.to_le_bytes());
    data[8..12].copy_from_slice(&0x80u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [10, DATA, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        u32::MAX as u64
    );
    assert_eq!(
        &query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?[..8],
        &handler.to_le_bytes()
    );
    // signal() returns the complete previous pointer and establishes its own
    // empty mask/SA_RESTART disposition, without delivering a signal.
    let next = 0xfedcba9876543210u64;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [10, next, 0, SERVICE + 32],
            &mut data,
            true,
            &mut errno
        )?,
        handler
    );
    let got = query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?;
    assert_eq!(&got[..8], &next.to_le_bytes());
    assert_eq!(&got[8..12], &2u32.to_le_bytes());
    assert_eq!(&got[12..], &[0; 20]);
    for signal in [0, 32, 129] {
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [signal, handler, 0, SERVICE + 32],
                &mut data,
                true,
                &mut errno
            )?,
            u64::MAX
        );
        assert_eq!(errno, 22u32.to_le_bytes());
    }
    assert_eq!(
        query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?,
        got
    );
    // Canonical default/ignore differs from the caught libthr cache.
    for signal in [10, 20] {
        data[..32].fill(0xff);
        data[..8].copy_from_slice(&1u64.to_le_bytes());
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [signal, DATA, 0, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        let got = query(&mut n, &mut a, &mut real, signal, &mut data, &mut errno)?;
        assert_eq!(
            &got[8..12],
            &(if signal == 20 { 0x3fu32 } else { 0x17 }).to_le_bytes()
        );
        assert_eq!(&got[12..16], &0xfffefeffu32.to_le_bytes());
        assert_eq!(&got[28..], &[0; 4]);
    }
    // Invalid action copy and failed errno copy cannot publish any state.
    let before = query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?;
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [10, DATA + 144, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(
        query(&mut n, &mut a, &mut real, 10, &mut data, &mut errno)?,
        before
    );
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [0, 0, 0, SERVICE],
            &mut data,
            true,
            &mut short
        )
        .is_err()
    );
    assert_eq!(short, [0xa5; 3]);
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [0, 0, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    println!(
        "Actual SDL signal registration/query/init/quit and guarded action copies passed; signal delivery not exercised"
    );
    Ok(())
}
