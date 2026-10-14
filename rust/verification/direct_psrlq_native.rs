use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};

fn main() {
    let mut checks = 0;
    let mut guards = 0;
    unsafe {
        for form in 0..3 {
            let legacy = form == 0;
            let size = if form == 2 { 32 } else { 16 };
            let arena = JitArena::new(64 * 1024 * 1024).unwrap();
            for count in 0..=255u8 {
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        if legacy && src != dst {
                            continue;
                        }
                        let mut bytes = if legacy {
                            vec![0x66, 0x40 | (src >> 3), 0x0f, 0x73]
                        } else {
                            vec![
                                0xc4,
                                0xc1 | if src < 8 { 0x20 } else { 0 },
                                ((!dst & 15) << 3) | 1 | if form == 2 { 4 } else { 0 },
                                0x73,
                            ]
                        };
                        bytes.extend_from_slice(&[0xd0 | (src & 7), count]);
                        let end = 0x100 + bytes.len() as u64;
                        bytes.push(0xc3);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for (fault, fuel) in [(7, 2), (0, 0)] {
                            let mut cpu = Cpu {
                                registers: [0xabcdef; 16],
                                vectors: [[0xa5; 32]; 16],
                                flags: 0xadff,
                                mxcsr: 0x9fc1,
                                rip: 0x100,
                                fault,
                                fuel,
                                ..Cpu::default()
                            };
                            run(&mut cpu);
                            assert_eq!(cpu.registers, [0xabcdef; 16]);
                            assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                            assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                            assert_eq!((cpu.rip, cpu.fault, cpu.fuel), (0x100, fault, fuel));
                            assert_eq!(cpu.stop, if fault != 0 { 0 } else { 4 });
                            guards += 1;
                        }
                        for pattern in [0u64, u64::MAX, 0x8000000000000001, 0x13579bdf2468ace0] {
                            let mut cpu = Cpu {
                                registers: [0xabcdef; 16],
                                flags: 0xadff,
                                mxcsr: 0x9fc1,
                                rip: 0x100,
                                fuel: 2,
                                ..Cpu::default()
                            };
                            for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                for (n, b) in v.iter_mut().enumerate() {
                                    *b = (r * 13 + n * 7) as u8;
                                }
                            }
                            for lane in 0..size / 8 {
                                let value = pattern.rotate_left(lane as u32 * 11);
                                cpu.vectors[src as usize][lane * 8..lane * 8 + 8]
                                    .copy_from_slice(&value.to_le_bytes());
                            }
                            let regs = cpu.registers;
                            let mut expected = cpu.vectors;
                            let source = expected[src as usize];
                            if !legacy {
                                expected[dst as usize] = [0; 32];
                            }
                            // Bit-by-bit model independent of the emitted host shift.
                            for lane in 0..size / 8 {
                                let mut result = [0u8; 8];
                                for bit in 0..64usize {
                                    let from = bit + usize::from(count);
                                    if from < 64
                                        && source[lane * 8 + from / 8] & (1 << (from % 8)) != 0
                                    {
                                        result[bit / 8] |= 1 << (bit % 8);
                                    }
                                }
                                expected[dst as usize][lane * 8..lane * 8 + 8]
                                    .copy_from_slice(&result);
                            }
                            run(&mut cpu);
                            assert_eq!(
                                cpu.vectors, expected,
                                "form={form} dst={dst} src={src} count={count}"
                            );
                            assert_eq!(cpu.registers, regs);
                            assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                            assert_eq!((cpu.rip, cpu.fault, cpu.fuel, cpu.stop), (end, 0, 0, 3));
                            checks += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "direct PSRLQ immediate: {checks} native state checks and {guards} fault/fuel guards passed"
    );
}
