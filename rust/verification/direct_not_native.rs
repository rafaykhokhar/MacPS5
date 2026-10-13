use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
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
                            if size == 1 { 0xf6 } else { 0xf7 },
                            (if mem { 0x40 } else { 0xc0 }) | 0x10 | (encoded & 7),
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
                        for value in [
                            0u64,
                            1,
                            0x7f,
                            0x80,
                            0x7fff,
                            0x8000,
                            u64::MAX,
                            0x123456789abcdef0,
                        ] {
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
                                            flags: 0xa55b,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            registers: [value; 16],
                                            ..Cpu::default()
                                        };
                                        if mem {
                                            cpu.registers[encoded as usize] = address + 8;
                                        }
                                        let mut expected = cpu.registers;
                                        let executed = fuel > 0 && prior == 0;
                                        let readable = !mem || address + size as u64 <= 0x1010;
                                        let valid = readable && (!mem || writable);
                                        if executed && valid {
                                            if mem {
                                                let at = (address - 0x1000) as usize;
                                                for byte in &mut expected_data[at..at + size] {
                                                    *byte = 255 - *byte;
                                                }
                                            } else {
                                                let (reg, shift) =
                                                    if size == 1 && !rex && encoded >= 4 {
                                                        ((encoded - 4) as usize, 8)
                                                    } else {
                                                        (encoded as usize, 0)
                                                    };
                                                let mask = u64::MAX >> (64 - size * 8);
                                                let result =
                                                    mask - ((expected[reg] >> shift) & mask);
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
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                        assert_eq!(cpu.flags, 0xa55b);
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
        println!("direct NOT: {checks} native state checks passed");
    }
}
