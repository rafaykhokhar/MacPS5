//! Real guest remove wrapper and memory-backed directory/file namespace integration.
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
        _ => Err("remove fixture suspended".into()),
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
    kernel
        .files
        .register_writable(b"/save/item", b"old".to_vec())?;
    kernel
        .files
        .register_directory(b"/save/empty", 0o777, false)?;
    kernel
        .files
        .register_directory(b"/save/nonempty", 0o777, false)?;
    kernel
        .files
        .register_writable(b"/save/nonempty/child", b"x".to_vec())?;
    kernel
        .files
        .register(b"/save/readonly-file", b"read-only bytes".to_vec())?;
    kernel.register_working_directory(b"/save")?;
    n.kernel = Some(kernel);
    for (i, s) in [
        Service::Remove,
        Service::Stat,
        Service::Syscall,
        Service::Mkdir,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(SERVICE + 16 * i as u64, s)?;
    }
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb50;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing remove GOT")?;
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
            BTreeSet::from([0x118480]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut data = [0xa5; 512];
    let mut errno = 77u32.to_le_bytes();
    path(&mut data, b"/save/item");
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
    let old_inode = word(&data, 260);
    let old_fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0, 0, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert!((3..100).contains(&old_fd));
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, BIAS + 0x118480, 0],
            &mut data,
            Some((&mut errno, false))
        )?,
        0
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA + 256, 0, SERVICE + 16, 0],
            &mut data,
            Some((&mut errno, true))
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 2u32.to_le_bytes());
    let new_fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0x202, 0o666, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert!((3..100).contains(&new_fd));
    assert_ne!(new_fd, old_fd);
    data[128..131].copy_from_slice(b"new");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [new_fd, DATA + 128, 3, SERVICE + 32, 4],
            &mut data,
            Some((&mut errno, true))
        )?,
        3
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [old_fd, DATA + 144, 3, SERVICE + 32, 3],
            &mut data,
            Some((&mut errno, true))
        )?,
        3
    );
    assert_eq!(&data[144..147], b"old");
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
    assert_ne!(word(&data, 260), old_inode);
    for fd in [old_fd, new_fd] {
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
    }
    for p in [
        b"/save/item".as_slice(),
        b"/save/empty",
        b"/save/readonly-file",
        b"/save/nonempty/child",
        b"/save/nonempty",
    ] {
        path(&mut data, p);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 0, 0, SERVICE, 0],
                &mut data,
                None
            )?,
            0
        );
    }
    // Recreate a nonempty directory for deterministic failure checks.
    path(&mut data, b"/save/dir");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0o777, 0, SERVICE + 48, 0],
            &mut data,
            None
        )?,
        0
    );
    path(&mut data, b"/save/dir/child");
    let fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0x202, 0o666, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
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
    for (p, error) in [
        (b"/save/dir".as_slice(), 66u32),
        (b"/save/missing", 2),
        (b"/readonly/asset", 30),
        (b"/save/dir/child/sub", 20),
        (b"/save/dir/child/", 20),
        (b"", 2),
    ] {
        path(&mut data, p);
        errno = 77u32.to_le_bytes();
        let before = data;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 0, 0, BIAS + 0x118480, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, error.to_le_bytes());
        assert_eq!(data, before);
    }
    for address in [0, u64::MAX, DATA + 512] {
        errno = 77u32.to_le_bytes();
        let before = data;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [address, 0, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, 14u32.to_le_bytes());
        assert_eq!(data, before);
    }
    // Error publication itself must be guarded. Namespace and caller data stay intact.
    path(&mut data, b"/save/dir");
    for writable in [false, true] {
        let before = data;
        errno = 77u32.to_le_bytes();
        let len = if writable { 3 } else { 4 };
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, 0, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno[..len], writable))
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
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
    // errno aliases the pathname: snapshot path before error publication.
    // A successful removal does not touch the aliased errno/path bytes.
    let mut alias_kernel = Kernel::new(4096, DATA)?;
    alias_kernel.files.allow_create_directory(b"/save")?;
    alias_kernel
        .files
        .register_writable(b"/save/alias", b"a".to_vec())?;
    n.kernel = Some(alias_kernel);
    path(&mut data, b"/save/alias");
    let before = data;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, BIAS + 0x118480, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(data, before);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, 0, 0, BIAS + 0x118480, 0],
            &mut data,
            None
        )?,
        u32::MAX as u64
    );
    assert_eq!(&data[..4], &2u32.to_le_bytes());
    assert_eq!(&data[4..], &before[4..]);
    println!(
        "PASS: actual M_remove tail wrapper/direct ABI, low32 errors/errno, open unlinked descriptor retains old bytes while recreated path has distinct inode, empty/nonempty directories, readonly/missing/path and errno guards; explicit guest namespace only"
    );
    Ok(())
}
