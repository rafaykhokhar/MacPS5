use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn product(a: u64, b: u64, bits: u32) -> i128 {
    let mask = u64::MAX >> (64 - bits);
    let sign = 1u64 << (bits - 1);
    let a = a & mask;
    let b = b & mask;
    let magnitude = |v: u64| {
        if v & sign != 0 {
            v.wrapping_neg() & mask
        } else {
            v
        }
    };
    let x = magnitude(a);
    let y = magnitude(b);
    let mut p = 0i128;
    for bit in 0..bits {
        if y & (1u64 << bit) != 0 {
            p += i128::from(x) << bit;
        }
    }
    if (a ^ b) & sign != 0 { -p } else { p }
}
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2usize, 4, 8] {
            let bits = (size * 8) as u32;
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    for memory_operand in [false, true] {
                        for mode in 0..3 {
                            for immediate in [0u64, 1, 127, 128, 255, 32768, 0x80000000, u64::MAX] {
                                if mode == 0 && immediate != 0 {
                                    continue;
                                }
                                let mut bytes = Vec::new();
                                if size == 2 {
                                    bytes.push(0x66);
                                }
                                bytes.push(
                                    0x40 | if size == 8 { 8 } else { 0 }
                                        | ((dst >> 3) << 2)
                                        | (src >> 3),
                                );
                                if mode == 0 {
                                    bytes.extend_from_slice(&[0x0f, 0xaf]);
                                } else {
                                    bytes.push(if mode == 1 { 0x6b } else { 0x69 });
                                }
                                bytes.push(
                                    (if memory_operand { 0x40 } else { 0xc0 })
                                        | ((dst & 7) << 3)
                                        | (src & 7),
                                );
                                if memory_operand {
                                    if src & 7 == 4 {
                                        bytes.push(0x24);
                                    }
                                    bytes.push(0xf8);
                                }
                                let rhs = if mode == 1 {
                                    bytes.push(immediate as u8);
                                    immediate as i8 as i64 as u64
                                } else if mode == 2 {
                                    let n = if size == 2 { 2 } else { 4 };
                                    bytes.extend_from_slice(&immediate.to_le_bytes()[..n]);
                                    if size == 8 {
                                        immediate as i32 as i64 as u64
                                    } else {
                                        immediate
                                    }
                                } else {
                                    0
                                };
                                let end = 0x100 + bytes.len() as u64;
                                bytes.push(0xc3);
                                let code = arena
                                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                for value in [0u64, 1, 2, sign - 1, sign, mask] {
                                    for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX] {
                                        if !memory_operand && address != 0x1000 {
                                            continue;
                                        }
                                        let mut data = [0u8; 16];
                                        data[..8].copy_from_slice(&value.to_le_bytes());
                                        data[8..].copy_from_slice(&value.to_le_bytes());
                                        let original = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, false, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fuel: 2,
                                            flags: 0xadff,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            registers: [0x123456789abcdef0; 16],
                                            ..Cpu::default()
                                        };
                                        cpu.registers[dst as usize] =
                                            (cpu.registers[dst as usize] & !mask) | (value & mask);
                                        if memory_operand {
                                            cpu.registers[src as usize] = address.wrapping_add(8);
                                        } else {
                                            cpu.registers[src as usize] =
                                                (cpu.registers[src as usize] & !mask)
                                                    | ((!value) & mask);
                                        }
                                        let mut expected = cpu.registers;
                                        let mut flags = cpu.flags;
                                        let valid = !memory_operand
                                            || (address >= 0x1000
                                                && address
                                                    .checked_add(size as u64)
                                                    .is_some_and(|end| end <= 0x1010));
                                        if valid {
                                            let input = if memory_operand {
                                                let mut bytes = [0u8; 8];
                                                let at = (address - 0x1000) as usize;
                                                bytes[..size]
                                                    .copy_from_slice(&original[at..at + size]);
                                                u64::from_le_bytes(bytes)
                                            } else {
                                                expected[src as usize]
                                            };
                                            let p = if mode == 0 {
                                                product(expected[dst as usize], input, bits)
                                            } else {
                                                product(input, rhs, bits)
                                            };
                                            let result = p as u64 & mask;
                                            let overflow = p < -(sign as i128) || p >= sign as i128;
                                            flags =
                                                (flags & !0x801) | if overflow { 0x801 } else { 0 };
                                            expected[dst as usize] = if size == 2 {
                                                (expected[dst as usize] & !mask) | result
                                            } else {
                                                result
                                            };
                                        }
                                        run(&mut cpu);
                                        assert_eq!(
                                            cpu.registers, expected,
                                            "register {size}/{dst}/{src}/{memory_operand}/{mode}/{immediate}/{value}/{address:x}"
                                        );
                                        assert_eq!(
                                            cpu.flags, flags,
                                            "flags {size}/{dst}/{src}/{memory_operand}/{mode}/{immediate}/{value}"
                                        );
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                        assert_eq!(
                                            (cpu.fault, cpu.fuel, cpu.stop, cpu.rip),
                                            if valid {
                                                (0, 0, 3, end)
                                            } else {
                                                (1, 1, 0, 0x100)
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
        println!("direct truncated IMUL: {checks} native state checks passed");
    }
}
