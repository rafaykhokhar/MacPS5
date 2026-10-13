//! Native opaque directory streams over an explicitly registered guest namespace.
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
        _ => Err("directory fixture suspended".into()),
    }
}
fn path(data: &mut [u8], p: &[u8]) {
    data[..128].fill(0);
    data[..p.len()].copy_from_slice(p);
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut inventory = Image::load(file, 0, 64 * 1024 * 1024)?;
    inventory.relocate(|_| None)?;
    for (name, address) in [
        ("opendir", 0x26fb98),
        ("readdir", 0x26fba8),
        ("closedir", 0x26fba0),
    ] {
        assert!(
            inventory
                .unresolved
                .iter()
                .any(|i| i.name == name && i.address == address),
            "Missing named import {name}"
        );
    }
    let mut fixture = memory_fixture();
    let code = [
        0x4c, 0x8b, 0x5f, 24, 0x48, 0x8b, 0x47, 32, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8,
        0x48, 0x8b, 0x3f, 0x48, 0x83, 0xec, 8, 0x41, 0xff, 0xd3, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    let mut kernel = Kernel::new(4096, ERRNO)?;
    kernel.files.allow_create_directory(b"/dir")?;
    for i in 0..100 {
        kernel
            .files
            .register(format!("/dir/n{i:03}").as_bytes(), vec![i as u8])?;
    }
    n.kernel = Some(kernel);
    for (i, service) in [
        Service::Opendir,
        Service::Readdir,
        Service::Closedir,
        Service::Memcpy,
        Service::Syscall,
        Service::Free,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(SERVICE + 16 * i as u64, service)?;
    }
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
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
    let mut data = [0u8; 512];
    let mut errno = 77u32.to_le_bytes();
    let baseline = n.bytes_allocated();
    path(&mut data, b"/dir");
    let handle = invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, 0, 0, SERVICE, 0],
        &mut data,
        Some((&mut errno, false)),
    )?;
    assert!(handle > u32::MAX as u64);
    assert!(n.bytes_allocated() > baseline);
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 80, 0],
            &mut data,
            None
        )
        .is_err()
    );
    let mut names = BTreeSet::new();
    let mut pointers = Vec::new();
    loop {
        let pointer = invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 16, 0],
            &mut data,
            Some((&mut errno, false)),
        )?;
        if pointer == 0 {
            break;
        }
        pointers.push(pointer);
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 256, pointer, 8, SERVICE + 48, 0],
            &mut data,
            None,
        )?;
        let length = u16::from_le_bytes(data[260..262].try_into().unwrap()) as usize;
        assert!(length >= 12 && length <= 256 && length % 4 == 0);
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 256, pointer, length as u64, SERVICE + 48, 0],
            &mut data,
            None,
        )?;
        let namelen = data[263] as usize;
        assert_eq!(data[264 + namelen], 0);
        assert!(names.insert(data[264..264 + namelen].to_vec()));
        assert!(pointers.len() <= 102);
    }
    assert_eq!(names.len(), 102);
    assert!(names.contains(b".".as_slice()));
    assert!(names.contains(b"..".as_slice()));
    for i in 0..100 {
        assert!(names.contains(format!("n{i:03}").as_bytes()));
    }
    assert!(
        pointers.windows(2).any(|p| p[1] < p[0]),
        "Expected batch buffer reuse"
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 16, 0],
            &mut data,
            Some((&mut errno, false))
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 32, 0],
            &mut data,
            Some((&mut errno, false))
        )?,
        0
    );
    assert_eq!(n.bytes_allocated(), baseline);
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 16, 0],
            &mut data,
            None
        )
        .is_err()
    );
    for (p, error) in [(b"/missing".as_slice(), 2u32), (b"/dir/n000", 20)] {
        path(&mut data, p);
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [DATA, 0, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            0
        );
        assert_eq!(errno, error.to_le_bytes());
        assert_eq!(n.bytes_allocated(), baseline);
    }
    // Buffered records survive external fd closure; closing a stale stream must
    // not close the new regular file which reused that descriptor number.
    path(&mut data, b"/dir");
    let handle = invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, 0, 0, SERVICE, 0],
        &mut data,
        None,
    )?;
    let fd = (3..259u64)
        .find(|&fd| {
            n.kernel.as_ref().unwrap().descriptors.kind(fd)
                == Some(native_ps5::descriptors::DescriptorKind::Directory)
        })
        .ok_or("Missing internal directory descriptor")?;
    assert_ne!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 16, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [fd, 0, 0, SERVICE + 64, 6],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    path(&mut data, b"/dir/n000");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 0, SERVICE + 64, 5],
            &mut data,
            Some((&mut errno, true))
        )?,
        fd
    );
    assert_ne!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 16, 0],
            &mut data,
            None
        )?,
        0
    );
    let allocated = n.bytes_allocated();
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 32, 0],
            &mut data,
            Some((&mut errno, false))
        )
        .is_err()
    );
    assert_eq!(n.bytes_allocated(), allocated);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [handle, 0, 0, SERVICE + 32, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 9u32.to_le_bytes());
    assert_eq!(n.bytes_allocated(), baseline);
    assert_eq!(
        n.kernel.as_ref().unwrap().descriptors.kind(fd),
        Some(native_ps5::descriptors::DescriptorKind::RegisteredFile)
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [fd, 0, 0, SERVICE + 64, 6],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    println!(
        "PASS: native owned opendir/readdir/closedir, 102 dirents across reused batches, exact names/record bounds, EOF errno preservation, runtime heap teardown/free guard, typed open errors, cached entries after fd close, stale close errno guard and fd-reuse ownership; opaque virtual DIR profile"
    );
    Ok(())
}
