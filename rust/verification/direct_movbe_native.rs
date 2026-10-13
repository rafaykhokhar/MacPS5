use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2usize, 4, 8] {
            for store in [false, true] {
                for reg in 0..16u8 {
                    for base in 0..16u8 {
                        let mut bytes = Vec::new();
                        if size == 2 {
                            bytes.push(0x66);
                        }
                        bytes.extend_from_slice(&[
                            0x40 | if size == 8 { 8 } else { 0 } | ((reg >> 3) << 2) | (base >> 3),
                            0x0f,
                            0x38,
                            if store { 0xf1 } else { 0xf0 },
                            0x40 | ((reg & 7) << 3) | (base & 7),
                        ]);
                        if base & 7 == 4 {
                            bytes.push(0x24);
                        }
                        bytes.push(0xf8);
                        let end = 0x100 + bytes.len() as u64;
                        bytes.push(0xc3);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for value in [0u64, 1, 0x80, 0x80000000, u64::MAX, 0x0123456789abcdef] {
                            for address in [
                                0x1000u64,
                                0x1001,
                                0x1010 - size as u64,
                                0x100f,
                                0x1010,
                                u64::MAX - 7,
                            ] {
                                for writable in [false, true] {
                                    for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 7)] {
                                        let mut data = [0u8; 16];
                                        for (n, b) in data.iter_mut().enumerate() {
                                            *b = (n as u8).wrapping_mul(13).wrapping_add(1);
                                        }
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
                                        cpu.registers[base as usize] = address.wrapping_add(8);
                                        let mut expected = cpu.registers;
                                        let executed = fuel > 0 && prior == 0;
                                        let valid = address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|end| end <= 0x1010)
                                            && (!store || writable);
                                        if executed && valid {
                                            let at = (address - 0x1000) as usize;
                                            if store {
                                                expected_data[at..at + size].copy_from_slice(
                                                    &expected[reg as usize].to_be_bytes()
                                                        [8 - size..],
                                                );
                                            } else {
                                                let result = expected_data[at..at + size]
                                                    .iter()
                                                    .fold(0u64, |n, b| n * 256 + *b as u64);
                                                expected[reg as usize] = if size == 2 {
                                                    (expected[reg as usize] & !65535) | result
                                                } else {
                                                    result
                                                };
                                            }
                                        }
                                        run(&mut cpu);
                                        assert_eq!(
                                            cpu.registers, expected,
                                            "{size}/{store}/{reg}/{base}/{address:x}"
                                        );
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                        assert_eq!(cpu.flags, 0xa55b);
                                        let fault = if executed && !valid {
                                            if store { 2 } else { 1 }
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
        for prefix in [0x64, 0x65, 0x67, 0xf0] {
            assert!(
                direct_cpu::translate(&[prefix, 0x0f, 0x38, 0xf0, 0x00, 0xc3], 0x100, 2).is_err()
            );
        }
        println!("direct MOVBE: {checks} native state checks passed");
    }
}
