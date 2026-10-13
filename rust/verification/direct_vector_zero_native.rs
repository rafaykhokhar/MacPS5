//! Direct AVX state clearing, instruction budgets, and resumption at RET.
use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};

fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for all in [false, true] {
            for encoding in 0..3 {
                let l = if all { 4 } else { 0 };
                let mut bytes = if encoding == 0 {
                    vec![0xc5, 0xf8 | l, 0x77]
                } else {
                    vec![
                        0xc4,
                        0xe1,
                        0x78 | l | if encoding == 2 { 0x80 } else { 0 },
                        0x77,
                    ]
                };
                let end = 0x100 + bytes.len() as u64;
                bytes.push(0xc3);
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for seed in 0..256u16 {
                    for fuel in 0..3 {
                        for fault in [0, 9] {
                            let mut cpu = Cpu {
                                rip: 0x100,
                                fuel,
                                fault,
                                flags: 0xa55a,
                                mxcsr: 0x9fc1,
                                x87_control: 0x027f,
                                x87_status: 0x4100,
                                bias: 0x10000000,
                                next_rip: 0x88776655,
                                ..Cpu::default()
                            };
                            for (r, value) in cpu.registers.iter_mut().enumerate() {
                                *value = u64::MAX.wrapping_sub(r as u64 * 771 + seed as u64);
                            }
                            for (r, vector) in cpu.vectors.iter_mut().enumerate() {
                                for (b, value) in vector.iter_mut().enumerate() {
                                    *value = (seed as u8)
                                        .wrapping_add(r as u8 * 13)
                                        .wrapping_add(b as u8 * 7);
                                }
                            }
                            let before = cpu.clone();
                            let mut expected = before.vectors;
                            if fault == 0 && fuel > 0 {
                                for vector in &mut expected {
                                    vector[if all { 0 } else { 16 }..].fill(0);
                                }
                            }
                            run(&mut cpu);
                            assert_eq!(cpu.vectors, expected);
                            assert_eq!(cpu.registers, before.registers);
                            assert_eq!(cpu.flags, before.flags);
                            assert_eq!(cpu.mxcsr, before.mxcsr);
                            assert_eq!(cpu.x87_control, before.x87_control);
                            assert_eq!(cpu.x87_status, before.x87_status);
                            assert_eq!(cpu.x87, before.x87);
                            assert_eq!(cpu.bias, before.bias);
                            assert_eq!(cpu.memory, 0);
                            assert_eq!(cpu.next_rip, before.next_rip);
                            assert_eq!(cpu.fault, fault);
                            assert_eq!(cpu.fuel, if fault != 0 { fuel } else { 0 });
                            assert_eq!(cpu.rip, if fault != 0 || fuel == 0 { 0x100 } else { end });
                            assert_eq!(
                                cpu.stop,
                                if fault != 0 {
                                    0
                                } else if fuel < 2 {
                                    4
                                } else {
                                    3
                                }
                            );
                            if fault == 0 && fuel == 1 {
                                // The resumed RET must not repeat the vector instruction.
                                cpu.vectors[0][31] = 0x5a;
                                let resumed = cpu.vectors;
                                cpu.fuel = 1;
                                cpu.stop = 0;
                                run(&mut cpu);
                                assert_eq!(cpu.vectors, resumed);
                                assert_eq!(
                                    (cpu.fault, cpu.fuel, cpu.stop, cpu.rip),
                                    (0, 0, 3, end)
                                );
                            }
                            checks += 1;
                        }
                    }
                }
            }
        }
        println!("direct VZEROUPPER/VZEROALL: {checks} native state checks passed");
    }
}
