//! Standalone synthetic CPU benchmark; no retail-game performance inference.
use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, TranslationCacheUsage, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};

fn main() {
    assert!(cfg!(all(target_os = "macos", target_arch = "aarch64")));
    let archive =
        PathBuf::from(std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").expect("runtime archive"));
    let parent = PathBuf::from(
        std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").expect("fresh output directory"),
    );
    // Refuse to reuse or remove any existing directory, even if empty.
    std::fs::create_dir(&parent).expect("benchmark directory must not exist");
    let mut file = memory_fixture();
    // xor eax,eax; mov ecx,10000; add eax,ecx; dec ecx; jne loop; ret.
    // Independent sum oracle: 10000 * 10001 / 2 = 50005000.
    file[0x100..0x10e].copy_from_slice(&[
        0x31, 0xc0, 0xb9, 0x10, 0x27, 0, 0, 0x01, 0xc8, 0xff, 0xc9, 0x75, 0xfa, 0xc3,
    ]);
    file[0x140..0x146].copy_from_slice(&[0xe8, 0xbb, 0xff, 0xff, 0xff, 0xc3]);
    println!(
        "sample,compile_ns,extend_ns,first_run_ns,warm_total_ns,warm_runs,guest_instructions_per_run,mapped_bytes,libraries"
    );
    for sample in 0..5 {
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100_000, 100_000).unwrap();
        let start = Instant::now();
        // SAFETY: trusted matching runtime archive; executor is quiescent.
        unsafe {
            executor
                .compile_image(
                    &file,
                    image.bias,
                    BTreeSet::from([0x100]),
                    100,
                    &archive,
                    &parent,
                )
                .unwrap();
        }
        let compile_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(50_005_000)
        ));
        let first_run_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        // SAFETY: new CALL/RET batch shares the same live immutable image.
        unsafe {
            executor
                .extend_image_translation(&image, 0x100140, &archive, &parent)
                .unwrap();
        }
        let extend_ns = start.elapsed().as_nanos();
        // Include host dispatch and result validation in every measured run.
        let start = Instant::now();
        for _ in 0..100 {
            assert!(matches!(
                executor
                    .run(&mut image, 0x100140, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(50_005_000)
            ));
        }
        let warm_total_ns = start.elapsed().as_nanos();
        let usage = executor.translation_cache_usage();
        // 2 initial instructions + 3 * 10000 loop instructions + RET,
        // plus CALL and RET in the extension = 30005 guest instructions.
        println!(
            "{sample},{compile_ns},{extend_ns},{first_run_ns},{warm_total_ns},100,30005,{},{}",
            usage.mapped_bytes, usage.libraries
        );
        executor.remove_image_translations(image.bias);
        assert_eq!(
            executor.translation_cache_usage(),
            TranslationCacheUsage::default()
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
    }
    std::fs::remove_dir(parent).unwrap();
}
