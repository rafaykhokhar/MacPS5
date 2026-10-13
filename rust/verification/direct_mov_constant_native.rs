use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(2 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for reg in 0..16u8 {
                for immediate in [0u32, 1, 0x7fffffff, 0x80000000, u32::MAX] {
                    let value = if wide {
                        immediate as i32 as i64 as u64
                    } else {
                        u64::from(immediate)
                    };
                    for memory_operand in [false, true] {
                        let mut bytes = vec![
                            0x40 | if wide { 8 } else { 0 } | (reg >> 3),
                            0xc7,
                            if memory_operand {
                                0x40 | (reg & 7)
                            } else {
                                0xc0 | (reg & 7)
                            },
                        ];
                        if memory_operand {
                            if reg & 7 == 4 {
                                bytes.push(0x24);
                            }
                            bytes.push(0xf8);
                        }
                        bytes.extend_from_slice(&immediate.to_le_bytes());
                        bytes.push(0xc3);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for address in [0x1000u64, 0x1001, 0x1008, 0x100c, 0x100d, u64::MAX] {
                            for writable in [false, true] {
                                for fuel in [0, 1, 2] {
                                    for prior_fault in [0, 9] {
                                        let mut data = [0xa5; 16];
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, writable, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fuel,
                                            fault: prior_fault,
                                            flags: 0xa55a,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            ..Cpu::default()
                                        };
                                        cpu.registers.fill(0x123456789abcdef0);
                                        cpu.registers[reg as usize] = address.wrapping_add(8);
                                        let mut registers = cpu.registers;
                                        let mut expected = [0xa5; 16];
                                        let size = if wide { 8 } else { 4 };
                                        let valid = !memory_operand
                                            || (writable
                                                && address >= 0x1000
                                                && address
                                                    .checked_add(size)
                                                    .is_some_and(|end| end <= 0x1010));
                                        if prior_fault == 0 && fuel > 0 && valid {
                                            if memory_operand {
                                                let at = (address - 0x1000) as usize;
                                                expected[at..at + size as usize].copy_from_slice(
                                                    &value.to_le_bytes()[..size as usize],
                                                );
                                            } else {
                                                registers[reg as usize] = value;
                                            }
                                        }
                                        run(&mut cpu);
                                        assert_eq!(cpu.registers, registers);
                                        assert_eq!(cpu.flags, 0xa55a);
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected);
                                        assert_eq!(
                                            cpu.fault,
                                            if prior_fault != 0 {
                                                prior_fault
                                            } else if fuel > 0 && !valid {
                                                2
                                            } else {
                                                0
                                            }
                                        );
                                        assert_eq!(
                                            cpu.fuel,
                                            if prior_fault != 0 {
                                                fuel
                                            } else if !valid && fuel > 0 {
                                                fuel - 1
                                            } else {
                                                0
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
        // RIP uses the end of the entire instruction, including its immediate.
        for wide in [false, true] {
            let bytes = [
                if wide { 0x48 } else { 0x40 },
                0xc7,
                0x05,
                0xf5,
                6,
                0,
                0,
                0,
                0,
                0,
                0x80,
                0xc3,
            ];
            let code = arena
                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            let mut data = [0xa5; 16];
            let mut memory = Memory::default();
            memory.add(0x1000, &mut data, true, false).unwrap();
            let mut cpu = Cpu {
                rip: 0x100,
                bias: 0x800,
                fuel: 2,
                memory: (&mut memory as *mut Memory<'_>) as u64,
                ..Cpu::default()
            };
            run(&mut cpu);
            assert_eq!(cpu.fault, 0);
            let expected = if wide {
                0xffffffff80000000u64
            } else {
                0xa5a5a5a580000000
            };
            assert_eq!(memory.read(0x1000, 8).unwrap(), expected.to_le_bytes());
            checks += 1;
        }
        println!(
            "PASS: {checks} C7 immediate MOV register/memory cases, sign extension, all bases, unaligned/boundary/read-only failures, fuel and prior faults"
        );
    }
}
