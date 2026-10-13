//! Native unbuffered regular FILE and redirected stdin byte input.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_67000001;
const ARGS: u64 = 0x68000000;
const ERRNO: u64 = 0x69000000;
const SERVICE: u64 = 0x840000;
const BIAS: u64 = 0x10000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 5],
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
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("fgetc fixture suspended".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    use native_ps5::stdio_file::{Operation, STDIN_FILE_ADDR};
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut inventory = Image::load(file, 0, 64 * 1024 * 1024)?;
    inventory.relocate(|_| None)?;
    assert!(
        inventory
            .unresolved
            .iter()
            .any(|i| i.name == "fgetc" && i.address == 0x26fbd0)
    );
    let mut fixture = memory_fixture();
    let code = [
        0x4c, 0x8b, 0x5f, 24, 0x48, 0x8b, 0x47, 32, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8,
        0x48, 0x8b, 0x3f, 0x48, 0x83, 0xec, 8, 0x41, 0xff, 0xd3, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    let mut kernel = Kernel::new(4096, ERRNO)?;
    kernel.files.allow_create_directory(b"/save")?;
    kernel
        .files
        .register_writable(b"/save/read", vec![0, 255, 65])?;
    kernel.files.register(b"/save/input", vec![0, 255])?;
    n.kernel = Some(kernel);
    n.configure_stdin(b"/save/input")?;
    for (i, op) in [
        Operation::ReadByte,
        Operation::Open,
        Operation::Seek,
        Operation::Tell,
        Operation::Close,
        Operation::Fileno,
        Operation::Eof,
        Operation::Error,
        Operation::Clearerr,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(SERVICE + i as u64 * 16, Service::File(op))?;
    }
    n.register_service(SERVICE + 144, Service::Memcpy)?;
    n.register_service(SERVICE + 160, Service::Syscall)?;
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut a = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut data = [0u8; 512];
    data[..11].copy_from_slice(b"/save/read\0");
    data[32..34].copy_from_slice(b"r\0");
    data[40..42].copy_from_slice(b"w\0");
    let mut errno = 77u32.to_le_bytes();
    let stream = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, DATA + 32, 0, SERVICE + 16, 0],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert_ne!(stream, 0);
    for (handle, expected) in [
        (stream, vec![0u64, 255, 65]),
        (STDIN_FILE_ADDR, vec![0, 255]),
    ] {
        for (value, position) in expected.into_iter().zip(1u64..) {
            assert_eq!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut real,
                    [handle, 0, 0, SERVICE, 0],
                    &mut data,
                    Some((&mut errno, false))
                )?,
                value
            );
            assert_eq!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut real,
                    [handle, 0, 0, SERVICE + 48, 0],
                    &mut data,
                    Some((&mut errno, true))
                )?,
                position
            );
        }
        for _ in 0..2 {
            assert_eq!(
                invoke(
                    &mut n,
                    &mut a,
                    &mut real,
                    [handle, 0, 0, SERVICE, 0],
                    &mut data,
                    Some((&mut errno, false))
                )?,
                u32::MAX as u64
            );
        }
        assert_ne!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [handle, 0, 0, SERVICE + 96, 0],
                &mut data,
                None
            )?,
            0
        );
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [handle, 0, 0, SERVICE + 112, 0],
                &mut data,
                None
            )?,
            0
        );
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [handle, 0, 0, SERVICE + 128, 0],
            &mut data,
            None,
        )?;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [handle, 0, 0, SERVICE + 96, 0],
                &mut data,
                None
            )?,
            0
        );
    }
    assert_eq!(errno, 77u32.to_le_bytes());
    // Seeking clears EOF and resets exact position; corrupted FILE rejected.
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stream, 0, 0, SERVICE + 32, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA + 256, stream, 32, SERVICE + 144, 0],
        &mut data,
        None,
    )?;
    data[288] = data[272] ^ 0x55;
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [stream + 16, DATA + 288, 1, SERVICE + 144, 0],
        &mut data,
        None,
    )?;
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stream, 0, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, true))
        )
        .is_err()
    );
    invoke(
        &mut n,
        &mut a,
        &mut real,
        [stream + 16, DATA + 272, 1, SERVICE + 144, 0],
        &mut data,
        None,
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stream, 0, 0, SERVICE + 48, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stream, 0, 0, SERVICE + 64, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    let writeonly = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, DATA + 40, 0, SERVICE + 16, 0],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert_ne!(writeonly, 0);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [writeonly, 0, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 9u32.to_le_bytes());
    assert_ne!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [writeonly, 0, 0, SERVICE + 112, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [writeonly, 0, 0, SERVICE + 96, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [writeonly, 0, 0, SERVICE + 64, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    let stale = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, DATA + 32, 0, SERVICE + 16, 0],
        &mut data,
        Some((&mut errno, true)),
    )?;
    let fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [stale, 0, 0, SERVICE + 80, 0],
        &mut data,
        None,
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, 0, 0, SERVICE + 160, 6],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stale, 0, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, false))
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stale, 0, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 9u32.to_le_bytes());
    assert_ne!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stale, 0, 0, SERVICE + 112, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [stale, 0, 0, SERVICE + 64, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    for bad in [0, DATA, u64::MAX] {
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [bad, 0, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )
            .is_err()
        );
    }
    println!(
        "PASS: native fgetc regular FILE/redirected stdin unsigned0/255, exact positions and sticky EOF/error distinction, seek/clearerr, corrupted FILE and errno-write guards, write-only/closedfd errors and cleanup; owned unbuffered profile"
    );
    Ok(())
}
