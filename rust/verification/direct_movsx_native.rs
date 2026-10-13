use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn extend(value: u64, source: usize, dest: usize, old: u64) -> u64 {
    let mask = (1u64 << (source * 8)) - 1;
    let low = value & mask;
    let signed = if low & (1 << (source * 8 - 1)) != 0 {
        low as i128 - (1i128 << (source * 8))
    } else {
        low as i128
    };
    match dest {
        2 => (old & !65535) | (signed as u64 & 65535),
        4 => signed as u32 as u64,
        _ => signed as u64,
    }
}
fn main() {
    unsafe {
        let arena = JitArena::new(8 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for (source_size, dest_size) in [(1, 2), (1, 4), (1, 8), (2, 4), (2, 8), (4, 8)] {
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    for mem in [false, true] {
                        let mut bytes = Vec::new();
                        if dest_size == 2 {
                            bytes.push(0x66);
                        }
                        bytes.push(
                            0x40 | if dest_size == 8 { 8 } else { 0 }
                                | ((dst >> 3) << 2)
                                | (src >> 3),
                        );
                        if source_size == 4 {
                            bytes.push(0x63);
                        } else {
                            bytes.extend_from_slice(&[
                                0x0f,
                                if source_size == 1 { 0xbe } else { 0xbf },
                            ]);
                        }
                        bytes.push((if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7));
                        if mem {
                            if src & 7 == 4 {
                                bytes.push(0x24);
                            }
                            bytes.push(0xf8);
                        }
                        bytes.push(0xc3);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for value in [
                            0u64,
                            0x7f,
                            0x80,
                            0x7fff,
                            0x8000,
                            0x7fffffff,
                            0x80000000,
                            u64::MAX,
                        ] {
                            for address in [0x1000u64, 0x1001, 0x100c, 0x100e, 0x100f, u64::MAX - 7]
                            {
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
                                    flags: 0xa55a,
                                    memory: (&mut memory as *mut Memory<'_>) as u64,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0x123456789abcdef0);
                                cpu.registers[src as usize] = value;
                                if mem {
                                    cpu.registers[src as usize] = address.wrapping_add(8);
                                }
                                let mut expected = cpu.registers;
                                let valid = !mem
                                    || (address >= 0x1000
                                        && address
                                            .checked_add(source_size as u64)
                                            .is_some_and(|end| end <= 0x1010));
                                if valid {
                                    let v = if mem {
                                        let at = (address - 0x1000) as usize;
                                        let mut raw = [0u8; 8];
                                        raw[..source_size]
                                            .copy_from_slice(&original[at..at + source_size]);
                                        u64::from_le_bytes(raw)
                                    } else {
                                        value
                                    };
                                    expected[dst as usize] =
                                        extend(v, source_size, dest_size, expected[dst as usize]);
                                }
                                run(&mut cpu);
                                assert_eq!(
                                    cpu.registers, expected,
                                    "{source_size}/{dest_size}/{dst}/{src}/{mem}/{value:x}/{address:x}"
                                );
                                assert_eq!(cpu.flags, 0xa55a);
                                assert_eq!(
                                    (cpu.fault, cpu.fuel, cpu.stop),
                                    if valid { (0, 0, 3) } else { (1, 1, 0) }
                                );
                                assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        for dest_size in [2, 4] {
            for dst in 0..8u8 {
                for src in 4..8u8 {
                    let mut bytes = Vec::new();
                    if dest_size == 2 {
                        bytes.push(0x66);
                    }
                    bytes.extend_from_slice(&[0x0f, 0xbe, 0xc0 | (dst << 3) | src, 0xc3]);
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for value in 0..256u64 {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            fuel: 2,
                            ..Cpu::default()
                        };
                        cpu.registers.fill(0xabcdef0012340000 | (value << 8));
                        let mut expected = cpu.registers;
                        expected[dst as usize] =
                            extend(value, 1, dest_size, expected[dst as usize]);
                        run(&mut cpu);
                        assert_eq!(cpu.registers, expected);
                        checks += 1;
                    }
                }
            }
        }
        println!("direct signed moves: {checks} native state checks passed");
    }
}
