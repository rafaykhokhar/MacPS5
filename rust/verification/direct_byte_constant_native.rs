use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(2 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for rex in [None, Some(0x40), Some(0x41)] {
            for number in 0..8u8 {
                for c6 in [false, true] {
                    for value in [0u8, 1, 0x7f, 0x80, 0xff] {
                        let mut bytes = Vec::new();
                        if let Some(rex) = rex {
                            bytes.push(rex);
                        }
                        if c6 {
                            bytes.extend_from_slice(&[0xc6, 0xc0 | number]);
                        } else {
                            bytes.push(0xb0 | number);
                        }
                        bytes.extend_from_slice(&[value, 0xc3]);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for fuel in [0, 1, 2] {
                            for fault in [0, 9] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel,
                                    fault,
                                    flags: 0xa55a,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0x123456789abcdef0);
                                let mut expected = cpu.registers;
                                if fuel > 0 && fault == 0 {
                                    let (register, shift) = if rex.is_none() && number >= 4 {
                                        ((number - 4) as usize, 8)
                                    } else {
                                        (number as usize + if rex == Some(0x41) { 8 } else { 0 }, 0)
                                    };
                                    expected[register] = (expected[register] & !(0xffu64 << shift))
                                        | ((value as u64) << shift);
                                }
                                run(&mut cpu);
                                assert_eq!(cpu.registers, expected);
                                assert_eq!((cpu.flags, cpu.fault), (0xa55a, fault));
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        for base in 0..16u8 {
            for value in [0u8, 0x80, 0xff] {
                let mut bytes = vec![0x40 | (base >> 3), 0xc6, 0x40 | (base & 7)];
                if base & 7 == 4 {
                    bytes.push(0x24);
                }
                bytes.extend_from_slice(&[0xf8, value, 0xc3]);
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for address in [0xfffu64, 0x1000, 0x1001, 0x100f, 0x1010, u64::MAX] {
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
                        cpu.registers[base as usize] = address.wrapping_add(8);
                        let registers = cpu.registers;
                        let valid = writable && (0x1000..0x1010).contains(&address);
                        let mut expected = [0xa5; 16];
                        if valid {
                            expected[(address - 0x1000) as usize] = value;
                        }
                        run(&mut cpu);
                        assert_eq!(cpu.registers, registers);
                        assert_eq!(cpu.flags, 0xa55a);
                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected);
                        assert_eq!(cpu.fault, if valid { 0 } else { 2 });
                        checks += 1;
                    }
                }
            }
        }
        println!(
            "PASS: {checks} byte-immediate MOV cases including legacy high bytes, REX low bytes, preserved surrounding bits, guarded single-byte stores and faults"
        );
    }
}
