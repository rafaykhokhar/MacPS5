use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};

fn reference(mut value: u64, bits: u32, count: u8, op: u8, old: u64) -> (u64, u64) {
    let mask = (1 << bits) - 1;
    let sign = 1 << (bits - 1);
    value &= mask;
    let original = value;
    let mut carry = old & 1;
    let count = count & 31;
    for _ in 0..count {
        if op == 0 {
            carry = u64::from(value & sign != 0);
            value = ((value * 2) & mask) | carry;
        } else if op == 1 {
            carry = value & 1;
            value = value / 2 | (carry << (bits - 1));
        } else if op == 4 {
            carry = u64::from(value & sign != 0);
            value = (value * 2) & mask;
        } else {
            carry = value & 1;
            value = value / 2 | if op == 7 { value & sign } else { 0 };
        }
    }
    let mut flags = old;
    if count > 0 {
        flags = (flags & !if op < 2 { 1 } else { 0xc5 }) | carry;
        if op >= 2 {
            if value == 0 {
                flags |= 64;
            }
            if value & sign != 0 {
                flags |= 128;
            }
            if (value as u8).count_ones() % 2 == 0 {
                flags |= 4;
            }
        }
        if count == 1 {
            let overflow = if op == 0 || op == 4 {
                u64::from(value & sign != 0) ^ carry
            } else if op == 1 {
                ((value >> (bits - 1)) ^ (value >> (bits - 2))) & 1
            } else if op == 5 {
                u64::from(original & sign != 0)
            } else {
                0
            };
            flags = (flags & !0x800) | (overflow << 11);
        }
    }
    (value, flags)
}

fn main() {
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [1usize, 2] {
            let mask = (1u64 << (size * 8)) - 1;
            for rex in [false, true] {
                for reg in 0..if rex { 16u8 } else { 8 } {
                    let (slot, shift) = if size == 1 && !rex && reg >= 4 {
                        ((reg - 4) as usize, 8)
                    } else {
                        (reg as usize, 0)
                    };
                    for op in [4u8, 5, 7, 0, 1] {
                        for mode in 0..3 {
                            for raw in 0..=255u8 {
                                if mode == 2 && raw != 1 {
                                    continue;
                                }
                                let mut bytes = Vec::new();
                                if size == 2 {
                                    bytes.push(0x66);
                                }
                                if rex {
                                    bytes.push(0x40 | (reg >> 3));
                                }
                                bytes.push(
                                    (if mode == 0 {
                                        0xc0
                                    } else if mode == 1 {
                                        0xd2
                                    } else {
                                        0xd0
                                    }) + u8::from(size == 2),
                                );
                                bytes.push(0xc0 | (op << 3) | (reg & 7));
                                if mode == 0 {
                                    bytes.push(raw);
                                }
                                bytes.push(0xc3);
                                let code = arena
                                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                let values: Vec<u64> = if size == 1 {
                                    (0..256).collect()
                                } else {
                                    vec![0, 1, 15, 0x7fff, 0x8000, 0xfffe, 0xffff]
                                };
                                for value in values {
                                    let old = if value & 1 == 0 { 2 } else { 0xadff };
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: old,
                                        registers: [0x123456789abcdef0; 16],
                                        ..Cpu::default()
                                    };
                                    cpu.registers[slot] =
                                        (cpu.registers[slot] & !(mask << shift)) | (value << shift);
                                    if mode == 1 {
                                        cpu.registers[1] =
                                            (cpu.registers[1] & !255) | u64::from(raw);
                                    }
                                    let mut expected = cpu.registers;
                                    let input = (expected[slot] >> shift) & mask;
                                    let (result, flags) =
                                        reference(input, (size * 8) as u32, raw, op, old);
                                    expected[slot] =
                                        (expected[slot] & !(mask << shift)) | (result << shift);
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, expected,
                                        "register {size}/{rex}/{reg}/{op}/{mode}/{raw}/{value}"
                                    );
                                    assert_eq!(
                                        cpu.flags, flags,
                                        "flags {size}/{rex}/{reg}/{op}/{mode}/{raw}/{value}"
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
        println!("direct narrow shifts/rotates: {checks} native state checks passed");
    }
}
