use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    let tables = [
        0xffff0000u32,
        0x0000ffff,
        0xaaaaaaaa,
        0x55555555,
        0xf0f0f0f0,
        0x0f0f0f0f,
        0xfafafafa,
        0x05050505,
        0xff00ff00,
        0x00ff00ff,
        0xcccccccc,
        0x33333333,
        0x00ffff00,
        0xff0000ff,
        0xf0fffff0,
        0x0f00000f,
    ];
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for condition in 0..16u8 {
            for size in [2, 4, 8usize] {
                let mask = u64::MAX >> (64 - size * 8);
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            bytes.extend_from_slice(&[
                                0x40 | if size == 8 { 8 } else { 0 }
                                    | ((dst >> 3) << 2)
                                    | (src >> 3),
                                0x0f,
                                0x40 | condition,
                                (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7),
                            ]);
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
                            for bits in 0..32u32 {
                                for address in [0x1000u64, 0x1001, 0x100e, 0x100f, u64::MAX - 7] {
                                    if !mem && address != 0x1000 {
                                        continue;
                                    }
                                    let flags = 0x202
                                        | (bits & 1) as u64
                                        | (u64::from((bits >> 1) & 1) << 2)
                                        | (u64::from((bits >> 2) & 1) << 6)
                                        | (u64::from((bits >> 3) & 1) << 7)
                                        | (u64::from((bits >> 4) & 1) << 11);
                                    let mut data = [0xa5u8; 16];
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, false, false).unwrap();
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        ..Cpu::default()
                                    };
                                    for (n, r) in cpu.registers.iter_mut().enumerate() {
                                        *r = 0x123456789abcdef0u64.wrapping_mul(n as u64 + 1);
                                    }
                                    if mem {
                                        cpu.registers[src as usize] = address.wrapping_add(8);
                                    }
                                    let mut expected = cpu.registers;
                                    let valid = !mem
                                        || (address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|end| end <= 0x1010));
                                    if valid {
                                        let source = if mem {
                                            0xa5a5a5a5a5a5a5a5
                                        } else {
                                            expected[src as usize]
                                        };
                                        let result =
                                            if (tables[condition as usize] >> bits) & 1 != 0 {
                                                source
                                            } else {
                                                expected[dst as usize]
                                            };
                                        expected[dst as usize] = (result & mask)
                                            | if size == 2 {
                                                expected[dst as usize] & !mask
                                            } else {
                                                0
                                            };
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, expected,
                                        "{condition}/{size}/{dst}/{src}/{mem}/{bits}/{address:x}"
                                    );
                                    assert_eq!(cpu.flags, flags);
                                    assert_eq!(
                                        (cpu.fault, cpu.fuel, cpu.stop),
                                        if valid { (0, 0, 3) } else { (1, 1, 0) }
                                    );
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), [0xa5; 16]);
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        for memory_operand in [false, true] {
            let bytes = [0x0f, 0x44, if memory_operand { 0x00 } else { 0xc1 }, 0xc3];
            let code = arena
                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for (fuel, fault) in [(0, 0), (2, 7)] {
                let mut cpu = Cpu {
                    rip: 0x100,
                    fuel,
                    fault,
                    registers: [u64::MAX; 16],
                    ..Cpu::default()
                };
                run(&mut cpu);
                assert_eq!(cpu.registers, [u64::MAX; 16]);
                assert_eq!(
                    (cpu.fuel, cpu.fault, cpu.stop),
                    (fuel, fault, if fault == 0 { 4 } else { 0 })
                );
                checks += 1;
            }
        }
        for prefix in [0x64, 0x65, 0x67] {
            assert!(direct_cpu::translate(&[prefix, 0x0f, 0x44, 0x00, 0xc3], 0x100, 2).is_err());
        }
        println!("direct CMOV: {checks} native state checks passed");
    }
}
