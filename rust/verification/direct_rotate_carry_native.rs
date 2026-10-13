use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(mut value: u64, bits: u32, raw: u64, right: bool, old: u64) -> (u64, u64) {
    let mask = u64::MAX >> (64 - bits);
    let sign = 1u64 << (bits - 1);
    value &= mask;
    let original = value;
    let masked = raw & if bits == 64 { 63 } else { 31 };
    let count = masked % u64::from(bits + 1);
    let mut carry = old & 1;
    for _ in 0..count {
        let next = if right {
            value & 1
        } else {
            u64::from(value & sign != 0)
        };
        value = if right {
            (value >> 1) | (carry << (bits - 1))
        } else {
            (value.wrapping_mul(2) & mask) | carry
        };
        carry = next;
    }
    let mut flags = (old & !1) | carry;
    if masked == 1 {
        let overflow = if right {
            u64::from(original & sign != 0) ^ (old & 1)
        } else {
            u64::from(value & sign != 0) ^ carry
        };
        flags = (flags & !0x800) | (overflow << 11);
    }
    (value, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(96 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [1usize, 2, 4, 8] {
            let mask = u64::MAX >> (64 - size * 8);
            let sign = 1u64 << (size * 8 - 1);
            for rex in [false, true] {
                for reg in 0..if rex { 16u8 } else { 8 } {
                    for memory_operand in [false, true] {
                        let (slot, shift) = if size == 1 && !rex && reg >= 4 {
                            ((reg - 4) as usize, 8)
                        } else {
                            (reg as usize, 0)
                        };
                        for right in [false, true] {
                            for mode in 0..3 {
                                for raw in 0..=255u8 {
                                    if mode == 2 && raw != 1 {
                                        continue;
                                    }
                                    let mut bytes = Vec::new();
                                    if size == 2 {
                                        bytes.push(0x66);
                                    }
                                    if rex || size == 8 {
                                        bytes.push(
                                            0x40 | if size == 8 { 8 } else { 0 } | (reg >> 3),
                                        );
                                    }
                                    bytes.push(
                                        (if mode == 0 {
                                            0xc0
                                        } else if mode == 1 {
                                            0xd2
                                        } else {
                                            0xd0
                                        }) + u8::from(size != 1),
                                    );
                                    bytes.push(
                                        (if memory_operand { 0x40 } else { 0xc0 })
                                            | ((if right { 3 } else { 2 }) << 3)
                                            | (reg & 7),
                                    );
                                    if memory_operand {
                                        if reg & 7 == 4 {
                                            bytes.push(0x24);
                                        }
                                        bytes.push(0xf8);
                                    }
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
                                    for value in [0u64, 1, sign - 1, sign, mask] {
                                        for old in [0xadfeu64, 0xadff] {
                                            for (address, writable) in [
                                                (0x1000u64, true),
                                                (0x1001, true),
                                                (0x100f, true),
                                                (0x1010, true),
                                                (u64::MAX, true),
                                                (0x1000, false),
                                            ] {
                                                if !memory_operand
                                                    && (address != 0x1000 || !writable)
                                                {
                                                    continue;
                                                }
                                                let mut data = [0u8; 16];
                                                data[..8].copy_from_slice(&value.to_le_bytes());
                                                data[8..].copy_from_slice(&value.to_le_bytes());
                                                let mut expected_data = data;
                                                let mut memory = Memory::default();
                                                memory
                                                    .add(0x1000, &mut data, writable, false)
                                                    .unwrap();
                                                let mut cpu = Cpu {
                                                    rip: 0x100,
                                                    fuel: 2,
                                                    flags: old,
                                                    memory: (&mut memory as *mut Memory<'_>) as u64,
                                                    registers: [0x123456789abcdef0; 16],
                                                    ..Cpu::default()
                                                };
                                                if !memory_operand {
                                                    cpu.registers[slot] = (cpu.registers[slot]
                                                        & !(mask << shift))
                                                        | (value << shift);
                                                }
                                                if mode == 1 {
                                                    cpu.registers[1] =
                                                        (cpu.registers[1] & !255) | u64::from(raw);
                                                }
                                                if memory_operand {
                                                    cpu.registers[reg as usize] =
                                                        address.wrapping_add(8);
                                                }
                                                let count = if mode == 1 {
                                                    cpu.registers[1]
                                                } else {
                                                    u64::from(raw)
                                                };
                                                let mut expected = cpu.registers;
                                                let readable = !memory_operand
                                                    || (address >= 0x1000
                                                        && address
                                                            .checked_add(size as u64)
                                                            .is_some_and(|end| end <= 0x1010));
                                                let valid = readable && writable;
                                                let mut flags = old;
                                                if valid {
                                                    let input = if memory_operand {
                                                        let mut bytes = [0u8; 8];
                                                        let at = (address - 0x1000) as usize;
                                                        bytes[..size].copy_from_slice(
                                                            &expected_data[at..at + size],
                                                        );
                                                        u64::from_le_bytes(bytes)
                                                    } else {
                                                        expected[slot] >> shift
                                                    };
                                                    let (result, f) = reference(
                                                        input,
                                                        (size * 8) as u32,
                                                        count,
                                                        right,
                                                        old,
                                                    );
                                                    flags = f;
                                                    if memory_operand {
                                                        let at = (address - 0x1000) as usize;
                                                        expected_data[at..at + size]
                                                            .copy_from_slice(
                                                                &result.to_le_bytes()[..size],
                                                            );
                                                    } else {
                                                        expected[slot] = if size == 4 {
                                                            result
                                                        } else {
                                                            (expected[slot] & !(mask << shift))
                                                                | (result << shift)
                                                        };
                                                    }
                                                }
                                                run(&mut cpu);
                                                assert_eq!(
                                                    cpu.registers, expected,
                                                    "register {size}/{rex}/{reg}/{right}/{mode}/{raw}/{memory_operand}"
                                                );
                                                assert_eq!(
                                                    cpu.flags, flags,
                                                    "flags {size}/{rex}/{reg}/{right}/{mode}/{raw}/{memory_operand}"
                                                );
                                                assert_eq!(
                                                    memory.read(0x1000, 16).unwrap(),
                                                    expected_data
                                                );
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
            }
        }
        for right in [false, true] {
            for value in 0..256u64 {
                for count in 0..256u64 {
                    for old in [0xadfe, 0xadff] {
                        assert_eq!(
                            native_ps5::shift_flags::rotate_carry(old, value, count, 8, right),
                            Some(reference(value, 8, count, right, old))
                        );
                        checks += 1;
                    }
                }
            }
        }
        assert_eq!(
            native_ps5::shift_flags::rotate_carry(2, 0, 0, 0, false),
            None
        );
        println!("direct RCL/RCR: {checks} native/pure reference checks passed");
    }
}
