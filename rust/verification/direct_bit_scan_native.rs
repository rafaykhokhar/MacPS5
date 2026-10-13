use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(value: u64, bits: usize, op: usize, old: u64) -> (Option<u64>, u64) {
    let set: Vec<usize> = (0..bits).filter(|bit| value & (1u64 << bit) != 0).collect();
    if op < 2 && set.is_empty() {
        return (None, old | 64);
    }
    let result = match op {
        0 => set[0],
        1 => *set.last().unwrap(),
        2 => set.last().map_or(bits, |bit| bits - 1 - bit),
        3 => set.first().copied().unwrap_or(bits),
        _ => set.len(),
    } as u64;
    let flags = if op < 2 {
        old & !64
    } else if op < 4 {
        (old & !65) | u64::from(set.is_empty()) | (u64::from(result == 0) << 6)
    } else {
        (old & !0x8d5) | (u64::from(set.is_empty()) << 6)
    };
    (Some(result), flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2usize, 4, 8] {
            let bits = size * 8;
            let mask = u64::MAX >> (64 - bits);
            for op in 0..5 {
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            if op >= 2 {
                                bytes.push(0xf3);
                            }
                            bytes.push(
                                0x40 | if size == 8 { 8 } else { 0 }
                                    | ((dst >> 3) << 2)
                                    | (src >> 3),
                            );
                            bytes.extend_from_slice(&[
                                0x0f,
                                if op == 4 {
                                    0xb8
                                } else if op == 0 || op == 3 {
                                    0xbc
                                } else {
                                    0xbd
                                },
                                (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7),
                            ]);
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
                            let mut values =
                                vec![0u64, u64::MAX, 0x5555555555555555, 0xaaaaaaaaaaaaaaaa];
                            values.extend((0..64).map(|b| 1u64 << b));
                            for value in values {
                                for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX] {
                                    if !mem && address != 0x1000 {
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
                                    if mem {
                                        cpu.registers[src as usize] = address.wrapping_add(8);
                                    } else {
                                        cpu.registers[src as usize] = value;
                                    }
                                    let mut expected = cpu.registers;
                                    let mut flags = cpu.flags;
                                    let valid = !mem
                                        || (address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|end| end <= 0x1010));
                                    if valid {
                                        let input = if mem {
                                            let mut raw = [0u8; 8];
                                            let at = (address - 0x1000) as usize;
                                            raw[..size].copy_from_slice(&original[at..at + size]);
                                            u64::from_le_bytes(raw)
                                        } else {
                                            expected[src as usize]
                                        };
                                        let (result, f) = reference(input, bits, op, flags);
                                        flags = f;
                                        if let Some(result) = result {
                                            expected[dst as usize] = if size == 2 {
                                                (expected[dst as usize] & !mask) | result
                                            } else {
                                                result
                                            };
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, expected,
                                        "register {size}/{op}/{dst}/{src}/{mem}/{value:x}/{address:x}"
                                    );
                                    assert_eq!(cpu.flags, flags, "flags {size}/{op}/{value:x}");
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                    assert_eq!(
                                        (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                        if valid {
                                            (0, 3, 0, end)
                                        } else {
                                            (1, 0, 1, 0x100)
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
        for (size, dst, op, prior, expected_fault) in [
            (0, 0, 0, 0, 3),
            (1, 0, 0, 0, 3),
            (8, 16, 0, 0, 3),
            (8, 0, 5, 0, 3),
            (8, 0, 0, 9, 9),
        ] {
            let mut cpu = Cpu {
                fault: prior,
                flags: 0xadff,
                registers: [u64::MAX; 16],
                ..Cpu::default()
            };
            native_ps5::bit_scan::execute(&mut cpu, 1, dst, size, op);
            assert_eq!(cpu.registers, [u64::MAX; 16]);
            assert_eq!(cpu.flags, 0xadff);
            assert_eq!(cpu.fault, expected_fault);
            checks += 1;
        }
        println!("direct bit scan/count: {checks} native/guard checks passed");
    }
}
