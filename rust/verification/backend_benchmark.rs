//! Identical guest workload through direct and dylib backends. Synthetic only.
use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor, TranslationCacheUsage, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::PathBuf, time::Instant};
fn main() {
    let archive = PathBuf::from(
        std::env::var_os("NATIVE_PS5_RUNTIME_ARCHIVE").expect("matching runtime archive"),
    );
    let directory = PathBuf::from(
        std::env::var_os("NATIVE_PS5_AOT_DIRECTORY").expect("fresh output directory"),
    );
    std::fs::create_dir(&directory).expect("directory must not exist");
    // MOV EAX,0; MOV ECX,10000; ADD EAX,ECX; SUB ECX,1; JNE loop; RET.
    let bytes = [
        0xb8, 0, 0, 0, 0, 0xb9, 0x10, 0x27, 0, 0, 0x01, 0xc8, 0x83, 0xe9, 1, 0x75, 0xf9, 0xc3,
    ];
    let mut file = memory_fixture();
    file[0x100..0x100 + bytes.len()].copy_from_slice(&bytes);
    println!(
        "sample,backend,arena_allocate_ns,translate_publish_ns,first_run_ns,warm_total_ns,warm_runs,guest_instructions_per_run,mapped_bytes,direct_code_bytes"
    );
    for sample in 0..5 {
        // Alternate ordering to avoid always measuring one backend first.
        for direct in if sample % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let mut image = Image::load(&file, 0x100000, 4096).unwrap();
            let mut executor = NativeExecutor::new(0x900000, 4096, 100000, 100000).unwrap();
            let start = Instant::now();
            // SAFETY: standalone single host thread, no competing JIT engine.
            let arena = if direct {
                Some(unsafe { JitArena::new(16384).unwrap() })
            } else {
                None
            };
            let allocation_ns = if direct {
                start.elapsed().as_nanos()
            } else {
                0
            };
            let start = Instant::now();
            unsafe {
                if let Some(arena) = &arena {
                    executor
                        .register_direct_in_image(&image, 0x100, &bytes, 100, arena)
                        .unwrap();
                } else {
                    executor
                        .compile_image(
                            &file,
                            image.bias,
                            BTreeSet::from([0x100]),
                            100,
                            &archive,
                            &directory,
                        )
                        .unwrap();
                }
            }
            let translate_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            assert!(matches!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(50_005_000)
            ));
            let first_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            for _ in 0..100 {
                assert!(matches!(
                    executor
                        .run(&mut image, 0x100100, Arguments::default(), &mut [])
                        .unwrap(),
                    Execution::Returned(50_005_000)
                ));
            }
            let warm_ns = start.elapsed().as_nanos();
            let mapped = arena.as_ref().map_or_else(
                || executor.translation_cache_usage().mapped_bytes,
                JitArena::capacity,
            );
            let code_bytes = arena.as_ref().map_or(0, JitArena::used);
            println!(
                "{sample},{},{allocation_ns},{translate_ns},{first_ns},{warm_ns},100,30003,{mapped},{code_bytes}",
                if direct { "direct" } else { "dylib" }
            );
            executor.remove_image_translations(image.bias);
            assert_eq!(
                executor.translation_cache_usage(),
                TranslationCacheUsage::default()
            );
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
            drop(executor);
            drop(arena);
        }
    }
    std::fs::remove_dir(directory).unwrap();
}
