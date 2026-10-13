use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for displacement in [0i32, 1, -1, 0x12345678, i32::MIN, i32::MAX] {
            let mut bytes = vec![0xe8];
            bytes.extend_from_slice(&displacement.to_le_bytes());
            let code = arena
                .publish(&direct_cpu::translate(&bytes, 0x100, 1).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for bias in [0u64, 0x100000, u64::MAX - 0x100] {
                for fuel in [0, 1, 10] {
                    for fault in [0, 9] {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            bias,
                            fuel,
                            fault,
                            flags: 0xa55a,
                            target: 77,
                            next_rip: 88,
                            ..Cpu::default()
                        };
                        cpu.registers.fill(0x1234);
                        run(&mut cpu);
                        assert_eq!(cpu.registers, [0x1234; 16]);
                        assert_eq!(cpu.flags, 0xa55a);
                        assert_eq!(cpu.rip, 0x100);
                        assert_eq!(cpu.fault, fault);
                        if fault != 0 {
                            assert_eq!(
                                (cpu.fuel, cpu.stop, cpu.target, cpu.next_rip),
                                (fuel, 0, 77, 88)
                            );
                        } else if fuel == 0 {
                            assert_eq!(
                                (cpu.fuel, cpu.stop, cpu.target, cpu.next_rip),
                                (0, 4, 77, 88)
                            );
                        } else {
                            assert_eq!(
                                (cpu.fuel, cpu.stop, cpu.target, cpu.next_rip),
                                (
                                    fuel - 1,
                                    1,
                                    0x105u64
                                        .wrapping_add(displacement as i64 as u64)
                                        .wrapping_add(bias),
                                    0x105
                                )
                            );
                        }
                        checks += 1;
                    }
                }
            }
        }
        // Caller saves RSP in RBX, calls outer, verifies stack restoration by
        // adding RSP-RBX to EAX, then adds 1. Outer calls leaf and adds 2.
        let caller = [0x48, 0x89, 0xe3, 0xe8, 0x18, 0, 0, 0];
        let resume = [
            0x48, 0x89, 0xe2, 0x48, 0x29, 0xda, 0x01, 0xd0, 0x83, 0xc0, 1, 0xc3,
        ];
        let outer = [0xe8, 0x1b, 0, 0, 0];
        let outer_resume = [0x83, 0xc0, 2, 0xc3];
        let leaf = [0xb8, 39, 0, 0, 0, 0xc3];
        let spans = [
            (0x100, &caller[..]),
            (0x108, &resume[..]),
            (0x120, &outer[..]),
            (0x125, &outer_resume[..]),
            (0x140, &leaf[..]),
        ];
        let mut file = memory_fixture();
        for (at, bytes) in spans {
            file[at..at + bytes.len()].copy_from_slice(bytes);
        }
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &caller, 2, &arena)
            .unwrap();
        assert!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .is_err()
        );
        for (at, bytes) in spans.into_iter().skip(1) {
            executor
                .register_direct_in_image(&image, at as u64, bytes, 10, &arena)
                .unwrap();
        }
        for _ in 0..20 {
            assert!(matches!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(42)
            ));
        }
        // Register caller/callee but omit the continuation: return must stop.
        let mut missing = NativeExecutor::new(0xa00000, 4096, 100, 1000).unwrap();
        missing
            .register_direct_in_image(&image, 0x120, &outer, 1, &arena)
            .unwrap();
        missing
            .register_direct_in_image(&image, 0x140, &leaf, 2, &arena)
            .unwrap();
        assert!(
            missing
                .run(&mut image, 0x100120, Arguments::default(), &mut [])
                .is_err()
        );
        // A callee that sets RSP to an unmapped address cannot return.
        executor.remove_image_translations(image.bias);
        let bad = [0x48, 0xbc, 0, 0, 0, 0, 0, 0, 0, 0, 0xc3];
        file[0x140..0x14b].copy_from_slice(&bad);
        let mut bad_image = Image::load(&file, 0x200000, 4096).unwrap();
        executor
            .register_direct_in_image(&bad_image, 0x120, &outer, 1, &arena)
            .unwrap();
        executor
            .register_direct_in_image(&bad_image, 0x140, &bad, 2, &arena)
            .unwrap();
        assert!(
            executor
                .run(&mut bad_image, 0x200120, Arguments::default(), &mut [])
                .is_err()
        );
        assert!(direct_cpu::translate(&[0xe8, 0, 0, 0, 0, 0xc3], 0x100, 2).is_err());
        println!(
            "PASS: {checks} direct CALL state/bias/fuel checks, 20 nested executor calls with restored stack, missing callee/continuation and invalid return-stack rejection"
        );
    }
}
