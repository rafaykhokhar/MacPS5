//! Execute the pinned PS5_DrawTileThread machine code on ARM64. Host invokes
//! worker ranges directly; this does not claim pthread_create/join or gameplay.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor},
    video_tile,
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const ENTRY: u64 = BIAS + 0xc3f20;
const DESC: u64 = 0x30000000;
const SOURCE: u64 = 0x31000000;
const DEST: u64 = 0x32000000;
fn descriptor(width: u16, start: u32, end: u64) -> [u8; 40] {
    let mut bytes = [0u8; 40];
    bytes[..8].copy_from_slice(&SOURCE.to_le_bytes());
    bytes[8..16].copy_from_slice(&DEST.to_le_bytes());
    bytes[16..18].copy_from_slice(&width.to_le_bytes());
    bytes[24..28].copy_from_slice(&start.to_le_bytes());
    bytes[32..40].copy_from_slice(&end.to_le_bytes());
    bytes
}
fn run(
    native: &mut NativeExecutor,
    image: &mut Image,
    desc: &mut [u8],
    source: &mut [u8],
    dest: &mut [u8],
    writable: bool,
) -> Result<(), String> {
    let mut data = [
        DataRegion {
            address: DESC,
            bytes: desc,
            writable: false,
        },
        DataRegion {
            address: SOURCE,
            bytes: source,
            writable: false,
        },
        DataRegion {
            address: DEST,
            bytes: dest,
            writable,
        },
    ];
    match native.run_worker_with_data(image, ENTRY, DESC, &mut [], &mut data)? {
        Execution::WorkerReturned(0) => Ok(()),
        _ => Err("Real tiler did not return NULL through worker ABI".into()),
    }
}
fn read(image: &Image, address: u64, size: usize) -> &[u8] {
    let segment = image
        .segments
        .iter()
        .find(|s| address >= s.address && address + size as u64 <= s.address + s.bytes.len() as u64)
        .expect("Pinned data mapped");
    let offset = (address - segment.address) as usize;
    &segment.bytes[offset..offset + size]
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut native = NativeExecutor::new(0x20000000, 65536, 1_000_000, 1_000_000)?;
    // SAFETY: supplied archive must match this locally compiled Rust runtime.
    let manifest = unsafe {
        native.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xc3f20]),
            1024,
            archive,
            directory,
        )?
    };
    if manifest.sha256 != "308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567" {
        return Err("Tiler verifier requires pinned Crispy Doom ELF".into());
    }
    assert!(
        manifest.untranslated.is_empty(),
        "Actual worker must have no untranslated boundaries"
    );
    assert_eq!(manifest.instruction_count, 48);
    let mut image = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    image.relocate(|_| None)?;
    assert_eq!(
        read(&image, BIAS + 0x291c60, 8),
        &0x3f60_0000_0000_0000u64.to_le_bytes()
    );
    assert_eq!(
        read(&image, BIAS + 0x291b78, 8),
        &0x40f0_0000_0000_0000u64.to_le_bytes()
    );
    let table_before = read(&image, BIAS + 0x534170, 131072).to_vec();
    let mut processed = 0;
    for width in [512u32, 640, 1920] {
        let height = 130u32;
        let pixels = (width * height) as usize;
        let mut source = Vec::with_capacity(pixels * 4);
        for index in 0..pixels {
            source.extend(((index as u32).wrapping_mul(0x9e3779b9) ^ 0x12345678).to_le_bytes());
        }
        let source_before = source.clone();
        let mut dest = vec![0xa5; video_tile::required_bytes(width, height)?];
        let mut expected = vec![0xa5; pixels * 4];
        let w = width as usize;
        let ranges = [
            (0, 4),
            (508, 516),
            (127 * w - 4, 127 * w + 4),
            (128 * w - 4, 128 * w + 4),
            (pixels - 4, pixels),
        ];
        for (start, end) in ranges {
            let mut desc = descriptor(width as u16, start as u32, end as u64);
            let before = desc;
            run(
                &mut native,
                &mut image,
                &mut desc,
                &mut source,
                &mut dest,
                true,
            )?;
            assert_eq!(desc, before);
            expected[start * 4..end * 4].copy_from_slice(&source[start * 4..end * 4]);
            processed += end - start;
        }
        assert_eq!(source, source_before);
        assert_eq!(
            video_tile::untile_rgba(&dest, width, height)?,
            expected,
            "Real guest worker output width{width}"
        );
    }
    // Empty range exits before width division, constants, source or pixel writes.
    let mut desc = descriptor(0, 0, 0);
    let mut source = [0x19; 3];
    let mut dest = [0xa5; 4];
    run(
        &mut native,
        &mut image,
        &mut desc,
        &mut source,
        &mut dest,
        false,
    )?;
    assert_eq!(dest, [0xa5; 4]);
    // First source load crosses the exact3byte mapping: no pixel is written.
    let mut desc = descriptor(512, 0, 1);
    let fault = run(
        &mut native,
        &mut image,
        &mut desc,
        &mut source,
        &mut dest,
        true,
    )
    .expect_err("Truncated pixel source must fault");
    assert!(fault.contains("fault 1 at 0xc3fc0"), "{fault}");
    assert_eq!(dest, [0xa5; 4]);
    let mut source = [0x19; 4];
    let fault = run(
        &mut native,
        &mut image,
        &mut desc,
        &mut source,
        &mut dest,
        false,
    )
    .expect_err("Readonly destination must fault");
    assert!(fault.contains("fault 2 at 0xc3fd4"), "{fault}");
    assert_eq!(dest, [0xa5; 4]);
    let mut short_dest = [0xa5; 3];
    let fault = run(
        &mut native,
        &mut image,
        &mut desc,
        &mut source,
        &mut short_dest,
        true,
    )
    .expect_err("Truncated pixel destination must fault");
    assert!(fault.contains("fault 2 at 0xc3fd4"), "{fault}");
    assert_eq!(short_dest, [0xa5; 3]);
    let mut desc = descriptor(0, 0, 1);
    let fault = run(
        &mut native,
        &mut image,
        &mut desc,
        &mut source,
        &mut dest,
        true,
    )
    .expect_err("Zero width must raise guest divide fault");
    assert!(fault.contains("fault 13 at 0xc3f58"), "{fault}");
    assert_eq!(dest, [0xa5; 4]);
    assert_eq!(read(&image, BIAS + 0x534170, 131072), table_before);
    println!(
        "PASS: actual pinned PS5_DrawTileThread48instructions, {processed} guest pixels across512/640/1920wide tile boundaries match independent untile oracle; source/table unchanged, emptyrange and source/destination/divide guards; direct host invocation, no pthread or gameplay claim"
    );
    Ok(())
}
