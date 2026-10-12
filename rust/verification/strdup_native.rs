//! Owned strdup errno-order regression and the real SDL_GetBasePath tail jump.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_57000001;
const ARGS: u64 = 0x58000000;
const ERRNO: u64 = 0x59000000;
const SERVICE: u64 = 0x830000;
const BIAS: u64 = 0x10000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: Option<(&mut [u8], bool)>,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut regions = vec![
        DataRegion {
            address: ARGS,
            bytes: &mut table,
            writable: false,
        },
        DataRegion {
            address: DATA,
            bytes: data,
            writable: true,
        },
    ];
    if let Some((bytes, writable)) = errno {
        regions.push(DataRegion {
            address: ERRNO,
            bytes,
            writable,
        });
    }
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [real], &mut regions)? {
        Execution::WorkerReturned(p) => Ok(p),
        _ => Err("strdup fixture suspended".into()),
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
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (i, s) in [Service::Strdup, Service::Memcpy, Service::Free]
        .into_iter()
        .enumerate()
    {
        n.register_service(SERVICE + 16 * i as u64, s)?;
    }
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fa60;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strdup GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SERVICE.to_le_bytes());
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
            BTreeSet::from([0x100d37]),
            1000,
            archive,
            &directory.join("real-tail"),
        )?;
    }
    let mut data = [0xa5; 64];
    let mut errno = 77u32.to_le_bytes();
    for target in [SERVICE, BIAS + 0x100d37] {
        for mapped in [false, true] {
            data[..6].copy_from_slice(b"hello\0");
            let before = n.bytes_allocated();
            let p = invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 0, 0, target],
                &mut data,
                if mapped {
                    Some((&mut errno, false))
                } else {
                    None
                },
            )?;
            assert!(p > u32::MAX as u64);
            assert!(n.bytes_allocated() > before);
            data[0] = b'X';
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA + 32, p, 6, SERVICE + 16],
                &mut data,
                None,
            )?;
            assert_eq!(&data[32..38], b"hello\0");
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [p, 0, 0, SERVICE + 32],
                &mut data,
                None,
            )?;
            assert_eq!(n.bytes_allocated(), before);
            assert_eq!(errno, 77u32.to_le_bytes());
        }
    }
    for source in [0, u64::MAX, DATA + 64] {
        let before = n.bytes_allocated();
        let snapshot = data;
        let error = invoke(
            &mut n,
            &mut a,
            &mut real,
            [source, 0, 0, SERVICE],
            &mut data,
            None,
        )
        .unwrap_err();
        assert!(
            !error.to_lowercase().contains("errno"),
            "Source error must precede errno: {error}"
        );
        assert_eq!(n.bytes_allocated(), before);
        assert_eq!(data, snapshot);
    }
    println!(
        "Native strdup success with readonly/unmapped errno, independent owned copy/free, invalid-source ordering and actual SDL_GetBasePath tail passed; ENOMEM guards covered separately by runtime model tests"
    );
    Ok(())
}
