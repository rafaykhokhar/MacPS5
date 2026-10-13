use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for dest in 0..16u8 {
                for base in 0..16u8 {
                    for index in 0..16u8 {
                        if index == 4 {
                            continue;
                        }
                        for scale in 0..4u8 {
                            let bytes = [
                                0x40 | if wide { 8 } else { 0 }
                                    | ((dest >> 3) << 2)
                                    | ((index >> 3) << 1)
                                    | (base >> 3),
                                0x8d,
                                0x44 | ((dest & 7) << 3),
                                (scale << 6) | ((index & 7) << 3) | (base & 7),
                                0xf8,
                                0xc3,
                            ];
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for value in [0u64, 0x123456789abcdef0, u64::MAX] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xa55a,
                                    memory: 0,
                                    ..Cpu::default()
                                };
                                for (r, v) in cpu.registers.iter_mut().enumerate() {
                                    *v = value.wrapping_add(r as u64);
                                }
                                let mut expected = cpu.registers;
                                let address = expected[base as usize]
                                    .wrapping_add(
                                        expected[index as usize].wrapping_shl(scale as u32),
                                    )
                                    .wrapping_sub(8);
                                expected[dest as usize] =
                                    if wide { address } else { address as u32 as u64 };
                                run(&mut cpu);
                                assert_eq!(cpu.registers, expected);
                                assert_eq!(
                                    (cpu.flags, cpu.fault, cpu.stop, cpu.fuel),
                                    (0xa55a, 0, 3, 0)
                                );
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        // RIP-relative forms, segment prefixes (ignored by LEA), fuel and faults.
        for prefix in [None, Some(0x64), Some(0x65)] {
            for wide in [false, true] {
                let mut bytes = Vec::new();
                if let Some(prefix) = prefix {
                    bytes.push(prefix);
                }
                bytes.extend_from_slice(&[
                    if wide { 0x48 } else { 0x40 },
                    0x8d,
                    0x05,
                    0xf0,
                    0xff,
                    0xff,
                    0xff,
                ]);
                let next = 0x100 + bytes.len() as u64;
                bytes.push(0xc3);
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for bias in [0u64, 0x1234567800000000, u64::MAX - 4] {
                    for fuel in [0, 1, 2] {
                        for fault in [0, 9] {
                            let mut cpu = Cpu {
                                rip: 0x100,
                                bias,
                                fuel,
                                fault,
                                flags: 0xa55a,
                                ..Cpu::default()
                            };
                            cpu.registers.fill(0x7777777777777777);
                            let mut expected = cpu.registers;
                            if fuel > 0 && fault == 0 {
                                let value = bias.wrapping_add(next).wrapping_sub(16);
                                expected[0] = if wide { value } else { value as u32 as u64 };
                            }
                            run(&mut cpu);
                            assert_eq!(cpu.registers, expected);
                            assert_eq!((cpu.flags, cpu.fault), (0xa55a, fault));
                            assert_eq!(cpu.fuel, if fault != 0 { fuel } else { 0 });
                            checks += 1;
                        }
                    }
                }
            }
        }
        assert!(direct_cpu::translate(&[0x67, 0x48, 0x8d, 0x00, 0xc3], 0x100, 2).is_err());
        assert!(direct_cpu::translate(&[0x66, 0x8d, 0x00, 0xc3], 0x100, 2).is_err());
        println!(
            "PASS: {checks} LEA cases with destination/base/index aliases, scales, wraparound, zero extension, RIP bias, ignored segment prefixes, fuel/faults and no memory mapping"
        );
    }
}
