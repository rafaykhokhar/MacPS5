//! Native direct legacy SSE and VEX moves: direction, upper lanes, alignment and faults.
use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for legacy in [false, true] {
            // pp, load opcode, alignment requirement; stores use opcode + 1.
            for (pp, opcode, aligned) in [
                (0, 0x10, false),
                (1, 0x10, false),
                (0, 0x28, true),
                (1, 0x28, true),
                (1, 0x6f, true),
                (2, 0x6f, false),
            ] {
                for wide in 0..2u8 {
                    if legacy && wide != 0 {
                        continue;
                    }
                    let size = if wide == 0 { 16 } else { 32 };
                    for store in [false, true] {
                        // DQA/DQU store opcode is 7F rather than 70.
                        let operation = if store {
                            if opcode == 0x6f { 0x7f } else { opcode + 1 }
                        } else {
                            opcode
                        };
                        for reg in 0..16u8 {
                            for rm in 0..16u8 {
                                for mem in [false, true] {
                                    let mut bytes = if legacy {
                                        let mut b = Vec::new();
                                        if pp != 0 {
                                            b.push(if pp == 1 { 0x66 } else { 0xf3 });
                                        }
                                        b.extend_from_slice(&[
                                            0x40 | ((reg >> 3) << 2) | (rm >> 3),
                                            0x0f,
                                            operation,
                                            (if mem { 0x40 } else { 0xc0 })
                                                | ((reg & 7) << 3)
                                                | (rm & 7),
                                        ]);
                                        b
                                    } else {
                                        vec![
                                            0xc4,
                                            0x41 | if reg < 8 { 0x80 } else { 0 }
                                                | if rm < 8 { 0x20 } else { 0 },
                                            0x78 | pp | (wide * 4),
                                            operation,
                                            (if mem { 0x40 } else { 0xc0 })
                                                | ((reg & 7) << 3)
                                                | (rm & 7),
                                        ]
                                    };
                                    if mem {
                                        if rm & 7 == 4 {
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
                                    for (fault, fuel) in [(0, 0), (9, 2)] {
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fault,
                                            fuel,
                                            flags: 0xadff,
                                            mxcsr: 0x9fc1,
                                            registers: [0xabcdef; 16],
                                            vectors: [[0xa5; 32]; 16],
                                            ..Cpu::default()
                                        };
                                        run(&mut cpu);
                                        assert_eq!(cpu.registers, [0xabcdef; 16]);
                                        assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                                        assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                                        assert_eq!(
                                            (cpu.rip, cpu.fault, cpu.fuel, cpu.stop),
                                            (0x100, fault, fuel, if fault == 0 { 4 } else { 0 })
                                        );
                                        checks += 1;
                                    }
                                    for address in [
                                        0x1000u64,
                                        0x1001,
                                        0x1020,
                                        0x1030,
                                        0x1031,
                                        0x1040,
                                        u64::MAX - 7,
                                    ] {
                                        for writable in [false, true] {
                                            if !mem && (address != 0x1000 || writable) {
                                                continue;
                                            }
                                            let mut data = [0x5a; 64];
                                            let mut expected_data = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, writable, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel: 2,
                                                flags: 0xa55a,
                                                mxcsr: 0x9fc1,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                ..Cpu::default()
                                            };
                                            cpu.registers.fill(0x12345678);
                                            cpu.registers[rm as usize] = address.wrapping_add(8);
                                            for (r, vector) in cpu.vectors.iter_mut().enumerate() {
                                                for (n, b) in vector.iter_mut().enumerate() {
                                                    *b = (r as u8)
                                                        .wrapping_mul(73)
                                                        .wrapping_add(n as u8 * 7);
                                                }
                                            }
                                            let registers = cpu.registers;
                                            let mut expected = cpu.vectors;
                                            let valid = !mem
                                                || (address >= 0x1000
                                                    && address
                                                        .checked_add(size as u64)
                                                        .is_some_and(|end| end <= 0x1040)
                                                    && (!aligned || address % size as u64 == 0)
                                                    && (!store || writable));
                                            if valid {
                                                if mem && store {
                                                    let at = (address - 0x1000) as usize;
                                                    expected_data[at..at + size].copy_from_slice(
                                                        &expected[reg as usize][..size],
                                                    );
                                                } else {
                                                    let (dst, src) =
                                                        if store { (rm, reg) } else { (reg, rm) };
                                                    let mut result = if legacy {
                                                        expected[dst as usize]
                                                    } else {
                                                        [0; 32]
                                                    };
                                                    if mem {
                                                        let at = (address - 0x1000) as usize;
                                                        result[..size].copy_from_slice(
                                                            &expected_data[at..at + size],
                                                        );
                                                    } else {
                                                        result[..size].copy_from_slice(
                                                            &expected[src as usize][..size],
                                                        );
                                                    }
                                                    expected[dst as usize] = result;
                                                }
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.vectors, expected,
                                                "{legacy}/{pp}/{opcode}/{wide}/{store}/{reg}/{rm}/{mem}/{address:x}"
                                            );
                                            assert_eq!(
                                                memory.read(0x1000, 64).unwrap(),
                                                expected_data
                                            );
                                            assert_eq!(cpu.registers, registers);
                                            assert_eq!((cpu.flags, cpu.mxcsr), (0xa55a, 0x9fc1));
                                            assert_eq!(
                                                cpu.rip,
                                                if valid {
                                                    0x100 + bytes.len() as u64 - 1
                                                } else {
                                                    0x100
                                                }
                                            );
                                            assert_eq!(
                                                (cpu.fault, cpu.fuel, cpu.stop),
                                                if valid {
                                                    (0, 0, 3)
                                                } else {
                                                    (if store { 2 } else { 1 }, 1, 0)
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
        for prefix in [0x64, 0x65, 0x67] {
            assert!(
                direct_cpu::translate(&[prefix, 0xc5, 0xf8, 0x10, 0x00, 0xc3], 0x100, 2).is_err()
            );
        }
        println!("direct vector moves: {checks} native state checks passed");
    }
}
