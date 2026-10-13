use native_ps5::{
    jit_memory::JitArena,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        for table in [false, true] {
            // Save the original stack in RBX. The destination verifies no return
            // address was pushed by the JMP, then returns to the original caller.
            let mut head = vec![0x48, 0x89, 0xe3, 0x48, 0xb8];
            head.extend_from_slice(&0x100140u64.to_le_bytes());
            if table {
                head.extend_from_slice(&[0xff, 0x25, 0x6d, 0, 0, 0]);
            } else {
                head.extend_from_slice(&[0xff, 0xe0]);
            }
            let leaf = [0x48, 0x89, 0xe0, 0x48, 0x29, 0xd8, 0x83, 0xc0, 42, 0xc3];
            let mut file = memory_fixture();
            file[0x100..0x100 + head.len()].copy_from_slice(&head);
            file[0x140..0x140 + leaf.len()].copy_from_slice(&leaf);
            file[0x180..0x188].copy_from_slice(&0x100140u64.to_le_bytes());
            let mut image = Image::load(&file, 0x100000, 4096).unwrap();
            let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
            executor
                .register_direct_in_image(&image, 0x100, &head, 3, &arena)
                .unwrap();
            assert!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .is_err()
            );
            executor
                .register_direct_in_image(&image, 0x140, &leaf, 4, &arena)
                .unwrap();
            assert!(matches!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(42)
            ));
        }
        // A register-indirect self-loop must still stop at the fuel boundary.
        let mut file = memory_fixture();
        let mut looping = vec![0x48, 0xb8];
        looping.extend_from_slice(&0x200100u64.to_le_bytes());
        looping.extend_from_slice(&[0xff, 0xe0]);
        file[0x100..0x10c].copy_from_slice(&looping);
        let mut image = Image::load(&file, 0x200000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0xa00000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &looping, 2, &arena)
            .unwrap();
        let error = executor
            .run(&mut image, 0x200100, Arguments::default(), &mut [])
            .err()
            .expect("loop must stop");
        assert!(error.contains("fuel exhausted"), "{error}");
        println!(
            "PASS: register and RIP-relative indirect JMP preserve executor stack, reject missing targets, and stop self-loop at fuel limit"
        );
    }
}
