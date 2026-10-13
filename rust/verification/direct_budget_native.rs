use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{
        DirectTranslationLimits, DirectTranslationUsage, Execution, NativeExecutor, memory_fixture,
    },
    modules::Arguments,
};
use std::rc::Rc;
fn main() {
    unsafe {
        let arena = Rc::new(JitArena::new(2 * 1024 * 1024).unwrap());
        let mut file = memory_fixture();
        for i in 0..32 {
            file[0x100 + i * 5..0x105 + i * 5].copy_from_slice(&[0xe9, 0, 0, 0, 0]);
        }
        file[0x1a0..0x1a6].copy_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
        for (instructions, attempts, expected_error) in [
            (34, 33, None),
            (34, 32, Some("attempt budget")),
            (33, 33, Some("instruction budget")),
        ] {
            let mut image = Image::load(&file, 0x100000, 4096).unwrap();
            let mut executor = NativeExecutor::new(0x900000, 4096, 1000, 1000).unwrap();
            executor
                .register_direct_source_with_limits(
                    &image,
                    DirectTranslationLimits {
                        instructions,
                        attempts,
                    },
                    arena.clone(),
                )
                .unwrap();
            executor.set_automatic_translation(true);
            let result = executor.run(&mut image, 0x100100, Arguments::default(), &mut []);
            if let Some(fragment) = expected_error {
                let error = result.err().expect("budget");
                assert!(error.contains(fragment), "{error}");
            } else {
                assert!(matches!(result.unwrap(), Execution::Returned(42)));
                assert_eq!(
                    executor.direct_translation_usage(image.bias),
                    Some(DirectTranslationUsage {
                        instructions: 34,
                        attempts: 33
                    })
                );
                let used = arena.used();
                for _ in 0..10 {
                    assert!(matches!(
                        executor
                            .run(&mut image, 0x100100, Arguments::default(), &mut [])
                            .unwrap(),
                        Execution::Returned(42)
                    ));
                }
                assert_eq!(arena.used(), used);
                assert_eq!(
                    executor.direct_translation_usage(image.bias),
                    Some(DirectTranslationUsage {
                        instructions: 34,
                        attempts: 33
                    })
                );
            }
            let usage = executor.direct_translation_usage(image.bias).unwrap();
            assert!(usage.instructions <= instructions);
            assert!(usage.attempts <= attempts);
            executor.remove_image_translations(image.bias);
            assert!(executor.direct_translation_usage(image.bias).is_none());
        }
        let image = Image::load(&file, 0x200000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0xa00000, 4096, 1000, 1000).unwrap();
        for limits in [
            DirectTranslationLimits {
                instructions: 0,
                attempts: 1,
            },
            DirectTranslationLimits {
                instructions: 1,
                attempts: 0,
            },
            DirectTranslationLimits {
                instructions: 1_048_577,
                attempts: 1,
            },
            DirectTranslationLimits {
                instructions: 1,
                attempts: 65_537,
            },
        ] {
            assert!(
                executor
                    .register_direct_source_with_limits(&image, limits, arena.clone())
                    .is_err()
            );
            assert!(executor.direct_translation_usage(image.bias).is_none());
        }
        println!(
            "PASS: 33 on-demand spans beyond old rebuild cap, exact instruction/attempt limits, cached execution without budget/allocation growth, invalid-limit rejection and retirement"
        );
    }
}
