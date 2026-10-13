use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn offset(n: u8, rex: Option<u8>, source: bool) -> (usize, u32) {
    if rex.is_none() && n >= 4 {
        ((n - 4) as usize, 8)
    } else {
        (
            n as usize
                + if rex.is_some_and(|r| r & if source { 4 } else { 1 } != 0) {
                    8
                } else {
                    0
                },
            0,
        )
    }
}
fn reference(a: u8, b: u8, op: usize, old: u64) -> (u8, u64) {
    let result = match op {
        0 => a.wrapping_add(b),
        1 | 2 => a.wrapping_sub(b),
        3 | 6 => a & b,
        4 => a | b,
        5 => a ^ b,
        _ => unreachable!(),
    };
    let mut flags = (old & !0x8d5) | 2;
    if if op == 0 {
        (a as u16 + b as u16) > 255
    } else {
        op <= 2 && a < b
    } {
        flags |= 1;
    }
    if result.count_ones() % 2 == 0 {
        flags |= 4;
    }
    if op <= 2
        && (if op == 0 {
            (a & 15) + (b & 15) > 15
        } else {
            (a & 15) < (b & 15)
        })
    {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result & 128 != 0 {
        flags |= 128;
    }
    let exact = if op == 0 {
        a as i8 as i16 + b as i8 as i16
    } else {
        a as i8 as i16 - b as i8 as i16
    };
    if op <= 2 && !(-128..128).contains(&exact) {
        flags |= 2048;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for (op, opcode) in [0x00, 0x28, 0x38, 0x20, 0x08, 0x30, 0x84]
            .into_iter()
            .enumerate()
        {
            for rex in [None, Some(0x40), Some(0x41), Some(0x44), Some(0x45)] {
                for dst in 0..8u8 {
                    for src in 0..8u8 {
                        let mut bytes = Vec::new();
                        if let Some(rex) = rex {
                            bytes.push(rex);
                        }
                        bytes.extend_from_slice(&[opcode, 0xc0 | (src << 3) | dst, 0xc3]);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        let (dr, ds) = offset(dst, rex, false);
                        let (sr, ss) = offset(src, rex, true);
                        for a in [0u8, 1, 15, 16, 127, 128, 254, 255] {
                            for b in [0u8, 1, 15, 16, 127, 128, 254, 255] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xa55b,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0x123456789abcdef0);
                                cpu.registers[dr] =
                                    (cpu.registers[dr] & !(255u64 << ds)) | ((a as u64) << ds);
                                cpu.registers[sr] =
                                    (cpu.registers[sr] & !(255u64 << ss)) | ((b as u64) << ss);
                                let mut expected = cpu.registers;
                                let (result, flags) = reference(
                                    (expected[dr] >> ds) as u8,
                                    (expected[sr] >> ss) as u8,
                                    op,
                                    cpu.flags,
                                );
                                if !matches!(op, 2 | 6) {
                                    expected[dr] =
                                        (expected[dr] & !(255u64 << ds)) | ((result as u64) << ds);
                                }
                                run(&mut cpu);
                                assert_eq!(cpu.registers, expected);
                                assert_eq!(cpu.flags, flags);
                                assert_eq!((cpu.fault, cpu.stop, cpu.fuel), (0, 3, 0));
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        // Immediate forms, including accumulator encoding and legacy high destination.
        for (op, group, accumulator) in [
            (0, 0, 0x04),
            (1, 5, 0x2c),
            (2, 7, 0x3c),
            (3, 4, 0x24),
            (4, 1, 0x0c),
            (5, 6, 0x34),
            (6, 0, 0xa8),
        ] {
            for value in [0u8, 1, 127, 128, 255] {
                for high in [false, true] {
                    let bytes = if high {
                        vec![
                            if op == 6 { 0xf6 } else { 0x80 },
                            0xc4 | (group << 3),
                            value,
                            0xc3,
                        ]
                    } else {
                        vec![accumulator, value, 0xc3]
                    };
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel: 2,
                        flags: 0xa55b,
                        ..Cpu::default()
                    };
                    cpu.registers[0] = 0x123456789abcdef0;
                    let shift = if high { 8 } else { 0 };
                    let (result, flags) =
                        reference((cpu.registers[0] >> shift) as u8, value, op, cpu.flags);
                    let expected = if matches!(op, 2 | 6) {
                        cpu.registers[0]
                    } else {
                        (cpu.registers[0] & !(255u64 << shift)) | ((result as u64) << shift)
                    };
                    run(&mut cpu);
                    assert_eq!(cpu.registers[0], expected);
                    assert_eq!(cpu.flags, flags);
                    checks += 1;
                }
            }
        }
        println!(
            "PASS: {checks} byte arithmetic/logical cases with independent flags, legacy/REX aliases, partial-write preservation and immediate forms"
        );
    }
}
