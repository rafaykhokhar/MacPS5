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
        for suffix in [0x105usize, 0x108] {
            for fuel in [2, 3, 100] {
                let mut file = memory_fixture();
                file[0x100..0x109].copy_from_slice(&[0xb8, 40, 0, 0, 0, 0x83, 0xc0, 2, 0xc3]);
                let mut image = Image::load(&file, 0x100000, 4096).unwrap();
                let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
                // Exactly three unique instructions: the join must not consume
                // extra instruction budget or add a synthetic guest instruction.
                executor
                    .register_direct_source(&image, 3, arena.clone())
                    .unwrap();
                executor.set_automatic_translation(true);
                assert!(
                    matches!(executor.run(&mut image,0x100000+suffix as u64,Arguments::default(),&mut []).unwrap(),Execution::Returned(v) if v == if suffix == 0x105 { 2 } else { 0 })
                );
                executor.fuel = fuel;
                let result = executor.run(&mut image, 0x100100, Arguments::default(), &mut []);
                if fuel == 2 {
                    let error = result.err().expect("fuel must stop before RET");
                    assert!(error.contains("fuel exhausted"), "{error}");
                } else {
                    assert!(matches!(result.unwrap(), Execution::Returned(42)));
                    let used = arena.used();
                    for _ in 0..10 {
                        assert!(matches!(
                            executor
                                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                                .unwrap(),
                            Execution::Returned(42)
                        ));
                        assert_eq!(arena.used(), used);
                    }
                }
            }
        }
        // A stale join destination must reject predecessor publication.
        let mut file = memory_fixture();
        file[0x100..0x109].copy_from_slice(&[0xb8, 40, 0, 0, 0, 0x83, 0xc0, 2, 0xc3]);
        let mut image = Image::load(&file, 0x200000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0xa00000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_source(&image, 3, arena.clone())
            .unwrap();
        executor.set_automatic_translation(true);
        executor
            .run(&mut image, 0x200105, Arguments::default(), &mut [])
            .unwrap();
        let used = arena.used();
        let segment = image
            .segments
            .iter_mut()
            .find(|s| s.address <= 0x200107 && 0x200107 - s.address < s.bytes.len() as u64)
            .unwrap();
        segment.bytes[(0x200107 - segment.address) as usize] = 3;
        let error = executor
            .run(&mut image, 0x200100, Arguments::default(), &mut [])
            .err()
            .expect("stale suffix");
        assert!(error.contains("Live executable bytes differ"), "{error}");
        assert_eq!(arena.used(), used);
        println!(
            "PASS: on-demand predecessor joins at two existing boundaries, exact instruction/fuel budgets, 40 cached repeats without allocation, stale suffix rejection before publication"
        );
    }
}
