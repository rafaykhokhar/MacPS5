use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        for form in 0..3 {
            let legacy = form == 0;
            let size = if form == 2 { 32usize } else { 16 };
            for high in [false, true] {
                for dst in 0..16u8 {
                    for first in 0..16u8 {
                        if legacy && first != dst {
                            continue;
                        }
                        for second in 0..16u8 {
                            for mem in [false, true] {
                                let opcode = if high { 0x15 } else { 0x14 };
                                let mut bytes = if legacy {
                                    vec![
                                        0x66,
                                        0x40 | ((dst >> 3) << 2) | (second >> 3),
                                        0x0f,
                                        opcode,
                                    ]
                                } else {
                                    vec![
                                        0xc4,
                                        0x41 | if dst < 8 { 0x80 } else { 0 }
                                            | if second < 8 { 0x20 } else { 0 },
                                        ((!first & 15) << 3) | 1 | if form == 2 { 4 } else { 0 },
                                        opcode,
                                    ]
                                };
                                bytes.push(
                                    (if mem { 0x40 } else { 0xc0 })
                                        | ((dst & 7) << 3)
                                        | (second & 7),
                                );
                                if mem {
                                    if second & 7 == 4 {
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
                                for address in [0x1000u64, 0x1001, 0x1020, 0x1030, 0x1031, u64::MAX]
                                {
                                    if !mem && address != 0x1000 {
                                        continue;
                                    }
                                    let mut data = [0u8; 64];
                                    for (n, b) in data.iter_mut().enumerate() {
                                        *b = n as u8 ^ 0xa5;
                                    }
                                    let original = data;
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, false, false).unwrap();
                                    let mut cpu = Cpu {
                                        registers: [0xabcdef; 16],
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xadff,
                                        mxcsr: 0x9fc1,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        ..Cpu::default()
                                    };
                                    cpu.registers[second as usize] = address.wrapping_add(8);
                                    for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                        for (n, b) in v.iter_mut().enumerate() {
                                            *b = (r * 11 + n * 7) as u8;
                                        }
                                    }
                                    let before = cpu.clone();
                                    let mut expected = before.vectors;
                                    let valid = !mem
                                        || ((!legacy || address & 15 == 0)
                                            && address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|e| e <= 0x1040));
                                    if valid {
                                        for half in 0..size / 16 {
                                            let at = half * 16 + if high { 8 } else { 0 };
                                            let from = if mem {
                                                &original[(address - 0x1000) as usize..][..size]
                                            } else {
                                                &before.vectors[second as usize][..size]
                                            };
                                            expected[dst as usize][half * 16..half * 16 + 8]
                                                .copy_from_slice(
                                                    &before.vectors[first as usize][at..at + 8],
                                                );
                                            expected[dst as usize][half * 16 + 8..half * 16 + 16]
                                                .copy_from_slice(&from[at..at + 8]);
                                        }
                                        if !legacy && size == 16 {
                                            expected[dst as usize][16..].fill(0);
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.vectors, expected,
                                        "form={form} high={high} dst={dst} first={first} second={second} mem={mem} address={address:x}"
                                    );
                                    assert_eq!(cpu.registers, before.registers);
                                    assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                                    assert_eq!(memory.read(0x1000, 64).unwrap(), original);
                                    assert_eq!(
                                        (cpu.rip, cpu.fault, cpu.stop, cpu.fuel),
                                        if valid {
                                            (end, 0, 3, 0)
                                        } else {
                                            (0x100, 1, 0, 1)
                                        }
                                    );
                                    checks += 1;
                                }
                                for (fault, fuel) in [(7, 2), (0, 0)] {
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fault,
                                        fuel,
                                        mxcsr: 0x9fc1,
                                        registers: [0xabcdef; 16],
                                        vectors: [[0xa5; 32]; 16],
                                        ..Cpu::default()
                                    };
                                    run(&mut cpu);
                                    assert_eq!(
                                        (cpu.rip, cpu.fault, cpu.fuel, cpu.mxcsr),
                                        (0x100, fault, fuel, 0x9fc1)
                                    );
                                    assert_eq!(cpu.registers, [0xabcdef; 16]);
                                    assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                                    assert_eq!(cpu.stop, if fault == 0 { 4 } else { 0 });
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    println!("direct packed-double unpack: {checks} native state checks passed");
}
