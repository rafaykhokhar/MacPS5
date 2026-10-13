use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    // SAFETY: standalone host owns JIT state; generated code only accesses Cpu.
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for dst in 0..16 {
                for src in 0..16 {
                    let bytes = [
                        0x40 | if wide { 8 } else { 0 } | ((dst >> 3) << 2) | (src >> 3),
                        0x8b,
                        0xc0 | ((dst & 7) << 3) | (src & 7),
                        0xc3,
                    ];
                    let words = direct_cpu::translate(&bytes, 0x100, 2).unwrap();
                    let block = arena.publish(&words).unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(block.entry(0).unwrap());
                    for fuel in [0, 1, 2, 10] {
                        for fault in [0, 9] {
                            let mut cpu = Cpu {
                                rip: 0x100,
                                fuel,
                                fault,
                                flags: 0xa55a,
                                ..Cpu::default()
                            };
                            for (index, r) in cpu.registers.iter_mut().enumerate() {
                                *r = 0xabcdef0000000080 | index as u64;
                            }
                            let old = cpu.clone();
                            run(&mut cpu);
                            let mut expected = old.registers;
                            if fault == 0 && fuel > 0 {
                                expected[dst as usize] = if wide {
                                    old.registers[src as usize]
                                } else {
                                    u64::from(old.registers[src as usize] as u32)
                                };
                            }
                            assert_eq!(cpu.registers, expected);
                            assert_eq!(cpu.flags, old.flags);
                            assert_eq!(cpu.fault, fault);
                            assert_eq!(
                                cpu.fuel,
                                if fault == 0 {
                                    fuel.saturating_sub(2)
                                } else {
                                    fuel
                                }
                            );
                            assert_eq!(
                                cpu.stop,
                                if fault != 0 {
                                    0
                                } else if fuel < 2 {
                                    4
                                } else {
                                    3
                                }
                            );
                            assert_eq!(cpu.rip, if fault == 0 && fuel > 0 { 0x103 } else { 0x100 });
                            checks += 1;
                        }
                    }
                    let mut bad = Cpu {
                        rip: 0x101,
                        fuel: 5,
                        ..Cpu::default()
                    };
                    run(&mut bad);
                    assert_eq!(bad.fault, 7);
                    assert_eq!(bad.fuel, 5);
                }
            }
        }
        for wide in [false, true] {
            for dst in 0..16u8 {
                let value = 0xfedcba9876543210u64;
                let mut bytes = vec![
                    0x40 | if wide { 8 } else { 0 } | (dst >> 3),
                    0xb8 | (dst & 7),
                ];
                bytes.extend_from_slice(&value.to_le_bytes()[..if wide { 8 } else { 4 }]);
                bytes.extend_from_slice(&[0x90, 0xc3]);
                let block = arena
                    .publish(&direct_cpu::translate(&bytes, 0x200, 3).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(block.entry(0).unwrap());
                let mut cpu = Cpu {
                    rip: 0x200,
                    fuel: 3,
                    ..Cpu::default()
                };
                run(&mut cpu);
                assert_eq!(
                    cpu.registers[dst as usize],
                    if wide { value } else { u64::from(value as u32) }
                );
                assert_eq!(cpu.stop, 3);
                assert_eq!(cpu.fuel, 0);
                assert_eq!(cpu.rip, 0x200 + bytes.len() as u64 - 1);
                checks += 1;
            }
        }
        for bytes in [
            &[0x01, 0xc0, 0xc3][..],
            &[0x64, 0x48, 0x8b, 0x00, 0xc3],
            &[0xb8, 1],
            &[0xc3, 0x90],
            &[0x90],
        ] {
            assert!(direct_cpu::translate(bytes, 0x100, 100).is_err());
        }
        assert!(direct_cpu::translate(&[0x90, 0xc3], 0x100, 1).is_err());
        let mut file = memory_fixture();
        let bytes = [0xb8, 42, 0, 0, 0, 0xc3];
        file[0x100..0x106].copy_from_slice(&bytes);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let before = arena.used();
        assert!(
            executor
                .register_direct_in_image(&image, 0x100, &[0xc3], 10, &arena)
                .is_err()
        );
        assert_eq!(before, arena.used());
        executor
            .register_direct_in_image(&image, 0x100, &bytes, 10, &arena)
            .unwrap();
        let used = arena.used();
        assert!(
            executor
                .register_direct_in_image(&image, 0x100, &bytes, 10, &arena)
                .is_err()
        );
        assert_eq!(used, arena.used());
        drop(arena);
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(42)
        ));
        image.segments[0].bytes[0x100] = 0x90;
        assert!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .is_err()
        );
        executor.remove_image_translations(image.bias);
        let _new_arena = JitArena::new(16384).unwrap();
        println!(
            "PASS: {checks} direct guest MOV state/fuel/fault checks, 512 invalid-PC checks, actual executor return 42, stale-byte rejection and pinned retirement"
        );
    }
}
