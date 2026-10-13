use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let mut checks = 0;
        for legacy in [false, true] {
            for family in 0..4u8 {
                let lane = 1usize << family;
                for op in 0..2usize {
                    let arena = JitArena::new(32 * 1024 * 1024).unwrap();
                    for wide in 0..2u8 {
                        if legacy && wide != 0 {
                            continue;
                        }
                        let size = if wide == 0 { 16usize } else { 32 };
                        for dst in 0..16u8 {
                            for first in 0..16u8 {
                                if legacy && first != dst {
                                    continue;
                                }
                                for second in 0..16u8 {
                                    for mem in [false, true] {
                                        let opcode = if op == 0 {
                                            [0xfc, 0xfd, 0xfe, 0xd4][family as usize]
                                        } else {
                                            [0xf8, 0xf9, 0xfa, 0xfb][family as usize]
                                        };
                                        let mut bytes = Vec::new();
                                        if legacy {
                                            bytes.push(0x66);
                                            bytes.extend_from_slice(&[
                                                0x40 | ((dst >> 3) << 2) | (second >> 3),
                                                0x0f,
                                                opcode,
                                            ]);
                                        } else {
                                            bytes.extend_from_slice(&[
                                                0xc4,
                                                0x41 | if dst < 8 { 0x80 } else { 0 }
                                                    | if second < 8 { 0x20 } else { 0 },
                                                ((!first & 15) << 3) | 1 | (wide * 4),
                                                opcode,
                                            ]);
                                        }
                                        bytes.push(
                                            (if mem { 0x40 } else { 0xc0 })
                                                | ((dst & 7) << 3)
                                                | (second & 7),
                                        );
                                        if mem {
                                            if second & 7 == 4 {
                                                bytes.push(0x24);
                                            }
                                            bytes.push(0xf8);
                                        }
                                        let end = 0x100 + bytes.len() as u64;
                                        bytes.push(0xc3);
                                        let code = arena
                                            .publish(
                                                &direct_cpu::translate(&bytes, 0x100, 2).unwrap(),
                                            )
                                            .unwrap();
                                        let run: unsafe extern "C" fn(*mut Cpu) =
                                            std::mem::transmute(code.entry(0).unwrap());
                                        for address in [
                                            0x1000u64,
                                            0x1001,
                                            0x1020,
                                            0x1030,
                                            0x1031,
                                            0x1040,
                                            u64::MAX,
                                        ] {
                                            if !mem && address != 0x1000 {
                                                continue;
                                            }
                                            let mut data = [0u8; 64];
                                            for (n, b) in data.iter_mut().enumerate() {
                                                *b = (n as u8).wrapping_mul(37).wrapping_add(9);
                                            }
                                            let original = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, false, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel: 2,
                                                flags: 0xadff,
                                                mxcsr: 0x9fc1,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                registers: [0xdeadbeef; 16],
                                                ..Cpu::default()
                                            };
                                            cpu.registers[second as usize] =
                                                address.wrapping_add(8);
                                            for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                                for (n, b) in v.iter_mut().enumerate() {
                                                    *b = (r as u8)
                                                        .wrapping_mul(71)
                                                        .wrapping_add(n as u8 * 7);
                                                }
                                            }
                                            let registers = cpu.registers;
                                            let mut expected = cpu.vectors;
                                            let valid = !mem
                                                || ((!legacy || address % 16 == 0)
                                                    && address >= 0x1000
                                                    && address
                                                        .checked_add(size as u64)
                                                        .is_some_and(|end| end <= 0x1040));
                                            if valid {
                                                let mut result = if legacy {
                                                    expected[dst as usize]
                                                } else {
                                                    [0; 32]
                                                };
                                                let mut carry = 0i16;
                                                for n in 0..size {
                                                    if n % lane == 0 {
                                                        carry = 0;
                                                    }
                                                    let a = expected[first as usize][n];
                                                    let b = if mem {
                                                        original[(address - 0x1000) as usize + n]
                                                    } else {
                                                        expected[second as usize][n]
                                                    };
                                                    let byte = if op == 0 {
                                                        a as i16 + b as i16 + carry
                                                    } else {
                                                        a as i16 - b as i16 + carry
                                                    };
                                                    result[n] = byte.rem_euclid(256) as u8;
                                                    carry = byte.div_euclid(256);
                                                }
                                                expected[dst as usize] = result;
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.vectors, expected,
                                                "legacy={legacy} family={family} op={op} wide={wide} dst={dst} first={first} second={second} mem={mem} addr={address:x}"
                                            );
                                            assert_eq!(cpu.registers, registers);
                                            assert_eq!(cpu.flags, 0xadff);
                                            assert_eq!(cpu.mxcsr, 0x9fc1);
                                            assert_eq!(memory.read(0x1000, 64).unwrap(), original);
                                            assert_eq!(
                                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                                if valid {
                                                    (0, 3, 0, end)
                                                } else {
                                                    (1, 0, 1, 0x100)
                                                }
                                            );
                                            checks += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("direct packed add/sub: {checks} native state checks passed");
    }
}
