use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for byte in [false, true] {
                for dest in 0..16u8 {
                    for source in 0..16u8 {
                        for memory_operand in [false, true] {
                            let mut bytes = vec![
                                0x40 | if wide { 8 } else { 0 }
                                    | ((dest >> 3) << 2)
                                    | (source >> 3),
                                0x0f,
                                if byte { 0xb6 } else { 0xb7 },
                                (if memory_operand { 0x40 } else { 0xc0 })
                                    | ((dest & 7) << 3)
                                    | (source & 7),
                            ];
                            if memory_operand {
                                if source & 7 == 4 {
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
                            for address in [0x1000u64, 0x1001, 0x100f, 0x1010] {
                                for value in [0u64, 0x123456789abc8001, u64::MAX] {
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
                                    cpu.registers.fill(value);
                                    if memory_operand {
                                        cpu.registers[source as usize] = address + 8;
                                    }
                                    let mut expected = cpu.registers;
                                    let size = if byte { 1 } else { 2 };
                                    let valid = !memory_operand || address + size <= 0x1010;
                                    if valid {
                                        expected[dest as usize] = if memory_operand {
                                            let at = (address - 0x1000) as usize;
                                            if byte {
                                                original[at] as u64
                                            } else {
                                                u16::from_le_bytes([original[at], original[at + 1]])
                                                    as u64
                                            }
                                        } else {
                                            value & if byte { 0xff } else { 0xffff }
                                        };
                                    }
                                    run(&mut cpu);
                                    assert_eq!(cpu.registers, expected);
                                    assert_eq!(cpu.flags, 0xa55a);
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
        // No REX: register encodings 4..7 select AH/CH/DH/BH.
        for dest in 0..8u8 {
            for source in 4..8u8 {
                let bytes = [0x0f, 0xb6, 0xc0 | (dest << 3) | source, 0xc3];
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                let mut cpu = Cpu {
                    rip: 0x100,
                    fuel: 2,
                    ..Cpu::default()
                };
                for (i, r) in cpu.registers.iter_mut().enumerate() {
                    *r = 0xabcdef0012340000 | ((0x80 + i as u64) << 8);
                }
                let mut expected = cpu.registers;
                expected[dest as usize] = (expected[(source - 4) as usize] >> 8) & 255;
                run(&mut cpu);
                assert_eq!(cpu.registers, expected);
                checks += 1;
            }
        }
        println!(
            "PASS: {checks} MOVZX byte/word source cases, 32/64-bit destinations, all register/base aliases, legacy high bytes, guarded boundaries and zero extension"
        );
    }
}
