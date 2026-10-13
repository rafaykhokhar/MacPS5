use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2u64, 4, 8] {
            let wide = size == 8;
            let bits = size * 8;
            let modulus = 1u128 << bits;
            let mask = (modulus - 1) as u64;
            let sign = 1u64 << (bits - 1);
            for test in [false, true] {
                for mode in 0..3 {
                    if test && mode == 1 {
                        continue;
                    }
                    for reg in 0..16u8 {
                        for base in 0..16u8 {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            bytes.push(
                                0x40 | if wide { 8 } else { 0 }
                                    | if mode == 0 { (reg >> 3) << 2 } else { 0 }
                                    | (base >> 3),
                            );
                            bytes.push(if mode == 0 {
                                if test { 0x85 } else { 0x39 }
                            } else if test {
                                0xf7
                            } else if mode == 1 {
                                0x83
                            } else {
                                0x81
                            });
                            let field = if mode == 0 {
                                reg & 7
                            } else if test {
                                0
                            } else {
                                7
                            };
                            bytes.push(0x40 | (field << 3) | (base & 7));
                            if base & 7 == 4 {
                                bytes.push(0x24);
                            }
                            bytes.push(0xf8);
                            let mut immediate = [
                                0u64,
                                1,
                                15,
                                127,
                                128,
                                255,
                                32767,
                                32768,
                                65535,
                                0x7fffffff,
                                0x80000000,
                                0xffffffff,
                                u64::MAX,
                                16,
                                0x55,
                                0xaa,
                            ][reg as usize];
                            if mode != 0 {
                                let n = if mode == 1 {
                                    1
                                } else if size == 2 {
                                    2
                                } else {
                                    4
                                };
                                bytes.extend_from_slice(&immediate.to_le_bytes()[..n]);
                                if n == 1 {
                                    immediate = immediate as i8 as i64 as u64;
                                } else if size == 8 {
                                    immediate = immediate as i32 as i64 as u64;
                                }
                            }
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for a in [0u64, 1, sign - 1, sign, mask] {
                                for b in [0u64, 1, sign, mask] {
                                    for address in [0x1000u64, 0x1001, 0x100d, u64::MAX] {
                                        let mut data = [0u8; 16];
                                        data[..8].copy_from_slice(&a.to_le_bytes());
                                        data[8..].copy_from_slice(&a.to_le_bytes());
                                        let original = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, false, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            flags: 0xa55b,
                                            fuel: 2,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            ..Cpu::default()
                                        };
                                        cpu.registers.fill(b);
                                        cpu.registers[base as usize] = address.wrapping_add(8);
                                        let registers = cpu.registers;
                                        let valid = address >= 0x1000
                                            && address
                                                .checked_add(size)
                                                .is_some_and(|end| end <= 0x1010);
                                        let mut flags = cpu.flags;
                                        if valid {
                                            let mut operand = [0u8; 8];
                                            let at = (address - 0x1000) as usize;
                                            operand[..size as usize]
                                                .copy_from_slice(&original[at..at + size as usize]);
                                            let a = u64::from_le_bytes(operand);
                                            let b = (if mode == 0 {
                                                registers[reg as usize]
                                            } else {
                                                immediate
                                            }) & mask;
                                            let result = if test {
                                                a & b
                                            } else {
                                                a.wrapping_sub(b) & mask
                                            };
                                            flags = (flags & !0x8d5) | 2;
                                            if !test && a < b {
                                                flags |= 1;
                                            }
                                            if (result as u8).count_ones() % 2 == 0 {
                                                flags |= 4;
                                            }
                                            if !test && (a & 15) < (b & 15) {
                                                flags |= 16;
                                            }
                                            if result == 0 {
                                                flags |= 64;
                                            }
                                            if result & sign != 0 {
                                                flags |= 128;
                                            }
                                            let signed = |v: u64| {
                                                if v & sign == 0 {
                                                    v as i128
                                                } else {
                                                    v as i128 - modulus as i128
                                                }
                                            };
                                            let difference = signed(a) - signed(b);
                                            if !test
                                                && (difference < -(sign as i128)
                                                    || difference >= sign as i128)
                                            {
                                                flags |= 2048;
                                            }
                                        }
                                        run(&mut cpu);
                                        assert_eq!(cpu.registers, registers);
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
        }
        println!(
            "PASS: {checks} memory-first CMP/TEST cases with independent flag oracle, register/immediate operands at 16/32/64 bits, all base aliases, read-only/unaligned operands and preserved state on faults"
        );
    }
}
