use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        for vex in [false, true] {
            for size in [4usize, 8] {
                for store in [false, true] {
                    for reg in 0..16u8 {
                        for rm in 0..16u8 {
                            for mem in [false, true] {
                                for merge in 0..16u8 {
                                    if (!vex || mem) && merge != 0 {
                                        continue;
                                    }
                                    let pp = if size == 4 { 2 } else { 3 };
                                    let opcode = if store { 0x11 } else { 0x10 };
                                    let mut bytes = if vex {
                                        vec![
                                            0xc4,
                                            0x41 | if reg < 8 { 0x80 } else { 0 }
                                                | if rm < 8 { 0x20 } else { 0 },
                                            ((!(if mem { 0 } else { merge }) & 15) << 3) | pp,
                                            opcode,
                                        ]
                                    } else {
                                        vec![
                                            if size == 4 { 0xf3 } else { 0xf2 },
                                            0x40 | ((reg >> 3) << 2) | (rm >> 3),
                                            0x0f,
                                            opcode,
                                        ]
                                    };
                                    bytes.push(
                                        (if mem { 0x40 } else { 0xc0 })
                                            | ((reg & 7) << 3)
                                            | (rm & 7),
                                    );
                                    if mem {
                                        if rm & 7 == 4 {
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
                                    for address in
                                        [0x1000u64, 0x1001, 0x1008, 0x100c, 0x100f, u64::MAX]
                                    {
                                        for writable in [false, true] {
                                            if !mem && (address != 0x1000 || writable) {
                                                continue;
                                            }
                                            let mut data = [0xa5; 16];
                                            let mut expected_data = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, writable, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel: 2,
                                                flags: 0xadff,
                                                mxcsr: 0x9fc1,
                                                registers: [0xabcdef; 16],
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                ..Cpu::default()
                                            };
                                            for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                                for (n, b) in v.iter_mut().enumerate() {
                                                    *b = (r * 11 + n * 7) as u8;
                                                }
                                            }
                                            cpu.registers[rm as usize] = address.wrapping_add(8);
                                            let regs = cpu.registers;
                                            let original = cpu.vectors;
                                            let mut expected = original;
                                            let valid = !mem
                                                || (address >= 0x1000
                                                    && address
                                                        .checked_add(size as u64)
                                                        .is_some_and(|e| e <= 0x1010)
                                                    && (!store || writable));
                                            if valid {
                                                if mem && store {
                                                    let at = (address - 0x1000) as usize;
                                                    expected_data[at..at + size].copy_from_slice(
                                                        &original[reg as usize][..size],
                                                    );
                                                } else {
                                                    let dst = if store { rm } else { reg } as usize;
                                                    let src = if store { reg } else { rm } as usize;
                                                    if mem {
                                                        let at = (address - 0x1000) as usize;
                                                        expected[dst][..size].copy_from_slice(
                                                            &expected_data[at..at + size],
                                                        );
                                                        expected[dst][size..16].fill(0);
                                                    } else {
                                                        expected[dst][..size].copy_from_slice(
                                                            &original[src][..size],
                                                        );
                                                        if vex {
                                                            expected[dst][size..16]
                                                                .copy_from_slice(
                                                                    &original[merge as usize]
                                                                        [size..16],
                                                                );
                                                        }
                                                    }
                                                    if vex {
                                                        expected[dst][16..].fill(0);
                                                    }
                                                }
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.vectors, expected,
                                                "vex={vex} size={size} store={store} reg={reg} rm={rm} mem={mem} merge={merge} address={address:x}"
                                            );
                                            assert_eq!(cpu.registers, regs);
                                            assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                                            assert_eq!(
                                                memory.read(0x1000, 16).unwrap(),
                                                expected_data
                                            );
                                            assert_eq!(
                                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                                if valid {
                                                    (0, 3, 0, end)
                                                } else {
                                                    (if store { 2 } else { 1 }, 0, 1, 0x100)
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
    }
    println!("direct MOVSS/MOVSD: {checks} native state checks passed");
}
