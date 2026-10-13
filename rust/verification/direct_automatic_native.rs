use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
use std::rc::Rc;
fn main() {
    unsafe {
        let arena = Rc::new(JitArena::new(1024 * 1024).unwrap());
        let mut file = memory_fixture();
        // Entry calls a previously undiscovered function. Its return continuation
        // then makes an indirect tail transfer into another undiscovered block.
        let call = [0xe8, 0x1b, 0, 0, 0];
        let mut resume = vec![0x48, 0xb9];
        resume.extend_from_slice(&0x100140u64.to_le_bytes());
        resume.extend_from_slice(&[0xff, 0xe1]);
        file[0x100..0x105].copy_from_slice(&call);
        file[0x105..0x111].copy_from_slice(&resume);
        file[0x120..0x126].copy_from_slice(&[0xb8, 40, 0, 0, 0, 0xc3]);
        file[0x140..0x144].copy_from_slice(&[0x83, 0xc0, 2, 0xc3]);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_source(&image, 100, arena.clone())
            .unwrap();
        assert!(
            executor
                .register_direct_source(&image, 7, arena.clone())
                .is_err()
        );
        executor.set_automatic_translation(true);
        for _ in 0..20 {
            assert!(matches!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(42)
            ));
        }
        assert_eq!(executor.translation_cache_usage().libraries, 0);
        // Enter a known instruction boundary inside a previously compiled span.
        assert!(matches!(
            executor
                .run(&mut image, 0x100143, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(0)
        ));
        // Instruction-interior decoding must be rejected before publication.
        let err = executor
            .run(&mut image, 0x100121, Arguments::default(), &mut [])
            .err()
            .expect("interior");
        assert!(err.contains("Overlapping"), "{err}");
        executor.remove_image_translations(image.bias);
        executor
            .register_direct_source(&image, 1, arena.clone())
            .unwrap();
        let err = executor
            .run(&mut image, 0x100100, Arguments::default(), &mut [])
            .err()
            .expect("budget");
        assert!(err.contains("instruction budget"), "{err}");
        executor.remove_image_translations(image.bias);
        // Source snapshot and live code must agree before publication.
        executor
            .register_direct_source(&image, 7, arena.clone())
            .unwrap();
        let segment = image
            .segments
            .iter_mut()
            .find(|s| s.address <= 0x100100 && 0x100100 - s.address < s.bytes.len() as u64)
            .unwrap();
        segment.bytes[(0x100100 - segment.address) as usize] = 0xc3;
        let err = executor
            .run(&mut image, 0x100100, Arguments::default(), &mut [])
            .err()
            .expect("changed bytes");
        assert!(err.contains("Live executable bytes differ"), "{err}");
        executor.remove_image_translations(image.bias);
        file[0x100..0x102].copy_from_slice(&[0x0f, 0x0b]); // unsupported UD2
        let mut unsupported = Image::load(&file, 0x200000, 4096).unwrap();
        executor
            .register_direct_source(&unsupported, 100, arena.clone())
            .unwrap();
        let used = arena.used();
        for _ in 0..native_ps5::module_executor::MAX_TRANSLATION_REBUILDS {
            let error = executor
                .run(&mut unsupported, 0x200100, Arguments::default(), &mut [])
                .err()
                .expect("unsupported");
            assert!(error.contains("Direct lowering unsupported"), "{error}");
            assert_eq!(arena.used(), used);
        }
        let error = executor
            .run(&mut unsupported, 0x200100, Arguments::default(), &mut [])
            .err()
            .expect("attempt budget");
        assert!(error.contains("attempt budget"), "{error}");
        println!(
            "PASS: automatic direct entry/callee/return/tail translation, 20 repeated results, internal entry, overlap/instruction/attempt budget rejection, stale-byte and unsupported rejection, zero dylibs"
        );
    }
}
