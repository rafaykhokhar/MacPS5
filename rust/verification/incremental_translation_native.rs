//! Standalone ARM64 verification of incremental translation publication.
use native_ps5::{loader::Image, module_executor::{Execution, NativeExecutor, TranslationCacheLimits, TranslationCacheUsage, memory_fixture}, modules::Arguments};
use std::{collections::{BTreeMap, BTreeSet}, path::Path};
fn artifacts(parent: &Path) -> BTreeMap<std::path::PathBuf, String> {
    std::fs::read_dir(parent).unwrap().map(|entry| {
        let path = entry.unwrap().path().join("module.S");
        let text = std::fs::read_to_string(&path).unwrap();
        (path, text)
    }).collect()
}
fn returned(n: &mut NativeExecutor, image: &mut Image, pc: u64, expected: i32) {
    assert!(matches!(n.run(image, pc, Arguments::default(), &mut []).unwrap(), Execution::Returned(value) if value == expected));
}
fn main() {
    let archive = std::path::PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").unwrap());
    let directory = std::path::PathBuf::from(std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0, "Use a fresh verifier directory");
    let mut file = memory_fixture();
    file[0x100..0x106].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
    // New region jumps into the old region's RET instruction.
    file[0x120..0x12a].copy_from_slice(&[0xb8, 7, 0, 0, 0, 0xe9, 0xdb, 0xff, 0xff, 0xff]);
    // New CALL reaches the first extension, then returns through both batches.
    file[0x140..0x146].copy_from_slice(&[0xe8, 0xdb, 0xff, 0xff, 0xff, 0xc3]);
    file[0x160..0x166].copy_from_slice(&[0xb8, 9, 0, 0, 0, 0xc3]);
    file[0x180..0x182].copy_from_slice(&[0x0f, 0x0b]);
    let mut image = Image::load(&file, 0x100000, 4096).unwrap();
    let mut n = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
    let default_limits = n.translation_cache_limits();
    n.set_translation_cache_limits(TranslationCacheLimits {mapped_bytes: 0, libraries: 0}).unwrap();
    assert!(unsafe { n.compile_image(&file, image.bias, BTreeSet::from([0x100]), 100, &archive, &directory) }.is_err());
    assert_eq!(n.translation_cache_usage(), TranslationCacheUsage::default());
    assert!(artifacts(&directory).is_empty());
    n.set_translation_cache_limits(default_limits).unwrap();
    // SAFETY: matching trusted archive and quiescent executor at every compile.
    unsafe { n.compile_image(&file, image.bias, BTreeSet::from([0x100]), 100, &archive, &directory).unwrap(); }
    returned(&mut n, &mut image, 0x100100, 42);
    let original = artifacts(&directory);
    assert_eq!(original.len(), 1);
    let generated = original.keys().next().unwrap().parent().unwrap();
    let symbols = std::process::Command::new("xcrun")
        .args(["nm", "-gjU"]).arg(generated.join("module.dylib"))
        .output().unwrap();
    assert!(symbols.status.success());
    assert_eq!(String::from_utf8(symbols.stdout).unwrap().trim(), "_module_aot_100",
        "Generated library exports only its block entry; runtime helpers stay private");
    let initial_usage = n.translation_cache_usage();
    assert_eq!(initial_usage.libraries, 1);
    assert!(initial_usage.mapped_bytes > 0);
    assert_eq!(initial_usage.mapped_bytes % 16384, 0);
    let defaults = n.translation_cache_limits();
    assert!(n.set_translation_cache_limits(TranslationCacheLimits {mapped_bytes: 0, libraries: 0}).is_err());
    assert_eq!(n.translation_cache_limits(), defaults);
    n.set_translation_cache_limits(TranslationCacheLimits {mapped_bytes: defaults.mapped_bytes, libraries: 1}).unwrap();
    assert!(unsafe { n.extend_image_translation(&image, 0x100120, &archive, &directory) }.err().unwrap().contains("library budget"));
    assert_eq!(n.translation_cache_usage(), initial_usage);
    assert_eq!(artifacts(&directory), original);
    n.set_translation_cache_limits(TranslationCacheLimits {mapped_bytes: initial_usage.mapped_bytes + 1, libraries: 2}).unwrap();
    assert!(unsafe { n.extend_image_translation(&image, 0x100120, &archive, &directory) }.err().unwrap().contains("mapped-byte budget"));
    assert_eq!(n.translation_cache_usage(), initial_usage);
    assert_eq!(artifacts(&directory), original);
    returned(&mut n, &mut image, 0x100100, 42);
    n.set_translation_cache_limits(defaults).unwrap();
    let first = unsafe { n.extend_image_translation(&image, 0x100120, &archive, &directory).unwrap() };
    assert_eq!(first.instruction_count, 4, "Public manifest retains union discovery");
    returned(&mut n, &mut image, 0x100120, 7);
    returned(&mut n, &mut image, 0x100100, 42);
    let second = artifacts(&directory);
    assert_eq!(second.len(), 2, "Old library remains pinned");
    assert_eq!(n.translation_cache_usage().libraries, 2);
    assert!(n.translation_cache_usage().mapped_bytes > initial_usage.mapped_bytes);
    for (path, text) in &original { assert_eq!(second.get(path), Some(text)); }
    let added = second.iter().find(|(path, _)| !original.contains_key(*path)).unwrap().1;
    assert!(added.contains("_module_aot_120:"));
    assert!(!added.contains("_module_aot_100:"));
    assert!(!added.contains("_module_aot_105:"));
    let next = unsafe { n.extend_image_translation(&image, 0x100140, &archive, &directory).unwrap() };
    assert_eq!(next.instruction_count, 6);
    returned(&mut n, &mut image, 0x100140, 7);
    let complete = artifacts(&directory);
    assert_eq!(complete.len(), 3);
    println!("CACHE MAPPINGS: initial={initial_usage:?}, three batches={:?}", n.translation_cache_usage());
    for (path, text) in &second { assert_eq!(complete.get(path), Some(text)); }
    let attempts = n.translation_rebuilds(image.bias);
    assert!(unsafe { n.extend_image_translation(&image, 0x100140, &archive, &directory) }.is_err());
    assert_eq!(n.translation_rebuilds(image.bias), attempts);
    for target in [0x100121, 0x100180] {
        assert!(unsafe { n.extend_image_translation(&image, target, &archive, &directory) }.is_err());
        assert_eq!(artifacts(&directory), complete, "Failed compilation must leave old code intact");
        returned(&mut n, &mut image, 0x100140, 7);
    }
    image.segments[0].bytes[0x100] = 0x90;
    assert!(unsafe { n.extend_image_translation(&image, 0x100160, &archive, &directory) }.is_err());
    image.segments[0].bytes[0x100] = file[0x100];
    assert_eq!(artifacts(&directory), complete);
    returned(&mut n, &mut image, 0x100100, 42);
    n.remove_image_translations(image.bias);
    assert!(artifacts(&directory).is_empty(), "All batches retire together");
    assert_eq!(n.translation_cache_usage(), TranslationCacheUsage::default());
    println!("PASS: incremental ARM64 batches retain old assembly/libraries, execute cross-batch interior JMP/CALL/RET, preserve full union manifests, enforce library/mapped-byte budgets without publication, reject overlap/unsupported/stale code atomically, and retire all batches with zero usage");
}
