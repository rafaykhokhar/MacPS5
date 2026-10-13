use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(mut value: u64, bits: u32, raw: u64, op: u8, old: u64) -> (u64, u64) {
    let mask = u64::MAX >> (64 - bits);
    let sign = 1u64 << (bits - 1);
    value &= mask;
    let original = value;
    let count = raw & if bits == 64 { 63 } else { 31 };
    let mut carry = old & 1;
    for _ in 0..count {
        if op == 0 {
            carry = u64::from(value & sign != 0);
            value = (value.wrapping_mul(2) & mask) | carry;
        } else if op == 1 {
            carry = value & 1;
            value = value / 2 | (carry << (bits - 1));
        } else if op == 4 {
            carry = u64::from(value & sign != 0);
            value = value.wrapping_mul(2) & mask;
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
        for size in [1usize, 2, 4, 8] {
            let sign = 1u64 << (size * 8 - 1);
            for base in 0..16u8 {
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
                            bytes.push(0x40 | if size == 8 { 8 } else { 0 } | (base >> 3));
                            bytes.push(
                                (if mode == 0 {
                                    0xc0
                                } else if mode == 1 {
                                    0xd2
                                } else {
                                    0xd0
                                }) + u8::from(size != 1),
                            );
                            bytes.push(0x40 | (op << 3) | (base & 7));
                            if base & 7 == 4 {
                                bytes.push(0x24);
                            }
                            bytes.push(0xf8);
                            if mode == 0 {
                                bytes.push(raw);
                            }
                            let end = 0x100 + bytes.len() as u64;
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for value in [0u64, 1, sign - 1, sign, u64::MAX] {
                                for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX] {
                                    for writable in [false, true] {
                                        let mut data = [0u8; 16];
                                        data[..8].copy_from_slice(&value.to_le_bytes());
                                        data[8..].copy_from_slice(&value.to_le_bytes());
                                        let mut expected_data = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, writable, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fuel: 2,
                                            flags: 0xadff,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            registers: [0x123456789abcdef0; 16],
                                            ..Cpu::default()
                                        };
                                        cpu.registers[1] = 0xfedcba9876543200 | u64::from(raw);
                                        cpu.registers[base as usize] = address.wrapping_add(8);
                                        let registers = cpu.registers;
                                        let count = if mode == 1 {
                                            registers[1]
                                        } else {
                                            u64::from(raw)
                                        };
                                        let readable = address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|end| end <= 0x1010);
                                        let valid = readable && writable;
                                        let mut flags = cpu.flags;
                                        if valid {
                                            let at = (address - 0x1000) as usize;
                                            let mut input = [0u8; 8];
                                            input[..size]
                                                .copy_from_slice(&expected_data[at..at + size]);
                                            let (result, f) = reference(
                                                u64::from_le_bytes(input),
                                                (size * 8) as u32,
                                                count,
                                                op,
                                                flags,
                                            );
                                            flags = f;
                                            expected_data[at..at + size]
                                                .copy_from_slice(&result.to_le_bytes()[..size]);
                                        }
                                        run(&mut cpu);
                                        assert_eq!(cpu.registers, registers);
                                        assert_eq!(
                                            cpu.flags, flags,
                                            "flags {size}/{base}/{op}/{mode}/{raw}/{value}/{address:x}/{writable}"
                                        );
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                        assert_eq!(
                                            (cpu.fault, cpu.fuel, cpu.stop, cpu.rip),
                                            if valid {
                                                (0, 0, 3, end)
                                            } else {
                                                (if readable { 2 } else { 1 }, 1, 0, 0x100)
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
        println!("direct memory shifts/rotates: {checks} native state checks passed");
    }
}
