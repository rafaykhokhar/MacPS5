use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(2 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for store in [false, true] {
                for reg in 0..16u8 {
                    for base in 0..16u8 {
                        // Mod=01 keeps RBP/R13 ordinary bases; SIB handles RSP/R12.
                        let mut bytes = vec![
                            0x40 | if wide { 8 } else { 0 } | ((reg >> 3) << 2) | (base >> 3),
                            if store { 0x89 } else { 0x8b },
                            0x40 | ((reg & 7) << 3) | (base & 7),
                        ];
                        if base & 7 == 4 {
                            bytes.push(0x24);
                        }
                        bytes.extend_from_slice(&[0xf8, 0xc3]); // displacement -8
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for address in [0x1000u64, 0x1001, 0x100c, 0x1010, u64::MAX] {
                            for writable in [false, true] {
                                let mut data = [0xa5; 16];
                                let mut memory = Memory::default();
                                memory.add(0x1000, &mut data, writable, false).unwrap();
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xa55a,
                                    memory: (&mut memory as *mut Memory<'_>) as u64,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0xfedcba9876543210);
                                cpu.registers[base as usize] = address.wrapping_add(8);
                                let old = cpu.clone();
                                let size = if wide { 8 } else { 4 };
                                let valid = address >= 0x1000
                                    && address.checked_add(size).is_some_and(|end| end <= 0x1010)
                                    && (!store || writable);
                                run(&mut cpu);
                                let mut expected = old.registers;
                                let mut expected_data = [0xa5; 16];
                                if valid {
                                    if store {
                                        let at = (address - 0x1000) as usize;
                                        expected_data[at..at + size as usize].copy_from_slice(
                                            &old.registers[reg as usize].to_le_bytes()
                                                [..size as usize],
                                        );
                                    } else {
                                        expected[reg as usize] =
                                            if wide { 0xa5a5a5a5a5a5a5a5 } else { 0xa5a5a5a5 };
                                    }
                                }
                                assert_eq!(cpu.registers, expected);
                                assert_eq!(cpu.flags, old.flags);
                                assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                assert_eq!(
                                    cpu.fault,
                                    if valid {
                                        0
                                    } else if store {
                                        2
                                    } else {
                                        1
                                    }
                                );
                                assert_eq!(cpu.fuel, if valid { 0 } else { 1 });
                                assert_eq!(
                                    cpu.rip,
                                    if valid {
                                        0x100 + bytes.len() as u64 - 1
                                    } else {
                                        0x100
                                    }
                                );
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        for store in [false, true] {
            let opcode = if store { 0x89 } else { 0x8b };
            let mut forms = Vec::new();
            forms.push(vec![0x48, opcode, 0x05, 0xf9, 0x06, 0, 0, 0xc3]); // bias 0x800 + next 0x107 + 0x6f9
            forms.push(vec![0x48, opcode, 0x04, 0x25, 0, 0x10, 0, 0, 0xc3]); // absolute disp32 in 64-bit address mode
            for scale in 0..4 {
                forms.push(vec![0x48, opcode, 0x44, (scale << 6) | 0x0a, 0x10, 0xc3]); // RDX + RCX*scale +16
                let mut bytes = vec![0x48, opcode, 0x04, (scale << 6) | 0x0d]; // no base + RCX*scale +disp32
                bytes.extend_from_slice(&(0x1000u32 - (3u32 << scale)).to_le_bytes());
                bytes.push(0xc3);
                forms.push(bytes);
            }
            for (index, bytes) in forms.iter().enumerate() {
                let mut data = [0x5a; 16];
                let mut memory = Memory::default();
                memory.add(0x1000, &mut data, true, false).unwrap();
                let mut cpu = Cpu {
                    rip: 0x100,
                    bias: 0x800,
                    fuel: 2,
                    memory: (&mut memory as *mut Memory<'_>) as u64,
                    ..Cpu::default()
                };
                cpu.registers[0] = 0x123456789abcdef0;
                cpu.registers[1] = 3;
                if index >= 2 {
                    cpu.registers[2] = 0x1000 - 16 - (3 << ((index - 2) / 2));
                }
                let block = arena
                    .publish(&direct_cpu::translate(bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(block.entry(0).unwrap());
                run(&mut cpu);
                assert_eq!(cpu.fault, 0, "address form {bytes:x?}");
                assert_eq!(cpu.stop, 3);
                if store {
                    assert_eq!(memory.load(0x1000, 8).unwrap(), 0x123456789abcdef0);
                } else {
                    assert_eq!(cpu.registers[0], 0x5a5a5a5a5a5a5a5a);
                }
                checks += 1;
            }
        }
        for bytes in [
            &[0x64, 0x48, 0x8b, 0x00, 0xc3][..],
            &[0x67, 0x8b, 0x00, 0xc3],
            &[0x67, 0x8b, 0x05, 0, 0, 0, 0, 0xc3],
        ] {
            assert!(direct_cpu::translate(bytes, 0x100, 2).is_err());
        }
        use native_ps5::{
            loader::Image,
            module_executor::{Execution, NativeExecutor, memory_fixture},
            modules::Arguments,
        };
        let mut file = memory_fixture();
        let mut bytes = vec![0x48, 0xb8];
        bytes.extend_from_slice(&0x100300u64.to_le_bytes());
        bytes.extend_from_slice(&[0x8b, 0x00, 0xc3]);
        file[0x100..0x100 + bytes.len()].copy_from_slice(&bytes);
        file[0x300..0x304].copy_from_slice(&42u32.to_le_bytes());
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &bytes, 3, &arena)
            .unwrap();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(42)
        ));
        println!(
            "PASS: {checks} direct x86 memory MOV checks covering all register/base aliases, widths, directions, negative offsets, unaligned access and guarded failures"
        );
    }
}
