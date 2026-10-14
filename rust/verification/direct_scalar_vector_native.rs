use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for vex in [false, true] {
            for (size, vector_rm, store, pp, opcode, w) in [
                (4usize, false, false, 1, 0x6e, 0),
                (4, false, true, 1, 0x7e, 0),
                (8, false, false, 1, 0x6e, 1),
                (8, false, true, 1, 0x7e, 1),
                (8, true, false, 2, 0x7e, 0),
                (8, true, true, 1, 0xd6, 0),
            ] {
                for reg in 0..16u8 {
                    for rm in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = if vex {
                                vec![
                                    0xc4,
                                    0x41 | if reg < 8 { 0x80 } else { 0 }
                                        | if rm < 8 { 0x20 } else { 0 },
                                    0x78 | pp | (w << 7),
                                    opcode,
                                ]
                            } else {
                                vec![
                                    if pp == 1 { 0x66 } else { 0xf3 },
                                    0x40 | (w << 3) | ((reg >> 3) << 2) | (rm >> 3),
                                    0x0f,
                                    opcode,
                                ]
                            };
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 }) | ((reg & 7) << 3) | (rm & 7),
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
                            for address in [0x1000u64, 0x1001, 0x100c, 0x100f, u64::MAX] {
                                for writable in [false, true] {
                                    if !mem && (address != 0x1000 || writable) {
                                        continue;
                                    }
                                    let mut data = [0xa5u8; 16];
                                    let mut expected_data = data;
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, writable, false).unwrap();
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xadff,
                                        mxcsr: 0x9fc1,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        registers: [0x0123456789abcdef; 16],
                                        ..Cpu::default()
                                    };
                                    for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                        for (n, b) in v.iter_mut().enumerate() {
                                            *b = (r * 11 + n * 7) as u8;
                                        }
                                    }
                                    if mem {
                                        cpu.registers[rm as usize] = address.wrapping_add(8);
                                    }
                                    let mut regs = cpu.registers;
                                    let mut vectors = cpu.vectors;
                                    let valid = !mem
                                        || (address >= 0x1000
                                            && address
                                                .checked_add(size as u64)
                                                .is_some_and(|end| end <= 0x1010)
                                            && (!store || writable));
                                    if valid {
                                        let mut value = [0u8; 8];
                                        if store {
                                            value[..size]
                                                .copy_from_slice(&vectors[reg as usize][..size]);
                                        } else if mem {
                                            let at = (address - 0x1000) as usize;
                                            value[..size]
                                                .copy_from_slice(&expected_data[at..at + size]);
                                        } else if vector_rm {
                                            value[..size]
                                                .copy_from_slice(&vectors[rm as usize][..size]);
                                        } else {
                                            value[..size].copy_from_slice(
                                                &regs[rm as usize].to_le_bytes()[..size],
                                            );
                                        }
                                        if store && mem {
                                            let at = (address - 0x1000) as usize;
                                            expected_data[at..at + size]
                                                .copy_from_slice(&value[..size]);
                                        } else if store && !vector_rm {
                                            regs[rm as usize] = u64::from_le_bytes(value);
                                        } else {
                                            let dst = if store { rm } else { reg } as usize;
                                            vectors[dst][..8].copy_from_slice(&value);
                                            vectors[dst][8..16].fill(0);
                                            if vex {
                                                vectors[dst][16..].fill(0);
                                            }
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(cpu.registers, regs);
                                    assert_eq!(
                                        cpu.vectors, vectors,
                                        "vex={vex} size={size} vec={vector_rm} store={store} reg={reg} rm={rm} mem={mem} addr={address:x}"
                                    );
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                    assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
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
        println!("direct MOVD/MOVQ: {checks} native state checks passed");
    }
}
