use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            let bits = if wide { 64 } else { 32 };
            let modulus = 1u128 << bits;
            let mask = (modulus - 1) as u64;
            let sign = 1u64 << (bits - 1);
            for decrement in [false, true] {
                for reg in 0..16u8 {
                    let bytes = [
                        0x40 | if wide { 8 } else { 0 } | (reg >> 3),
                        0xff,
                        0xc0 | if decrement { 8 } else { 0 } | (reg & 7),
                        0xc3,
                    ];
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for value in [0u64, 1, 15, 16, sign - 1, sign, mask, 0x123456789abcdef0] {
                        for oldflags in [2u64, 3, 0xa55a, 0xa55b] {
                            for fuel in [0, 1, 2] {
                                for fault in [0, 9] {
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        flags: oldflags,
                                        fuel,
                                        fault,
                                        ..Cpu::default()
                                    };
                                    cpu.registers.fill(0xfedcba9876543210);
                                    cpu.registers[reg as usize] = value;
                                    let mut expected = cpu.registers;
                                    let mut flags = oldflags;
                                    if fuel > 0 && fault == 0 {
                                        let a = value & mask;
                                        let result = if decrement {
                                            a.wrapping_sub(1)
                                        } else {
                                            a.wrapping_add(1)
                                        } & mask;
                                        expected[reg as usize] = result;
                                        let signed = if a & sign == 0 {
                                            a as i128
                                        } else {
                                            a as i128 - modulus as i128
                                        };
                                        let exact = if decrement { signed - 1 } else { signed + 1 };
                                        flags = (oldflags & !0x8d4) | 2;
                                        if (result as u8).count_ones() % 2 == 0 {
                                            flags |= 4;
                                        }
                                        if if decrement { a & 15 == 0 } else { a & 15 == 15 } {
                                            flags |= 16;
                                        }
                                        if result == 0 {
                                            flags |= 64;
                                        }
                                        if result & sign != 0 {
                                            flags |= 128;
                                        }
                                        if exact < -(sign as i128) || exact >= sign as i128 {
                                            flags |= 2048;
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(cpu.registers, expected);
                                    assert_eq!(cpu.flags, flags);
                                    assert_eq!(cpu.flags & 1, oldflags & 1);
                                    assert_eq!(cpu.fault, fault);
                                    assert_eq!(cpu.fuel, if fault != 0 { fuel } else { 0 });
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(direct_cpu::translate(&[0x48, 0xff, 0x00, 0xc3], 0x100, 2).is_err());
        println!(
            "PASS: {checks} register INC/DEC cases with independent result/flags, carry preservation, width/alias coverage, fuel and prior faults"
        );
    }
}
