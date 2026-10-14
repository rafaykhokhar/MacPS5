use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        for vex in [false, true] {
            for load in [false, true] {
                for base in 0..16u8 {
                    let mut bytes = if vex {
                        vec![0xc4, 0xc1 | if base < 8 { 0x20 } else { 0 }, 0x78, 0xae]
                    } else {
                        vec![0x40 | (base >> 3), 0x0f, 0xae]
                    };
                    bytes.push(0x40 | ((if load { 2 } else { 3 }) << 3) | (base & 7));
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
                    let controls = (0..=0xffffu32).chain((16..32).map(|bit| 0x1f80 | (1 << bit)));
                    for control in controls {
                        for (address, writable) in [
                            (0x1001u64, true),
                            (0x1004, true),
                            (0x1005, true),
                            (u64::MAX, true),
                            (0x1001, false),
                        ] {
                            if control != 0
                                && control != 0xffff
                                && control != 0x1f80
                                && control <= 0xffff
                                && (address, writable) != (0x1001, true)
                            {
                                continue;
                            }
                            let mut data = [0xa5u8; 8];
                            // Overlapping placement allows both tested valid addresses
                            // to read exactly the intended bits in separate executions.
                            let valid = address >= 0x1000
                                && address.checked_add(4).is_some_and(|e| e <= 0x1008);
                            if valid {
                                let at = (address - 0x1000) as usize;
                                data[at..at + 4].copy_from_slice(&control.to_le_bytes());
                            }
                            let mut expected_data = data;
                            let mut memory = Memory::default();
                            memory.add(0x1000, &mut data, writable, false).unwrap();
                            let mut cpu = Cpu {
                                registers: [0xabcdef; 16],
                                vectors: [[0xa5; 32]; 16],
                                rip: 0x100,
                                fuel: 2,
                                flags: 0xadff,
                                mxcsr: if load { 0x9fc1 } else { control & 0xffff },
                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                x87_control: 0x0b7f,
                                x87_status: 0x20,
                                ..Cpu::default()
                            };
                            cpu.registers[base as usize] = address.wrapping_add(8);
                            let before = cpu.clone();
                            let fault = if !valid || (!load && !writable) {
                                if load { 1 } else { 2 }
                            } else if load && control > 0xffff {
                                14
                            } else {
                                0
                            };
                            let expected_control = if load && fault == 0 {
                                control
                            } else {
                                before.mxcsr
                            };
                            if !load && fault == 0 {
                                let at = (address - 0x1000) as usize;
                                expected_data[at..at + 4]
                                    .copy_from_slice(&before.mxcsr.to_le_bytes());
                            }
                            run(&mut cpu);
                            assert_eq!(
                                cpu.mxcsr, expected_control,
                                "vex={vex} load={load} base={base} control={control:x}"
                            );
                            assert_eq!(cpu.registers, before.registers);
                            assert_eq!(cpu.vectors, before.vectors);
                            assert_eq!(
                                (cpu.flags, cpu.x87_control, cpu.x87_status),
                                (before.flags, before.x87_control, before.x87_status)
                            );
                            assert_eq!(memory.read(0x1000, 8).unwrap(), expected_data);
                            assert_eq!(
                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                if fault == 0 {
                                    (0, 3, 0, end)
                                } else {
                                    (fault, 0, 1, 0x100)
                                }
                            );
                            checks += 1;
                        }
                    }
                    for (fault, fuel) in [(7, 2), (0, 0)] {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            fault,
                            fuel,
                            mxcsr: 0x9fc1,
                            ..Cpu::default()
                        };
                        run(&mut cpu);
                        assert_eq!(
                            (cpu.rip, cpu.fault, cpu.fuel, cpu.mxcsr),
                            (0x100, fault, fuel, 0x9fc1)
                        );
                        assert_eq!(cpu.stop, if fault == 0 { 4 } else { 0 });
                        checks += 1;
                    }
                }
            }
        }
        // Actual guest control load -> arithmetic -> status store, with no host
        // mutation of MXCSR between instructions. Unmasked precision must stop
        // before the status store and preserve the multiplication destination.
        let bytes = [
            0x0f, 0xae, 0x17, 0xf2, 0x0f, 0x59, 0x47, 0x08, 0x0f, 0xae, 0x5f, 0x04, 0xc3,
        ];
        let code = arena
            .publish(&direct_cpu::translate(&bytes, 0x100, 4).unwrap())
            .unwrap();
        let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
        for (control, wanted, fault) in [
            (0x1f80u32, 0x3ff0000000000002u64, 0),
            (0x3f80, 0x3ff0000000000002, 0),
            (0x5f80, 0x3ff0000000000003, 0),
            (0x7f80, 0x3ff0000000000002, 0),
            (0x0f80, 0x3ff0000000000001, 15),
        ] {
            let mut data = [0xa5u8; 16];
            data[..4].copy_from_slice(&control.to_le_bytes());
            data[8..].copy_from_slice(&0x3ff0000000000001u64.to_le_bytes());
            let mut memory = Memory::default();
            memory.add(0x2000, &mut data, true, false).unwrap();
            let mut cpu = Cpu {
                rip: 0x100,
                fuel: 4,
                memory: (&mut memory as *mut Memory<'_>) as u64,
                ..Cpu::default()
            };
            cpu.registers[7] = 0x2000;
            cpu.vectors[0][..8].copy_from_slice(&0x3ff0000000000001u64.to_le_bytes());
            run(&mut cpu);
            assert_eq!(cpu.mxcsr, control | 32);
            assert_eq!(cpu.vectors[0][..8], wanted.to_le_bytes());
            assert_eq!(cpu.fault, fault);
            assert_eq!(
                memory.read(0x2004, 4).unwrap(),
                if fault == 0 {
                    (control | 32).to_le_bytes()
                } else {
                    [0xa5; 4]
                }
            );
            assert_eq!(
                (cpu.rip, cpu.stop, cpu.fuel),
                if fault == 0 {
                    (0x10c, 3, 0)
                } else {
                    (0x103, 0, 2)
                }
            );
            checks += 1;
        }
    }
    println!("direct MXCSR controls: {checks} native checks passed");
}
