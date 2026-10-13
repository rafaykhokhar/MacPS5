//! Native direct VEX packed moves: register direction, widths, alignment and faults.
use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
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
                                let mut bytes = vec![
                                    0xc4,
                                    0x41 | if reg < 8 { 0x80 } else { 0 }
                                        | if rm < 8 { 0x20 } else { 0 },
                                    0x78 | pp | (wide * 4),
                                    operation,
                                    (if mem { 0x40 } else { 0xc0 }) | ((reg & 7) << 3) | (rm & 7),
                                ];
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
                                                let mut result = [0; 32];
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
                                            "{pp}/{opcode}/{wide}/{store}/{reg}/{rm}/{mem}/{address:x}"
                                        );
                                        assert_eq!(memory.read(0x1000, 64).unwrap(), expected_data);
                                        assert_eq!(cpu.registers, registers);
                                        assert_eq!((cpu.flags, cpu.mxcsr), (0xa55a, 0x9fc1));
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
        for prefix in [0x64, 0x65, 0x67] {
            assert!(
                direct_cpu::translate(&[prefix, 0xc5, 0xf8, 0x10, 0x00, 0xc3], 0x100, 2).is_err()
            );
        }
        println!("direct vector moves: {checks} native state checks passed");
    }
}
