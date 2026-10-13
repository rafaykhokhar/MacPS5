//! __mb_cur_max is a writable four-byte data export, not a service address.
//! Executes both actual pinned accessors, data writes and locale transitions.
use native_ps5::{
    guest_locale::MB_CUR_MAX_ADDR,
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const TABLE: u64 = 0x8a000000;
const NAME: u64 = 0x1_8b000001;
const SETLOCALE: u64 = 0x8c0000;
fn invoke(
    n: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    entry: u64,
    args: [u64; 4],
    name: &[u8],
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut data = name.to_vec();
    let before = n.bytes_allocated();
    let mut regions = vec![DataRegion {
        address: TABLE,
        bytes: &mut table,
        writable: false,
    }];
    if !data.is_empty() {
        regions.push(DataRegion {
            address: NAME,
            bytes: &mut data,
            writable: false,
        });
    }
    let result = n.run_worker_with_data(adapter, entry, TABLE, &mut [real], &mut regions);
    drop(regions);
    assert_eq!(data, name);
    assert_eq!(n.bytes_allocated(), before);
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("mb_cur_max fixture suspended".into()),
    }
}
fn read(n: &mut NativeExecutor, a: &mut Image, r: &mut Image, expected: u64) -> Result<(), String> {
    for target in [BIAS + 0x1163c0, BIAS + 0x1163d0] {
        assert_eq!(
            invoke(n, a, r, 0x100100, [u64::MAX, u64::MAX, 0, target], &[])?,
            expected
        );
    }
    assert_eq!(
        invoke(n, a, r, 0x100300, [MB_CUR_MAX_ADDR, 0, 0, 0], &[])?,
        expected
    );
    Ok(())
}
fn write(n: &mut NativeExecutor, a: &mut Image, r: &mut Image, value: u64) -> Result<(), String> {
    assert_eq!(
        invoke(n, a, r, 0x100200, [MB_CUR_MAX_ADDR, value, 0, 0], &[])?,
        0
    );
    read(n, a, r, value as u32 as u64)
}
fn setup(
    file: &[u8],
    archive: &Path,
    directory: &Path,
) -> Result<(NativeExecutor, Image, Image), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    fixture.resize(0x600, 0);
    fixture[56..58].copy_from_slice(&1u16.to_le_bytes());
    fixture[96..104].copy_from_slice(&0x600u64.to_le_bytes());
    fixture[104..112].copy_from_slice(&0x600u64.to_le_bytes());
    let call = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48, 0x83, 0xec, 8, 0xff,
        0xd0, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    fixture[0x100..0x100 + call.len()].copy_from_slice(&call);
    let store = [
        0x48, 0x8b, 0x07, 0x48, 0x8b, 0x57, 8, 0x89, 0x10, 0x31, 0xc0, 0xc3,
    ];
    fixture[0x200..0x200 + store.len()].copy_from_slice(&store);
    let load = [0x48, 0x8b, 0x07, 0x8b, 0x00, 0xc3];
    fixture[0x300..0x300 + load.len()].copy_from_slice(&load);
    let store64 = [
        0x48, 0x8b, 0x07, 0x48, 0x8b, 0x57, 8, 0x48, 0x89, 0x10, 0x31, 0xc0, 0xc3,
    ];
    fixture[0x400..0x400 + store64.len()].copy_from_slice(&store64);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?; // Intentionally no Setlocale/kernel registration yet.
    let adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb18;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing __mb_cur_max object GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&MB_CUR_MAX_ADDR.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100, 0x200, 0x300, 0x400]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1163c0, 0x1163d0]),
            1000,
            archive,
            &directory.join("real-accessors"),
        )?;
    }
    Ok((n, adapter, real))
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    let (mut n, mut a, mut r) = setup(file, archive, &directory.join("first"))?;
    read(&mut n, &mut a, &mut r, 1)?;
    for value in [0, 7, 0x80000000, 0xffffffff, 0xdeadbeef12345678] {
        write(&mut n, &mut a, &mut r, value)?;
    }
    let (mut second, mut sa, mut sr) = setup(file, archive, &directory.join("second"))?;
    read(&mut second, &mut sa, &mut sr, 1)?;
    read(&mut n, &mut a, &mut r, 0x12345678)?;
    write(&mut second, &mut sa, &mut sr, 99)?;
    read(&mut n, &mut a, &mut r, 0x12345678)?;
    // Full-width cross-boundary writes fault atomically; neighbouring reads do not
    // accidentally expose an eight-byte or pointer-sized exported object.
    for (entry, address) in [
        (0x100200, MB_CUR_MAX_ADDR + 1),
        (0x100200, MB_CUR_MAX_ADDR - 1),
        (0x100400, MB_CUR_MAX_ADDR),
    ] {
        assert!(invoke(&mut n, &mut a, &mut r, entry, [address, 0, 0, 0], &[]).is_err());
        read(&mut n, &mut a, &mut r, 0x12345678)?;
    }
    for address in [MB_CUR_MAX_ADDR + 1, MB_CUR_MAX_ADDR + 4, u64::MAX] {
        assert!(invoke(&mut n, &mut a, &mut r, 0x100300, [address, 0, 0, 0], &[]).is_err());
    }
    n.register_service(SETLOCALE, Service::Setlocale)?;
    // Source loadlocale compares exact current/requested names. Queries and
    // unchanged CTYPE names preserve writes; real C <-> POSIX changes reset1.
    for (category, name, nonnull, expected) in [
        (2, b"".as_slice(), true, 0x12345678), // null-pointer query selected below
        (2, b"C\0".as_slice(), true, 0x12345678),
        (1, b"POSIX\0".as_slice(), true, 0x12345678),
        (2, b"unavailable-locale\0".as_slice(), false, 0x12345678),
        (2, b"POSIX\0".as_slice(), true, 1),
    ] {
        let value = invoke(
            &mut n,
            &mut a,
            &mut r,
            0x100100,
            [
                category,
                if name.is_empty() { 0 } else { NAME },
                0,
                SETLOCALE,
            ],
            name,
        )?;
        assert_eq!(value != 0, nonnull);
        read(&mut n, &mut a, &mut r, expected)?;
    }
    write(&mut n, &mut a, &mut r, 23)?;
    assert_ne!(
        invoke(
            &mut n,
            &mut a,
            &mut r,
            0x100100,
            [2, NAME, 0, SETLOCALE],
            b"POSIX\0"
        )?,
        0
    );
    read(&mut n, &mut a, &mut r, 23)?;
    assert_ne!(
        invoke(&mut n, &mut a, &mut r, 0x100100, [2, 0, 0, SETLOCALE], &[])?,
        0
    );
    read(&mut n, &mut a, &mut r, 23)?;
    assert_ne!(
        invoke(
            &mut n,
            &mut a,
            &mut r,
            0x100100,
            [0, NAME, 0, SETLOCALE],
            b"C\0"
        )?,
        0
    );
    read(&mut n, &mut a, &mut r, 1)?;
    read(&mut second, &mut sa, &mut sr, 99)?;
    println!(
        "PASS: __mb_cur_max owned writable4-byte data, both actualaccessors, low32 loads/stores, persistence/separateexecutor isolation, exactobject guards, query/same-name preservation and real C/POSIX CTYPE transitions resetting1; no service alias or protectedconstant substitution"
    );
    Ok(())
}
