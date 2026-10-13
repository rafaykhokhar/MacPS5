//! Real guest rename wrapper and memory-backed directory/file namespace integration.
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
        _ => Err("rename fixture suspended".into()),
    }
}
fn path(data: &mut [u8], p: &[u8]) {
    data[..128].fill(0);
    data[..p.len()].copy_from_slice(p);
}
fn paths(data: &mut [u8], old: &[u8], new: &[u8]) {
    path(data, old);
    data[128..256].fill(0);
    data[128..128 + new.len()].copy_from_slice(new);
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
    for dir in [
        b"/save".as_slice(),
        b"/save/tree",
        b"/save/tree/sub",
        b"/save/nonempty",
    ] {
        kernel.files.allow_create_directory(dir)?;
    }
    for (path, bytes) in [
        (b"/save/src".as_slice(), b"src".as_slice()),
        (b"/save/dst", b"dst"),
        (b"/save/tree/sub/file", b"sub"),
        (b"/save/nonempty/child", b"x"),
    ] {
        kernel.files.register_writable(path, bytes.to_vec())?;
    }
    kernel
        .files
        .register(b"/readonly/asset", b"asset".to_vec())?;
    kernel.register_working_directory(b"/save/tree/sub")?;
    n.kernel = Some(kernel);
    for (i, s) in [
        Service::Rename,
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
    let got = BIAS + 0x26fb58;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing rename GOT")?;
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
            BTreeSet::from([0x118490]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut data = [0xa5; 512];
    let mut errno = 77u32.to_le_bytes();
    path(&mut data, b"/save/src");
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
    let source_inode = word(&data, 260);
    let source_fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0, 0, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    path(&mut data, b"/save/dst");
    let destination_fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0, 0, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    paths(&mut data, b"/save/src", b"/save/dst");
    let before = data;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA + 128, 0, BIAS + 0x118490, 0],
            &mut data,
            Some((&mut errno, false))
        )?,
        0
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(data, before);
    path(&mut data, b"/save/dst");
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
    assert_eq!(word(&data, 260), source_inode);
    for (fd, expected, links) in [(source_fd, b"src", 1u16), (destination_fd, b"dst", 0u16)] {
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [fd, DATA + 300, 3, SERVICE + 32, 3],
                &mut data,
                Some((&mut errno, true))
            )?,
            3
        );
        assert_eq!(&data[300..303], expected);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [fd, DATA + 256, 0, SERVICE + 32, 189],
                &mut data,
                Some((&mut errno, true))
            )?,
            0
        );
        assert_eq!(&data[266..268], &links.to_le_bytes());
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
    // Directory move rewrites cwd under the source and preserves child lookup.
    paths(&mut data, b"/save/tree", b"/save/moved");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA + 128, 0, SERVICE, 0],
            &mut data,
            None
        )?,
        0
    );
    assert_eq!(
        n.kernel.as_ref().unwrap().working_directory_bytes(),
        Some(b"/save/moved/sub\0".as_slice())
    );
    path(&mut data, b"file");
    let fd = invoke(
        &mut n,
        &mut a,
        &mut real,
        [DATA, 0, 0, SERVICE + 32, 5],
        &mut data,
        Some((&mut errno, true)),
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [fd, DATA + 300, 3, SERVICE + 32, 3],
            &mut data,
            Some((&mut errno, true))
        )?,
        3
    );
    assert_eq!(&data[300..303], b"sub");
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
    for (old, new, error) in [
        (b"/save/missing".as_slice(), b"/save/new".as_slice(), 2u32),
        (b"/save/dst", b"/save/nonempty", 21),
        (b"/save/nonempty", b"/save/dst", 20),
        (b"/save/moved", b"/save/nonempty", 66),
        (b"/save/moved", b"/save/moved/sub/new", 22),
        (b"/save/dst", b"/readonly/new", 30),
        (b"/readonly/asset", b"/save/asset", 30),
    ] {
        paths(&mut data, old, new);
        let before = data;
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, DATA + 128, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, error.to_le_bytes());
        assert_eq!(data, before);
    }
    for (old, new) in [(0, DATA + 128), (DATA, u64::MAX)] {
        paths(&mut data, b"/save/dst", b"/save/new");
        errno = 77u32.to_le_bytes();
        let before = data;
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [old, new, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno, true))
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, 14u32.to_le_bytes());
        assert_eq!(data, before);
    }
    paths(&mut data, b"/save/missing", b"/save/new");
    for writable in [false, true] {
        errno = 77u32.to_le_bytes();
        let len = if writable { 3 } else { 4 };
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut a,
                &mut real,
                [DATA, DATA + 128, 0, SERVICE, 0],
                &mut data,
                Some((&mut errno[..len], writable))
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    // Both pathname arguments may alias and succeed without namespace mutation.
    path(&mut data, b"/save/dst");
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA, 0, BIAS + 0x118490, 0],
            &mut data,
            None
        )?,
        0
    );
    // Alias errno to the old pathname after all namespace checks above.
    let mut alias = Kernel::new(4096, DATA)?;
    alias.files.allow_create_directory(b"/save")?;
    alias.files.register_writable(b"/save/a", b"a".to_vec())?;
    n.kernel = Some(alias);
    paths(&mut data, b"/save/a", b"/save/b");
    let before = data;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            &mut real,
            [DATA, DATA + 128, 0, BIAS + 0x118490, 0],
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
            [DATA, DATA + 128, 0, BIAS + 0x118490, 0],
            &mut data,
            None
        )?,
        u32::MAX as u64
    );
    assert_eq!(&data[..4], &2u32.to_le_bytes());
    assert_eq!(&data[4..], &before[4..]);
    println!(
        "PASS: actual M_rename/direct ABI, inode-preserving replacement and open replaced destination/nlink, subtree/cwd rewrite and relative reopen, low32 errno failures, type/nonempty/selfdescendant/readonly guards, pathname/errno aliases"
    );
    Ok(())
}
