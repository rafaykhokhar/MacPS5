use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2u64, 4, 8] {
            let wide = size == 8;
            let bits = size * 8;
            let modulus = 1u128 << bits;
            let mask = (modulus - 1) as u64;
            let sign = 1u64 << (bits - 1);
            for (op, opcode) in [0x03, 0x2b, 0x3b, 0x23, 0x0b, 0x33].into_iter().enumerate() {
                for dst in 0..16u8 {
                    for base in 0..16u8 {
                        let mut bytes = vec![
                            0x40 | if wide { 8 } else { 0 } | ((dst >> 3) << 2) | (base >> 3),
                            opcode,
                            0x40 | ((dst & 7) << 3) | (base & 7),
                        ];
                        if size == 2 {
                            bytes.insert(0, 0x66);
                        }
                        if base & 7 == 4 {
                            bytes.push(0x24);
                        }
                        bytes.extend_from_slice(&[0xf8, 0xc3]);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for a in [0u64, sign - 1, sign, mask] {
                            for b in [0u64, 1, sign, mask] {
                                for address in [0x1000u64, 0x1001, 0x100d, u64::MAX] {
                                    let mut data = [0u8; 16];
                                    data[..8].copy_from_slice(&b.to_le_bytes());
                                    data[8..].copy_from_slice(&b.to_le_bytes());
                                    let original = data;
                                    let valid = address >= 0x1000
                                        && address
                                            .checked_add(size)
                                            .is_some_and(|end| end <= 0x1010);
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, false, false).unwrap();
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xa55a,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        ..Cpu::default()
                                    };
                                    cpu.registers.fill(a);
                                    cpu.registers[base as usize] = address.wrapping_add(8);
                                    let mut expected = cpu.registers;
                                    let mut flags = cpu.flags;
                                    if valid {
                                        let a = expected[dst as usize] & mask;
                                        let offset = (address - 0x1000) as usize;
                                        let mut operand = [0u8; 8];
                                        operand[..size as usize].copy_from_slice(
                                            &original[offset..offset + size as usize],
                                        );
                                        let b = u64::from_le_bytes(operand);
                                        let result = match op {
                                            0 => a.wrapping_add(b),
                                            1 | 2 => a.wrapping_sub(b),
                                            3 => a & b,
                                            4 => a | b,
                                            5 => a ^ b,
                                            _ => unreachable!(),
                                        } & mask;
                                        let signed = |v: u64| {
                                            if v & sign == 0 {
                                                v as i128
                                            } else {
                                                v as i128 - modulus as i128
                                            }
                                        };
                                        let exact = if op == 0 {
                                            signed(a) + signed(b)
                                        } else {
                                            signed(a) - signed(b)
                                        };
                                        let overflow = op <= 2
                                            && (exact < -(sign as i128) || exact >= sign as i128);
                                        let carry = if op == 0 {
                                            a as u128 + b as u128 >= modulus
                                        } else {
                                            op <= 2 && a < b
                                        };
                                        flags = (flags & !0x8d5) | 2;
                                        if carry {
                                            flags |= 1;
                                        }
                                        if (result as u8).count_ones() % 2 == 0 {
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
                                        if result & sign != 0 {
                                            flags |= 128;
                                        }
                                        if overflow {
                                            flags |= 2048;
                                        }
                                        if op != 2 {
                                            expected[dst as usize] = if size == 2 {
                                                (expected[dst as usize] & !mask) | result
                                            } else {
                                                result
                                            };
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(cpu.registers, expected);
                                    assert_eq!(cpu.flags, flags);
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                    assert_eq!(
                                        (cpu.fault, cpu.fuel, cpu.stop),
                                        if valid { (0, 0, 3) } else { (1, 1, 0) }
                                    );
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        println!(
            "PASS: {checks} guarded register/memory arithmetic cases with independent result/flags oracle, aliases, unaligned reads and unchanged state on faults"
        );
    }
}
