//! Real guest mkdir wrapper and memory-backed directory/file namespace integration.
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
        _ => Err("mkdir fixture suspended".into()),
    }
}
fn path(data: &mut [u8], p: &[u8]) {
    data[..128].fill(0);
    data[..p.len()].copy_from_slice(p);
}
fn word(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    // Load target into r11, syscall number into rax, then the three SysV arguments.
    let code = [
        0x4c, 0x8b, 0x5f, 24, 0x48, 0x8b, 0x47, 32, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8,
        0x48, 0x8b, 0x3f, 0x41, 0xff, 0xd3, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    let mut kernel = Kernel::new(4096, ERRNO)?;
    let mut profile = kernel.files.profile();
    profile.umask = 0o027;
    kernel.files.configure_profile(profile)?;
    kernel.files.allow_create_directory(b"/save")?;
    kernel
        .files
        .register(b"/readonly/asset", b"asset".to_vec())?;
    kernel.register_working_directory(b"/save")?;
    n.kernel = Some(kernel);
    for (i, s) in [Service::Mkdir, Service::Stat, Service::Syscall]
        .into_iter()
        .enumerate()
    {
        n.register_service(SERVICE + 16 * i as u64, s)?;
    }
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fa70;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing mkdir GOT")?;
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
            BTreeSet::from([0x1184c0]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut data = [0xa5; 512];
    let mut errno = 77u32.to_le_bytes();
    // Real wrapper supplies0755; configured umask027 yields0750.
    path(&mut data, b"/save/game");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, BIAS + 0x1184c0, 0],
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
            [DATA, DATA + 256, 0, SERVICE + 16, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    assert_eq!(
        u16::from_le_bytes(data[264..266].try_into().unwrap()),
        0o40750
    );
    assert_eq!(word(&data, 268), 1000);
    assert_eq!(word(&data, 272), 1000);
    let inode = word(&data, 260);
    assert_ne!(inode, 0);
    // Relative paths traverse the real node; high mode bits do not become permissions.
    path(&mut data, b"game/./slot");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0x1_0000_01ff, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, false))
        )?,
        0
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    path(&mut data, b"game/slot/../slot/save.bin");
    let fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0x202, 0o666, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert!(fd >= 3 && fd < 100);
    data[128..136].copy_from_slice(b"save123\n");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, DATA + 128, 8, SERVICE + 32, 4],
            &mut data,
            Some((&mut errno, true))
        )?,
        8
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, 0, 0, SERVICE + 32, 6],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    let fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0, 0, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert!(fd >= 3 && fd < 100);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, DATA + 144, 32, SERVICE + 32, 3],
            &mut data,
            Some((&mut errno, true))
        )?,
        8
    );
    assert_eq!(&data[144..152], b"save123\n");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, 0, 0, SERVICE + 32, 6],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    // Stable directory identity after file creation and normalized traversal.
    path(&mut data, b"game/slot/..");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA + 256, 0, SERVICE + 16, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        0
    );
    assert_eq!(word(&data, 260), inode);
    for (p, error) in [
        (b"/save/game".as_slice(), 17u32),
        (b"/save/missing/child", 2),
        (b"/save/game/slot/save.bin/child", 20),
        (b"/readonly/new", 30),
        (b"", 2),
    ] {
        path(&mut data, p);
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 0o777, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, error.to_le_bytes());
    }
    path(&mut data, b"/save/blocked");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0o600, 0, SERVICE, 0],
            &mut data,
            None
        )?,
        0
    );
    path(&mut data, b"/save/blocked/child");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0o755, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 13u32.to_le_bytes());
    // Invalid path and short/readonly errno have no node or caller-output writes.
    for address in [0, u64::MAX, DATA + 512] {
        errno = 77u32.to_le_bytes();
        let before = data;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [address, 0o755, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, 14u32.to_le_bytes());
        assert_eq!(data, before);
    }
    path(&mut data, b"/save/game");
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0o755, 0, SERVICE, 0],
            &mut data,
            Some((&mut errno, false))
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0o755, 0, SERVICE, 0],
            &mut data,
            Some((&mut short, true))
        )
        .is_err()
    );
    assert_eq!(short, [0xa5; 3]);
    // Error publication must not overwrite the active service return slot.
    let mut frame = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    let mut frame_kernel = Kernel::new(4096, 0x20000000 + 65536 - 16)?;
    frame_kernel.files.allow_create_directory(b"/save")?;
    frame.kernel = Some(frame_kernel);
    frame.register_service(SERVICE, Service::Mkdir)?;
    unsafe {
        frame.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("frame"),
        )?;
    }
    path(&mut data, b"/save");
    let before = data;
    let error = invoke(
        &mut frame,
        &mut a,
        &mut real,
        [DATA, 0o755, 0, SERVICE, 0],
        &mut data,
        None,
    )
    .unwrap_err();
    assert!(
        error.contains("return"),
        "Expected return-frame guard: {error}"
    );
    assert_eq!(data, before);
    println!(
        "Native actual M_MakeDirectory, directory stat identity/mode, relative create/write/reopen/read, namespace errors and errno guards passed; virtual memory-backed files only"
    );
    Ok(())
}
