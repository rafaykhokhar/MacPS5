//! Configured virtual getcwd and bounded real SDL_GetBasePath callsite.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_47000001;
const ARGS: u64 = 0x48000000;
const ERRNO: u64 = 0x49000000;
const SERVICE: u64 = 0x820000;
const CWD: &[u8] = b"/app0/scene\0";
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing getcwd fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn fixed(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
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
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [], &mut regions)? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("getcwd call did not return".into()),
    }
}
fn executor() -> Result<NativeExecutor, String> {
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (index, service) in [
        Service::Getcwd,
        Service::Free,
        Service::Memcpy,
        Service::Realloc,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(SERVICE + 16 * index as u64, service)?;
    }
    Ok(n)
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x280u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x280u64.to_le_bytes());
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut branch = vec![0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec, 0x10, 4, 0, 0];
    fixed(&mut branch, 0xb8, BIAS + 0x100cf4);
    branch.extend([0xff, 0xe0]);
    f[0x200..0x200 + branch.len()].copy_from_slice(&branch);
    let mut n = executor()?;
    n.kernel
        .as_mut()
        .unwrap()
        .register_working_directory(&CWD[..CWD.len() - 1])?;
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    patch(&mut real, 0x26fa50, SERVICE)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x100cf4]),
            10000,
            archive,
            &directory.join("callsite"),
        )?;
    }
    let mut data = [0xa5; 128];
    let mut errno = 77u32.to_le_bytes();
    for size in [CWD.len() as u64, 1024, 1u64 << 32, u64::MAX] {
        data.fill(0xa5);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [DATA, size, 0, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            DATA
        );
        assert_eq!(&data[..CWD.len()], CWD);
        assert_eq!(data[CWD.len()], 0xa5);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    for (size, error) in [(0, 22u32), (1, 34), (CWD.len() as u64 - 1, 34)] {
        data.fill(0xa5);
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [DATA, size, 0, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        assert_eq!(errno, error.to_le_bytes());
        assert_eq!(data, [0xa5; 128]);
    }
    for size in [0u64, 1, u64::MAX] {
        errno = 77u32.to_le_bytes();
        let before = n.bytes_allocated();
        let p = invoke(
            &mut n,
            &mut a,
            [0, size, 0, SERVICE],
            &mut data,
            true,
            &mut errno,
        )?;
        assert!(p > u32::MAX as u64);
        assert_eq!(n.bytes_allocated(), before + 1024);
        invoke(
            &mut n,
            &mut a,
            [DATA, p, CWD.len() as u64, SERVICE + 32],
            &mut data,
            true,
            &mut errno,
        )?;
        assert_eq!(&data[..CWD.len()], CWD);
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, p + 1023, 1, SERVICE + 32],
            &mut data,
            true,
            &mut errno,
        )?;
        let resized = invoke(
            &mut n,
            &mut a,
            [p, 2048, 0, SERVICE + 48],
            &mut data,
            true,
            &mut errno,
        )?;
        assert_ne!(resized, 0);
        invoke(
            &mut n,
            &mut a,
            [DATA, resized, CWD.len() as u64, SERVICE + 32],
            &mut data,
            true,
            &mut errno,
        )?;
        assert_eq!(&data[..CWD.len()], CWD);
        invoke(
            &mut n,
            &mut a,
            [resized, 0, 0, SERVICE + 16],
            &mut data,
            true,
            &mut errno,
        )?;
        assert_eq!(n.bytes_allocated(), before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // Valid capacity still requires a complete writable destination.
    for (dst, writable) in [(DATA + 127, true), (DATA, false), (u64::MAX, true)] {
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut a,
                [dst, 1024, 0, SERVICE],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            [u64::MAX, 0, 0, SERVICE],
            &mut data,
            true,
            &mut short
        )
        .is_err()
    );
    assert_eq!(short, [0xa5; 3]);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA, 1024, 0, SERVICE],
            &mut data,
            true,
            &mut []
        )?,
        DATA
    );
    // Successful caller-buffer and allocating forms never require writable errno.
    for dst in [DATA, 0] {
        let mut descriptor: Vec<u8> = [dst, u64::MAX, 0, SERVICE]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect();
        errno = 77u32.to_le_bytes();
        let result = n.run_worker_with_data(
            &mut a,
            0x100100,
            ARGS,
            &mut [],
            &mut [
                DataRegion {
                    address: ARGS,
                    bytes: &mut descriptor,
                    writable: false,
                },
                DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true,
                },
                DataRegion {
                    address: ERRNO,
                    bytes: &mut errno,
                    writable: false,
                },
            ],
        )?;
        let Execution::WorkerReturned(p) = result else {
            return Err("Readonly errno getcwd did not return".into());
        };
        assert_eq!(errno, 77u32.to_le_bytes());
        if dst == 0 {
            assert_ne!(p, 0);
            invoke(
                &mut n,
                &mut a,
                [p, 0, 0, SERVICE + 16],
                &mut data,
                true,
                &mut [],
            )?;
        } else {
            assert_eq!(p, DATA);
        }
    }
    // Real branch sets destination rbp-0x410 and size1024, then calls the GOT.
    n.set_worker_quantum(Some(1))?;
    let mut outcome = n.run_worker_with_data(
        &mut a,
        0x100200,
        0,
        &mut [&mut real],
        &mut [DataRegion {
            address: ERRNO,
            bytes: &mut errno,
            writable: true,
        }],
    )?;
    let mut observed = false;
    for _ in 0..24 {
        let Execution::QuantumYield(mut wait) = outcome else {
            return Err("Expected getcwd callsite quantum".into());
        };
        let stack = wait.stack_region()?;
        let at = stack.bytes.len() - 16 - 0x410;
        if &stack.bytes[at..at + CWD.len()] == CWD {
            observed = true;
            break;
        }
        outcome = n.resume_quantum(
            &mut a,
            &mut [&mut real],
            &mut wait,
            &mut [DataRegion {
                address: ERRNO,
                bytes: &mut errno,
                writable: true,
            }],
        )?;
    }
    assert!(observed);
    n.set_worker_quantum(None)?;
    missing(&f, archive, &directory.join("missing"))?;
    println!(
        "Native configured getcwd, allocation lifecycle and bounded actual SDL callsite passed; no host cwd access"
    );
    Ok(())
}
fn missing(f: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    let mut n = executor()?;
    let mut a = Image::load(f, 0x100000, 65536)?;
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
    let mut data = [0xa5; 128];
    let mut errno = 77u32.to_le_bytes();
    for (size, error) in [(0, 22u32), (1, 34)] {
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [u64::MAX, size, 0, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        assert_eq!(errno, error.to_le_bytes());
    }
    errno = 77u32.to_le_bytes();
    let before = n.bytes_allocated();
    assert!(
        invoke(
            &mut n,
            &mut a,
            [DATA, 1024, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(data, [0xa5; 128]);
    assert!(
        invoke(
            &mut n,
            &mut a,
            [0, u64::MAX, 0, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(n.bytes_allocated(), before);
    Ok(())
}
