use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            let bits = if wide { 64 } else { 32 };
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            for (operation, extension) in [4u8, 5, 7, 0, 1].into_iter().enumerate() {
                for variable in [false, true] {
                    for reg in 0..16u8 {
                        for raw in 0..=255u8 {
                            let mut bytes = vec![
                                0x40 | if wide { 8 } else { 0 } | (reg >> 3),
                                if variable { 0xd3 } else { 0xc1 },
                                0xc0 | (extension << 3) | (reg & 7),
                            ];
                            if !variable {
                                bytes.push(raw);
                            }
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for value in [0u64, 1, sign - 1, sign, mask, 0x123456789abcdef0] {
                                let value = if variable && reg == 1 {
                                    (value & !255) | u64::from(raw)
                                } else {
                                    value
                                };
                                for oldflags in [2u64, 0xa55b] {
                                    let count = raw & if wide { 63 } else { 31 };
                                    let mut result = value & mask;
                                    let mut carry = oldflags & 1;
                                    for _ in 0..count {
                                        match operation {
                                            0 => {
                                                carry = (result & sign != 0) as u64;
                                                result = (result << 1) & mask;
                                            }
                                            1 => {
                                                carry = result & 1;
                                                result >>= 1;
                                            }
                                            2 => {
                                                carry = result & 1;
                                                result = (result >> 1) | (result & sign);
                                            }
                                            3 => {
                                                carry = (result & sign != 0) as u64;
                                                result = ((result << 1) & mask) | carry;
                                            }
                                            4 => {
                                                carry = result & 1;
                                                result = (result >> 1) | (carry << (bits - 1));
                                            }
                                            _ => unreachable!(),
                                        }
                                    }
                                    let mut flags = oldflags;
                                    if count != 0 {
                                        flags = (flags & !if operation >= 3 { 1 } else { 0xc5 })
                                            | carry;
                                        if operation < 3 {
                                            if result == 0 {
                                                flags |= 64;
                                            }
                                            if result & sign != 0 {
                                                flags |= 128;
                                            }
                                            if (result as u8).count_ones() % 2 == 0 {
                                                flags |= 4;
                                            }
                                        }
                                        if count == 1 {
                                            let overflow = match operation {
                                                0 | 3 => (result >> (bits - 1)) ^ carry,
                                                1 => (value & mask) >> (bits - 1),
                                                4 => {
                                                    ((result >> (bits - 1))
                                                        ^ (result >> (bits - 2)))
                                                        & 1
                                                }
                                                _ => 0,
                                            };
                                            flags = (flags & !0x800) | (overflow << 11);
                                        }
                                    }
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: oldflags,
                                        ..Cpu::default()
                                    };
                                    cpu.registers.fill(0xfeeddeadbeeff00d);
                                    if variable {
                                        cpu.registers[1] = 0xfedcba9876543200 | u64::from(raw);
                                    }
                                    cpu.registers[reg as usize] = value;
                                    let mut expected = cpu.registers;
                                    expected[reg as usize] = result;
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
        }
        // Implicit-one encoding and shared budget/previous fault behavior.
        for fuel in [0, 1, 2] {
            for fault in [0, 9] {
                let code = arena
                    .publish(&direct_cpu::translate(&[0x48, 0xd1, 0xe0, 0xc3], 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                let mut cpu = Cpu {
                    rip: 0x100,
                    fuel,
                    fault,
                    flags: 0xa55a,
                    ..Cpu::default()
                };
                cpu.registers[0] = 3;
                run(&mut cpu);
                assert_eq!(cpu.registers[0], if fuel > 0 && fault == 0 { 6 } else { 3 });
                assert_eq!(cpu.fault, fault);
                checks += 1;
            }
        }
        println!(
            "PASS: {checks} immediate/CL shift/rotate register cases with all 256 counts, iterative result/flag oracle, both widths, all registers including count aliases, and implicit-one/fuel/fault checks"
        );
    }
}
