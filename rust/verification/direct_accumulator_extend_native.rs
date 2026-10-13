use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for (kind, body) in [
            vec![0x66, 0x98],
            vec![0x98],
            vec![0x48, 0x98],
            vec![0x66, 0x99],
            vec![0x99],
            vec![0x48, 0x99],
        ]
        .into_iter()
        .enumerate()
        {
            let mut bytes = body.clone();
            let end = 0x100 + bytes.len() as u64;
            bytes.push(0xc3);
            let code = arena
                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            let mut values = vec![
                0u64,
                1,
                0x7f,
                0x80,
                0xff,
                0x7fff,
                0x8000,
                0xffff,
                0x7fffffff,
                0x80000000,
                0x7fffffffffffffff,
                0x8000000000000000,
                u64::MAX,
            ];
            if kind == 0 {
                values.extend((0..256).map(|v| 0x123456789abcde00 | v));
            }
            if kind == 1 || kind == 3 {
                values.extend((0..65536).map(|v| 0x123456789abc0000 | v));
            }
            for value in values {
                for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 9)] {
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel,
                        fault: prior,
                        flags: 0xadff,
                        registers: [0x123456789abcdef0; 16],
                        ..Cpu::default()
                    };
                    cpu.registers[0] = value;
                    let mut expected = cpu.registers;
                    let executed = fuel > 0 && prior == 0;
                    if executed {
                        match kind {
                            0 => {
                                expected[0] =
                                    (value & !65535) | u64::from(value as i8 as i16 as u16)
                            }
                            1 => expected[0] = u64::from(value as i16 as i32 as u32),
                            2 => expected[0] = value as i32 as i64 as u64,
                            3 => {
                                expected[2] = (expected[2] & !65535)
                                    | if (value as i16) < 0 { 65535 } else { 0 }
                            }
                            4 => {
                                expected[2] = if (value as i32) < 0 {
                                    u32::MAX as u64
                                } else {
                                    0
                                }
                            }
                            _ => expected[2] = if (value as i64) < 0 { u64::MAX } else { 0 },
                        }
                    }
                    run(&mut cpu);
                    assert_eq!(cpu.registers, expected, "{kind}/{value:x}");
                    assert_eq!(cpu.flags, 0xadff);
                    assert_eq!(
                        (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                        (
                            prior,
                            if prior != 0 {
                                0
                            } else if fuel < 2 {
                                4
                            } else {
                                3
                            },
                            if prior != 0 { fuel } else { 0 },
                            if executed { end } else { 0x100 }
                        )
                    );
                    checks += 1;
                }
            }
        }
        // Compiler-style sign extension followed immediately by signed division.
        for size in [1usize, 2, 4, 8] {
            let mut bytes = match size {
                1 => vec![0x66, 0x98, 0xf6, 0xf9],
                2 => vec![0x66, 0x99, 0x66, 0xf7, 0xf9],
                4 => vec![0x99, 0xf7, 0xf9],
                _ => vec![0x48, 0x99, 0x48, 0xf7, 0xf9],
            };
            bytes.push(0xc3);
            let code = arena
                .publish(&direct_cpu::translate(&bytes, 0x100, 3).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            let mask = u64::MAX >> (64 - size * 8);
            for dividend in [-127i64, -3, -1, 0, 1, 3, 127] {
                for divisor in [-7i64, -1, 1, 7] {
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel: 3,
                        flags: 0xadff,
                        registers: [0x123456789abcdef0; 16],
                        ..Cpu::default()
                    };
                    cpu.registers[0] = (cpu.registers[0] & !mask) | ((dividend as u64) & mask);
                    cpu.registers[1] = (divisor as u64) & mask;
                    let mut expected = cpu.registers;
                    let q = (dividend / divisor) as u64 & mask;
                    let r = (dividend % divisor) as u64 & mask;
                    if size == 1 {
                        expected[0] = (expected[0] & !65535) | q | (r << 8);
                    } else if size == 2 {
                        expected[0] = (expected[0] & !mask) | q;
                        expected[2] = (expected[2] & !mask) | r;
                    } else {
                        expected[0] = q;
                        expected[2] = r;
                    }
                    run(&mut cpu);
                    assert_eq!(cpu.registers, expected);
                    assert_eq!(cpu.flags, 0xadff);
                    assert_eq!((cpu.fault, cpu.stop, cpu.fuel), (0, 3, 0));
                    checks += 1;
                }
            }
        }
        println!("direct accumulator extension: {checks} native state/composition checks passed");
    }
}
