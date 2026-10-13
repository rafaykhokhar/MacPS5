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
        let arena = JitArena::new(2 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for condition in 0..16u8 {
            for rex in [None, Some(0x40), Some(0x41)] {
                for r in 0..8u8 {
                    let mut bytes = Vec::new();
                    if let Some(rex) = rex {
                        bytes.push(rex);
                    }
                    bytes.extend_from_slice(&[0x0f, 0x90 | condition, 0xc0 | r, 0xc3]);
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for bits in 0..32u32 {
                        for fuel in [0, 1, 2] {
                            for fault in [0, 9] {
                                let flags = 2
                                    | u64::from(bits & 1)
                                    | (u64::from((bits >> 1) & 1) << 2)
                                    | (u64::from((bits >> 2) & 1) << 6)
                                    | (u64::from((bits >> 3) & 1) << 7)
                                    | (u64::from((bits >> 4) & 1) << 11);
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    flags,
                                    fuel,
                                    fault,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0x123456789abcdef0);
                                let mut expected = cpu.registers;
                                if fuel > 0 && fault == 0 {
                                    let value = u64::from((tables[condition as usize] >> bits) & 1);
                                    let (reg, shift) = if rex.is_none() && r >= 4 {
                                        ((r - 4) as usize, 8)
                                    } else {
                                        (r as usize + if rex == Some(0x41) { 8 } else { 0 }, 0)
                                    };
                                    expected[reg] =
                                        (expected[reg] & !(255u64 << shift)) | (value << shift);
                                }
                                run(&mut cpu);
                                assert_eq!(cpu.registers, expected);
                                assert_eq!((cpu.flags, cpu.fault), (flags, fault));
                                assert_eq!(cpu.fuel, if fault != 0 { fuel } else { 0 });
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        for condition in 0..16u8 {
            for base in 0..16u8 {
                let mut bytes = vec![
                    0x40 | (base >> 3),
                    0x0f,
                    0x90 | condition,
                    0x40 | (base & 7),
                ];
                if base & 7 == 4 {
                    bytes.push(0x24);
                }
                bytes.extend_from_slice(&[0xf8, 0xc3]);
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for bits in 0..32u32 {
                    for address in [0x1000u64, 0x100f, 0x1010, u64::MAX] {
                        for writable in [false, true] {
                            let mut data = [0xa5; 16];
                            let mut memory = Memory::default();
                            memory.add(0x1000, &mut data, writable, false).unwrap();
                            let flags = 2
                                | u64::from(bits & 1)
                                | (u64::from((bits >> 1) & 1) << 2)
                                | (u64::from((bits >> 2) & 1) << 6)
                                | (u64::from((bits >> 3) & 1) << 7)
                                | (u64::from((bits >> 4) & 1) << 11);
                            let mut cpu = Cpu {
                                rip: 0x100,
                                flags,
                                fuel: 2,
                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                ..Cpu::default()
                            };
                            cpu.registers[base as usize] = address.wrapping_add(8);
                            let registers = cpu.registers;
                            let valid = writable && (0x1000..0x1010).contains(&address);
                            let mut expected = [0xa5; 16];
                            if valid {
                                expected[(address - 0x1000) as usize] =
                                    ((tables[condition as usize] >> bits) & 1) as u8;
                            }
                            run(&mut cpu);
                            assert_eq!(cpu.registers, registers);
                            assert_eq!(cpu.flags, flags);
                            assert_eq!(memory.read(0x1000, 16).unwrap(), expected);
                            assert_eq!(cpu.fault, if valid { 0 } else { 2 });
                            checks += 1;
                        }
                    }
                }
            }
        }
        println!(
            "PASS: {checks} SETcc truth-table cases across 16 conditions, byte register encodings, all memory bases, fault-preserved state and guarded one-byte stores"
        );
    }
}
