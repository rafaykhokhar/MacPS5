use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(a: u64, b: u64, op: usize, old: u64) -> (u64, u64) {
    let a = a & 65535;
    let b = b & 65535;
    let exact = if op == 0 {
        a as i64 + b as i64
    } else {
        a as i64 - b as i64
    };
    let result = match op {
        0 | 1 | 2 => exact as u64,
        3 | 6 => a & b,
        4 => a | b,
        _ => a ^ b,
    } & 65535;
    let mut flags = (old & !0x8d5) | 2;
    if op < 3 {
        if !(0..65536).contains(&exact) {
            flags |= 1;
        }
        let nibble = if op == 0 {
            (a & 15) as i32 + (b & 15) as i32
        } else {
            (a & 15) as i32 - (b & 15) as i32
        };
        if !(0..16).contains(&nibble) {
            flags |= 16;
        }
        let signed = if op == 0 {
            a as i16 as i32 + b as i16 as i32
        } else {
            a as i16 as i32 - b as i16 as i32
        };
        if !(-32768..32768).contains(&signed) {
            flags |= 2048;
        }
    }
    if result == 0 {
        flags |= 64;
    }
    if result & 32768 != 0 {
        flags |= 128;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for (op, (opcode, group)) in [
            (0x01, 0),
            (0x29, 5),
            (0x39, 7),
            (0x21, 4),
            (0x09, 1),
            (0x31, 6),
            (0x85, 0),
        ]
        .into_iter()
        .enumerate()
        {
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    for mode in 0..3 {
                        if op == 6 && mode == 1 {
                            continue;
                        }
                        for immediate in [0u16, 1, 15, 127, 128, 255, 32767, 32768, 65535] {
                            if mode == 0 && immediate != 0 {
                                continue;
                            }
                            let mut bytes = vec![
                                0x66,
                                0x40 | if mode == 0 { (src >> 3) << 2 } else { 0 } | (dst >> 3),
                            ];
                            bytes.push(if mode == 0 {
                                opcode
                            } else if op == 6 {
                                0xf7
                            } else if mode == 1 {
                                0x83
                            } else {
                                0x81
                            });
                            bytes.push(
                                0xc0 | ((if mode == 0 { src & 7 } else { group }) << 3) | (dst & 7),
                            );
                            if mode == 1 {
                                bytes.push(immediate as u8);
                            } else if mode == 2 {
                                bytes.extend_from_slice(&immediate.to_le_bytes());
                            }
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for a in [0u64, 1, 15, 32767, 32768, 65535] {
                                for b in [0u64, 1, 15, 32767, 32768, 65535] {
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xadff,
                                        registers: [0x123456789abcdef0; 16],
                                        ..Cpu::default()
                                    };
                                    cpu.registers[dst as usize] =
                                        (cpu.registers[dst as usize] & !65535) | a;
                                    if mode == 0 {
                                        cpu.registers[src as usize] =
                                            (cpu.registers[src as usize] & !65535) | b;
                                    }
                                    let mut expected = cpu.registers;
                                    let rhs = if mode == 0 {
                                        expected[src as usize]
                                    } else if mode == 1 {
                                        immediate as i8 as i16 as u16 as u64
                                    } else {
                                        immediate as u64
                                    };
                                    let (result, flags) =
                                        reference(expected[dst as usize], rhs, op, cpu.flags);
                                    if op != 2 && op != 6 {
                                        expected[dst as usize] =
                                            (expected[dst as usize] & !65535) | result;
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, expected,
                                        "reg {op}/{dst}/{src}/{mode}/{immediate}/{a}/{b}"
                                    );
                                    assert_eq!(
                                        cpu.flags, flags,
                                        "flags {op}/{dst}/{src}/{mode}/{immediate}/{a}/{b}"
                                    );
                                    assert_eq!((cpu.fault, cpu.fuel, cpu.stop), (0, 0, 3));
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("direct word arithmetic: {checks} native state checks passed");
    }
}
