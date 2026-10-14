use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let mut checks = 0;
        for form in 0..3 {
            let legacy = form == 0;
            let size = if form == 2 { 32usize } else { 16 };
            let arena = JitArena::new(64 * 1024 * 1024).unwrap();
            for control in 0..=255u8 {
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        for mem in [false, true] {
                            let mut b = if legacy {
                                vec![0x66, 0x40 | ((dst >> 3) << 2) | (src >> 3), 0x0f, 0x70]
                            } else {
                                vec![
                                    0xc4,
                                    0x41 | if dst < 8 { 0x80 } else { 0 }
                                        | if src < 8 { 0x20 } else { 0 },
                                    0x79 | if form == 2 { 4 } else { 0 },
                                    0x70,
                                ]
                            };
                            b.push((if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7));
                            if mem {
                                if src & 7 == 4 {
                                    b.push(0x24);
                                }
                                b.push(0xf8);
                            }
                            b.push(control);
                            let end = 0x100 + b.len() as u64;
                            b.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&b, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for address in [0x1000u64, 0x1001, 0x1020, 0x1030, 0x1031, u64::MAX] {
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
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xadff,
                                    mxcsr: 0x9fc1,
                                    memory: (&mut memory as *mut Memory<'_>) as u64,
                                    registers: [0xabcdef; 16],
                                    ..Cpu::default()
                                };
                                cpu.registers[src as usize] = address.wrapping_add(8);
                                for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                    for (n, b) in v.iter_mut().enumerate() {
                                        *b = (r * 13 + n * 7) as u8;
                                    }
                                }
                                let regs = cpu.registers;
                                let mut expected = cpu.vectors;
                                let valid = !mem
                                    || ((!legacy || address % 16 == 0)
                                        && address >= 0x1000
                                        && address
                                            .checked_add(size as u64)
                                            .is_some_and(|end| end <= 0x1040));
                                if valid {
                                    let mut result = if legacy {
                                        expected[dst as usize]
                                    } else {
                                        [0; 32]
                                    };
                                    let source = if mem {
                                        &original[(address - 0x1000) as usize..][..size]
                                    } else {
                                        &expected[src as usize][..size]
                                    };
                                    for half in 0..size / 16 {
                                        let mut selector = control;
                                        for lane in 0..4 {
                                            let chosen = usize::from(selector % 4);
                                            selector /= 4;
                                            for byte in 0..4 {
                                                result[half * 16 + lane * 4 + byte] =
                                                    source[half * 16 + chosen * 4 + byte];
                                            }
                                        }
                                    }
                                    expected[dst as usize] = result;
                                }
                                run(&mut cpu);
                                assert_eq!(
                                    cpu.vectors, expected,
                                    "form={form} control={control} dst={dst} src={src} mem={mem} address={address:x}"
                                );
                                assert_eq!(cpu.registers, regs);
                                assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                                assert_eq!(memory.read(0x1000, 64).unwrap(), original);
                                assert_eq!(
                                    (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                    if valid {
                                        (0, 3, 0, end)
                                    } else {
                                        (1, 0, 1, 0x100)
                                    }
                                );
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        println!("direct PSHUFD: {checks} native state checks passed");
    }
}
