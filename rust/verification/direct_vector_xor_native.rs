//! Direct VEX XOR: all register triples, both widths, plus guarded memory faults.
use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for pd in 0..2u8 {
            for wide in 0..2u8 {
                let size = if wide == 0 { 16 } else { 32 };
                for dst in 0..16u8 {
                    for first in 0..16u8 {
                        for second in 0..16u8 {
                            for memory_operand in [false, true] {
                                let mut bytes = vec![
                                    0xc4,
                                    0x41 | if dst < 8 { 0x80 } else { 0 }
                                        | if second < 8 { 0x20 } else { 0 },
                                    ((!first & 15) << 3) | pd | wide * 4,
                                    0x57,
                                    (if memory_operand { 0x40 } else { 0xc0 })
                                        | ((dst & 7) << 3)
                                        | (second & 7),
                                ];
                                if memory_operand {
                                    if second & 7 == 4 {
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
                                for address in [0x1000u64, 0x1001, 0x1020, 0x1021, u64::MAX - 7] {
                                    if !memory_operand && address != 0x1000 {
                                        continue;
                                    }
                                    let mut data = [0u8; 64];
                                    for (n, b) in data.iter_mut().enumerate() {
                                        *b = (n as u8).wrapping_mul(37).wrapping_add(9);
                                    }
                                    let original = data;
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, false, false).unwrap();
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xa55a,
                                        mxcsr: 0x9fc1,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        ..Cpu::default()
                                    };
                                    cpu.registers.fill(0xdeadbeef);
                                    cpu.registers[second as usize] = address.wrapping_add(8);
                                    for (r, vector) in cpu.vectors.iter_mut().enumerate() {
                                        for (n, b) in vector.iter_mut().enumerate() {
                                            *b = (r as u8)
                                                .wrapping_mul(71)
                                                .wrapping_add(n as u8 * 7);
                                        }
                                    }
                                    let registers = cpu.registers;
                                    let mut expected = cpu.vectors;
                                    let valid = !memory_operand
                                        || address
                                            .checked_add(size as u64)
                                            .is_some_and(|end| address >= 0x1000 && end <= 0x1040);
                                    if valid {
                                        let mut result = [0; 32];
                                        for n in 0..size {
                                            let a = expected[first as usize][n];
                                            let b = if memory_operand {
                                                original[(address - 0x1000) as usize + n]
                                            } else {
                                                expected[second as usize][n]
                                            };
                                            // Independent Boolean XOR truth table, evaluated per bit.
                                            for bit in 0..8 {
                                                if ((a >> bit) & 1) != ((b >> bit) & 1) {
                                                    result[n] |= 1 << bit;
                                                }
                                            }
                                        }
                                        expected[dst as usize] = result;
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.vectors, expected,
                                        "{pd}/{wide}/{dst}/{first}/{second}/{address:x}"
                                    );
                                    assert_eq!(cpu.registers, registers);
                                    assert_eq!(cpu.flags, 0xa55a);
                                    assert_eq!(cpu.mxcsr, 0x9fc1);
                                    assert_eq!(
                                        (cpu.fault, cpu.fuel, cpu.stop),
                                        if valid { (0, 0, 3) } else { (1, 1, 0) }
                                    );
                                    assert_eq!(memory.read(0x1000, 64).unwrap(), original);
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        for operand in [0xc0, 0x00] {
            let code = arena
                .publish(
                    &direct_cpu::translate(&[0xc5, 0xf8, 0x57, operand, 0xc3], 0x100, 2).unwrap(),
                )
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for (fault, fuel) in [(0, 0), (7, 2)] {
                let mut cpu = Cpu {
                    rip: 0x100,
                    fuel,
                    fault,
                    vectors: [[0xa5; 32]; 16],
                    ..Cpu::default()
                };
                run(&mut cpu);
                assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                assert_eq!(
                    (cpu.fault, cpu.fuel, cpu.stop),
                    (fault, fuel, if fault == 0 { 4 } else { 0 })
                );
                checks += 1;
            }
        }
        for prefix in [0x64, 0x65, 0x67] {
            assert!(
                direct_cpu::translate(&[prefix, 0xc5, 0xf0, 0x57, 0x00, 0xc3], 0x100, 2).is_err()
            );
        }
        println!("direct vector XOR: {checks} native state checks passed");
    }
}
