//! Actual pinned SDL twelve-thread tiler wrapper, driven by the guest scheduler.
//! The only synthetic code is an ABI adapter loading its four arguments; neither
//! the wrapper nor worker instructions or partitioning are replaced.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{NativeExecutor, Service, memory_fixture},
    scheduler::{Scheduler, Step},
    video_tile,
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const ADAPTER: u64 = 0x40000000;
const ARGS: u64 = 0x30000000;
const SOURCE: u64 = 0x31000000;
const DEST: u64 = 0x32000000;
fn bind(image: &mut Image, offset: u64, name: &str, target: u64) {
    assert!(
        image
            .unresolved
            .iter()
            .any(|i| i.address == BIAS + offset && i.name.starts_with(name)),
        "Pinned import{name}"
    );
    let address = BIAS + offset;
    let segment = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .expect("GOT mapping");
    let at = (address - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&target.to_le_bytes());
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut native = NativeExecutor::new(0x20000000, 2 * 1024 * 1024, 10_000_000, 10_000_000)?;
    native.register_service(0x780000, Service::ThreadCreate)?;
    native.register_service(0x780010, Service::ThreadJoin)?;
    let mut adapter_file = memory_fixture();
    // mov rcx,[rdi+24]; mov rdx,[rdi+16]; mov rsi,[rdi+8]; mov rdi,[rdi]
    let mut code = vec![
        0x48, 0x8b, 0x4f, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48,
        0xb8,
    ];
    code.extend((BIAS + 0xc3a30).to_le_bytes());
    code.extend([0xff, 0xe0]);
    adapter_file[0x100..0x100 + code.len()].copy_from_slice(&code);
    // SAFETY: supplied archive is the matching trusted locally built runtime.
    unsafe {
        let manifest = native.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xc3a30, 0xc3f20]),
            1024,
            archive,
            directory,
        )?;
        assert_eq!(
            manifest.sha256,
            "308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567"
        );
        assert!(manifest.untranslated.is_empty());
        assert_eq!(manifest.instruction_count, 290);
        native.compile_image(
            &adapter_file,
            ADAPTER,
            BTreeSet::from([0x100]),
            1024,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    image.relocate(|_| None)?;
    bind(&mut image, 0x26f760, "pthread_create", 0x780000);
    bind(&mut image, 0x26f7b8, "pthread_join", 0x780010);
    let mut adapter = Image::load(&adapter_file, ADAPTER, 65536)?;
    let width = 640u32;
    let height = 129u32;
    let mut source = Vec::new();
    for index in 0..width * height {
        source.extend((index.wrapping_mul(0x9e3779b9) ^ 0x12345678).to_le_bytes());
    }
    let expected = source.clone();
    let required = video_tile::required_bytes(width, height)?;
    let mut destination = vec![0xa5; required + 16];
    let mut arguments = [0u8; 32];
    for (i, value) in [SOURCE, DEST, width as u64, height as u64]
        .into_iter()
        .enumerate()
    {
        arguments[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
    }
    let before = arguments;
    let mut scheduler = Scheduler::new(4096, 10_000, 10_000_000)?;
    scheduler.enqueue(&mut native, 2, ADAPTER + 0x100, ARGS)?;
    let mut completed = BTreeSet::new();
    let mut services = 0;
    let mut joins = 0;
    let mut finished = false;
    for _ in 0..10_000 {
        let mut mappings = [
            DataRegion {
                address: ARGS,
                bytes: &mut arguments,
                writable: false,
            },
            DataRegion {
                address: SOURCE,
                bytes: &mut source,
                writable: false,
            },
            DataRegion {
                address: DEST,
                bytes: &mut destination,
                writable: true,
            },
        ];
        match scheduler.step(&mut native, &mut image, &mut [&mut adapter], &mut mappings)? {
            Step::Finished => {
                finished = true;
                break;
            }
            Step::Completed { thread, value } => {
                assert_eq!(value, 0);
                assert!(completed.insert(thread));
            }
            Step::ThreadService(_) => services += 1,
            Step::JoinPending(_) => joins += 1,
            Step::Yielded(_) | Step::ServiceBoundary(_) => {}
            other => return Err(format!("Unexpected actual tiler boundary{other:?}")),
        }
    }
    assert!(
        finished,
        "Scheduler budget must cover complete wrapper/worker run"
    );
    assert_eq!(completed, BTreeSet::from_iter(2..=14));
    assert_eq!(scheduler.result(2), Some(0));
    for thread in 3..=14 {
        assert_eq!(
            scheduler.result(thread),
            None,
            "All actual children must be joined/reaped"
        );
    }
    assert_eq!(native.tls.live_threads(), 1);
    assert_eq!(arguments, before);
    assert_eq!(source, expected);
    assert_eq!(&destination[required..], &[0xa5; 16]);
    assert_eq!(
        video_tile::untile_rgba(&destination[..required], width, height)?,
        expected
    );
    println!(
        "PASS: actual pinned PS5_DrawPixelsAsTiles242instructions creates/joins12 real48instruction workers; {}pixels fullymatch independentuntile, parentstack descriptors retained, childrenreaped, {services}services/{joins}joinwaits; no syntheticwrapper or gameplayclaim",
        width * height
    );
    Ok(())
}
