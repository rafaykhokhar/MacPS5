use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(value: u64, size: usize, old: u64, decrement: bool) -> (u64, u64) {
    let modulus = 1u128 << (size * 8);
    let mask = (modulus - 1) as u64;
    let value = value & mask;
    let result = if decrement {
        value.wrapping_sub(1)
    } else {
        value.wrapping_add(1)
    } & mask;
    let sign = 1u64 << (size * 8 - 1);
    let mut flags = (old & !0x8d4) | 2;
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    if (decrement && value % 16 == 0) || (!decrement && value % 16 == 15) {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result & sign != 0 {
        flags |= 128;
    }
    if value == if decrement { sign } else { sign - 1 } {
        flags |= 2048;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for decrement in [false, true] {
            for size in [1usize, 2, 4, 8] {
                for rex in [false, true] {
                    for encoded in 0..if rex { 16u8 } else { 8 } {
                        for mem in [false, true] {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            if rex || size == 8 {
                                bytes.push(0x40 | if size == 8 { 8 } else { 0 } | (encoded >> 3));
                            }
                            bytes.extend_from_slice(&[
                                if size == 1 { 0xfe } else { 0xff },
                                (if mem { 0x40 } else { 0xc0 })
                                    | if decrement { 8 } else { 0 }
                                    | (encoded & 7),
                            ]);
                            if mem {
                                if encoded & 7 == 4 {
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
                            if mem {
                                let mut locked = vec![0xf0];
                                locked.extend_from_slice(&bytes);
                                assert!(direct_cpu::translate(&locked, 0x100, 2).is_err());
                            }
                            let mut values = vec![
                                0u64,
                                1,
                                0x7f,
                                0x80,
                                0x7fff,
                                0x8000,
                                0x80000000,
                                0x7fffffff,
                                0x8000000000000000,
                                0x7fffffffffffffff,
                                u64::MAX,
                                0x123456789abcdef0,
                            ];
                            if size == 1 {
                                values.extend((0..256u64).map(|n| n * 0x0101010101010101));
                            }
                            for value in values {
                                for address in [0x1000u64, 0x1001, 0x100f, 0x1010] {
                                    for writable in [false, true] {
                                        for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 7)] {
                                            if !mem && (address != 0x1000 || writable) {
                                                continue;
                                            }
                                            let mut data = [0u8; 16];
                                            data[..8].copy_from_slice(&value.to_le_bytes());
                                            data[8..].copy_from_slice(&value.to_le_bytes());
                                            let mut expected_data = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, writable, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel,
                                                fault: prior,
                                                flags: 0xa55a | (value & 1),
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                registers: [value; 16],
                                                ..Cpu::default()
                                            };
                                            if mem {
                                                cpu.registers[encoded as usize] = address + 8;
                                            }
                                            let mut expected = cpu.registers;
                                            let mut flags = cpu.flags;
                                            let executed = fuel > 0 && prior == 0;
                                            let readable = !mem || address + size as u64 <= 0x1010;
                                            let valid = readable && (!mem || writable);
                                            if executed && valid {
                                                if mem {
                                                    let at = (address - 0x1000) as usize;
                                                    let mut raw = [0u8; 8];
                                                    raw[..size].copy_from_slice(
                                                        &expected_data[at..at + size],
                                                    );
                                                    let (result, f) = reference(
                                                        u64::from_le_bytes(raw),
                                                        size,
                                                        flags,
                                                        decrement,
                                                    );
                                                    flags = f;
                                                    expected_data[at..at + size].copy_from_slice(
                                                        &result.to_le_bytes()[..size],
                                                    );
                                                } else {
                                                    let (reg, shift) =
                                                        if size == 1 && !rex && encoded >= 4 {
                                                            ((encoded - 4) as usize, 8)
                                                        } else {
                                                            (encoded as usize, 0)
                                                        };
                                                    let mask = u64::MAX >> (64 - size * 8);
                                                    let (result, f) = reference(
                                                        expected[reg] >> shift,
                                                        size,
                                                        flags,
                                                        decrement,
                                                    );
                                                    flags = f;
                                                    expected[reg] = if size == 4 {
                                                        result
                                                    } else {
                                                        (expected[reg] & !(mask << shift))
                                                            | (result << shift)
                                                    };
                                                }
                                            }
                                            run(&mut cpu);
                                            assert_eq!(cpu.registers, expected);
                                            assert_eq!(
                                                memory.read(0x1000, 16).unwrap(),
                                                expected_data
                                            );
                                            assert_eq!(cpu.flags, flags);
                                            let fault = if executed && !valid {
                                                if readable { 2 } else { 1 }
                                            } else {
                                                prior
                                            };
                                            let stop = if fault != 0 {
                                                0
                                            } else if fuel < 2 {
                                                4
                                            } else {
                                                3
                                            };
                                            let remaining = if prior != 0 {
                                                fuel
                                            } else if executed && !valid {
                                                fuel - 1
                                            } else {
                                                0
                                            };
                                            assert_eq!(
                                                (cpu.fault, cpu.stop, cpu.fuel),
                                                (fault, stop, remaining)
                                            );
                                            assert_eq!(
                                                cpu.rip,
                                                if executed && valid { end } else { 0x100 }
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
        println!("direct INC/DEC: {checks} native state checks passed");
    }
}
