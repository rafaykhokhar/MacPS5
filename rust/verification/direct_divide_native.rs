use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(dividend: u128, divisor: u64, bits: u32, signed: bool) -> Option<(u64, u64)> {
    let mask = u64::MAX >> (64 - bits);
    let divisor = divisor & mask;
    if divisor == 0 {
        return None;
    }
    let sign = 1u64 << (bits - 1);
    let full_mask = if bits == 64 {
        u128::MAX
    } else {
        (1u128 << (bits * 2)) - 1
    };
    let negative_dividend = signed && dividend & (1u128 << (bits * 2 - 1)) != 0;
    let negative_divisor = signed && divisor & sign != 0;
    let numerator = if negative_dividend {
        dividend.wrapping_neg() & full_mask
    } else {
        dividend
    };
    let denominator = u128::from(if negative_divisor {
        divisor.wrapping_neg() & mask
    } else {
        divisor
    });
    let (mut q, mut r) = (0u128, 0u128);
    for bit in (0..bits * 2).rev() {
        r = (r << 1) | ((numerator >> bit) & 1);
        q <<= 1;
        if r >= denominator {
            r -= denominator;
            q |= 1;
        }
    }
    let negative = negative_dividend != negative_divisor;
    let limit = if signed {
        if negative {
            u128::from(sign)
        } else {
            u128::from(sign - 1)
        }
    } else {
        u128::from(mask)
    };
    if q > limit {
        return None;
    }
    let q = if negative { q.wrapping_neg() } else { q } as u64 & mask;
    let r = if negative_dividend {
        r.wrapping_neg()
    } else {
        r
    } as u64
        & mask;
    Some((q, r))
}
fn main() {
    unsafe {
        let arena = JitArena::new(8 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [1usize, 2, 4, 8] {
            let bits = (size * 8) as u32;
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            for signed in [false, true] {
                for rex in [false, true] {
                    for src in 0..if rex { 16u8 } else { 8 } {
                        for mem in [false, true] {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            if rex || size == 8 {
                                bytes.push(0x40 | if size == 8 { 8 } else { 0 } | (src >> 3));
                            }
                            bytes.push(if size == 1 { 0xf6 } else { 0xf7 });
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 })
                                    | if signed { 0x38 } else { 0x30 }
                                    | (src & 7),
                            );
                            if mem {
                                if src & 7 == 4 {
                                    bytes.push(0x24);
                                }
                                bytes.push(0xf8);
                            }
                            let end = 0x100 + bytes.len() as u64;
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for (hi, lo) in [
                                (0u64, 0u64),
                                (0, 1),
                                (0, mask),
                                (1, 0),
                                (sign, 0),
                                (mask, mask),
                                (mask, sign),
                                (0, sign),
                            ] {
                                for divisor in [0u64, 1, 2, sign - 1, sign, mask] {
                                    for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX] {
                                        if !mem && address != 0x1000 {
                                            continue;
                                        }
                                        for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 9)] {
                                            let mut data = [0u8; 16];
                                            data[..8].copy_from_slice(&divisor.to_le_bytes());
                                            data[8..].copy_from_slice(&divisor.to_le_bytes());
                                            let original = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, false, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel,
                                                fault: prior,
                                                flags: 0xadff,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                registers: [0x123456789abcdef0; 16],
                                                ..Cpu::default()
                                            };
                                            if size == 1 {
                                                cpu.registers[0] =
                                                    (cpu.registers[0] & !65535) | (hi << 8) | lo;
                                            } else {
                                                cpu.registers[0] = (cpu.registers[0] & !mask) | lo;
                                                cpu.registers[2] = (cpu.registers[2] & !mask) | hi;
                                            }
                                            let (slot, shift) = if size == 1 && !rex && src >= 4 {
                                                ((src - 4) as usize, 8)
                                            } else {
                                                (src as usize, 0)
                                            };
                                            if mem {
                                                cpu.registers[src as usize] =
                                                    address.wrapping_add(8);
                                            } else {
                                                cpu.registers[slot] = (cpu.registers[slot]
                                                    & !(mask << shift))
                                                    | (divisor << shift);
                                            }
                                            let mut expected = cpu.registers;
                                            let mut fault = prior;
                                            let readable = !mem
                                                || (address >= 0x1000
                                                    && address
                                                        .checked_add(size as u64)
                                                        .is_some_and(|end| end <= 0x1010));
                                            let executed = fuel > 0 && prior == 0;
                                            if executed {
                                                if !readable {
                                                    fault = 1;
                                                } else {
                                                    let source = if mem {
                                                        let mut bytes = [0u8; 8];
                                                        let at = (address - 0x1000) as usize;
                                                        bytes[..size].copy_from_slice(
                                                            &original[at..at + size],
                                                        );
                                                        u64::from_le_bytes(bytes)
                                                    } else {
                                                        expected[slot] >> shift
                                                    };
                                                    let dividend = if size == 1 {
                                                        u128::from(expected[0] & 65535)
                                                    } else {
                                                        (u128::from(expected[2] & mask) << bits)
                                                            | u128::from(expected[0] & mask)
                                                    };
                                                    if let Some((q, r)) =
                                                        reference(dividend, source, bits, signed)
                                                    {
                                                        if size == 1 {
                                                            expected[0] = (expected[0] & !65535)
                                                                | q
                                                                | (r << 8);
                                                        } else if size == 2 {
                                                            expected[0] = (expected[0] & !mask) | q;
                                                            expected[2] = (expected[2] & !mask) | r;
                                                        } else {
                                                            expected[0] = q;
                                                            expected[2] = r;
                                                        }
                                                    } else {
                                                        fault = 13;
                                                    }
                                                }
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.registers, expected,
                                                "register {size}/{signed}/{rex}/{src}/{mem}/{hi}/{lo}/{divisor}/{address:x}"
                                            );
                                            assert_eq!(cpu.flags, 0xadff);
                                            assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                            assert_eq!(
                                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                                (
                                                    fault,
                                                    if fault != 0 {
                                                        0
                                                    } else if fuel < 2 {
                                                        4
                                                    } else {
                                                        3
                                                    },
                                                    if prior != 0 {
                                                        fuel
                                                    } else if executed && fault != 0 {
                                                        fuel - 1
                                                    } else {
                                                        0
                                                    },
                                                    if executed && fault == 0 {
                                                        end
                                                    } else {
                                                        0x100
                                                    }
                                                ),
                                                "status {size}/{signed}/{src}/{mem}/{hi}/{lo}/{divisor}"
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
        println!("direct DIV/IDIV: {checks} native state checks passed");
    }
}
