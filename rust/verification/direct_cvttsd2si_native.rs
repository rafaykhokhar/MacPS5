use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
// input bits, i32 result/status, i64 result/status. Explicit architectural bits.
const CASES: &[(u64, u32, u32, u64, u32)] = &[
    (0, 0, 0, 0, 0),
    (0x8000000000000000, 0, 0, 0, 0),
    (0x3ff0000000000000, 1, 0, 1, 0),
    (0xbff0000000000000, u32::MAX, 0, u64::MAX, 0),
    (0x3fffffffffffffff, 1, 32, 1, 32),
    (0xbfffffffffffffff, u32::MAX, 32, u64::MAX, 32),
    (0x3fe0000000000000, 0, 32, 0, 32),
    (0xbfe0000000000000, 0, 32, 0, 32),
    (1, 0, 32, 0, 32),
    (0x8000000000000001, 0, 32, 0, 32),
    (0x41dfffffffc00000, 0x7fffffff, 0, 0x7fffffff, 0),
    (0xc1e0000000000000, 0x80000000, 0, 0xffffffff80000000, 0),
    (0x41e0000000000000, 0x80000000, 1, 0x80000000, 0),
    (0x43dfffffffffffff, 0x80000000, 1, 0x7ffffffffffffc00, 0),
    (0xc3e0000000000000, 0x80000000, 1, 0x8000000000000000, 0),
    (0x43e0000000000000, 0x80000000, 1, 0x8000000000000000, 1),
    (0xc3e0000000000001, 0x80000000, 1, 0x8000000000000000, 1),
    (0x7ff0000000000000, 0x80000000, 1, 0x8000000000000000, 1),
    (0xfff0000000000000, 0x80000000, 1, 0x8000000000000000, 1),
    (0x7ff0000000000001, 0x80000000, 1, 0x8000000000000000, 1),
    (0x7ff8123456789abc, 0x80000000, 1, 0x8000000000000000, 1),
];
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        for vex in [false, true] {
            for wide in [false, true] {
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = if vex {
                                vec![
                                    0xc4,
                                    0x41 | if dst < 8 { 0x80 } else { 0 }
                                        | if src < 8 { 0x20 } else { 0 },
                                    0x7b | ((wide as u8) << 7),
                                    0x2c,
                                ]
                            } else {
                                vec![
                                    0xf2,
                                    0x40 | ((wide as u8) << 3) | ((dst >> 3) << 2) | (src >> 3),
                                    0x0f,
                                    0x2c,
                                ]
                            };
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7),
                            );
                            if mem {
                                if src & 7 == 4 {
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
                            for &(input, res32, status32, res64, status64) in CASES {
                                for rc in 0..4 {
                                    for control in [
                                        0x1f80 | (rc << 13),
                                        0x9f80 | (rc << 13),
                                        0x9fc0 | (rc << 13),
                                        0x0fa1 | (rc << 13),
                                        0x1f00 | (rc << 13),
                                        0x10000,
                                    ] {
                                        for address in [0x1001u64, 0x1008, 0x1009, u64::MAX] {
                                            if !mem && address != 0x1001 {
                                                continue;
                                            }
                                            let valid =
                                                !mem || address == 0x1001 || address == 0x1008;
                                            let mut data = [0xa5u8; 16];
                                            if valid {
                                                let at = if mem {
                                                    (address - 0x1000) as usize
                                                } else {
                                                    1
                                                };
                                                data[at..at + 8]
                                                    .copy_from_slice(&input.to_le_bytes());
                                            }
                                            let original = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, false, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel: 2,
                                                flags: 0xadff,
                                                mxcsr: control,
                                                registers: [0xabcdef1234567890; 16],
                                                vectors: [[0xa5; 32]; 16],
                                                x87_control: 0x0b7f,
                                                x87_status: 0x20,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                ..Cpu::default()
                                            };
                                            if mem {
                                                cpu.registers[src as usize] =
                                                    address.wrapping_add(8);
                                            } else {
                                                cpu.vectors[src as usize][..8]
                                                    .copy_from_slice(&input.to_le_bytes());
                                            }
                                            let before = cpu.clone();
                                            let mut regs = before.registers;
                                            let status = if input & 0x7fffffffffffffff == 1
                                                && control & 0x40 != 0
                                            {
                                                0
                                            } else if wide {
                                                status64
                                            } else {
                                                status32
                                            };
                                            let fault = if !valid {
                                                1
                                            } else if control > 0xffff {
                                                14
                                            } else if status & !(control >> 7) != 0 {
                                                15
                                            } else {
                                                0
                                            };
                                            if fault == 0 {
                                                regs[dst as usize] =
                                                    if wide { res64 } else { u64::from(res32) };
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.registers, regs,
                                                "vex={vex} wide={wide} dst={dst} src={src} mem={mem} input={input:x} control={control:x}"
                                            );
                                            assert_eq!(cpu.vectors, before.vectors);
                                            assert_eq!(
                                                cpu.mxcsr,
                                                if valid && control <= 0xffff {
                                                    control | status
                                                } else {
                                                    control
                                                }
                                            );
                                            assert_eq!(
                                                (cpu.flags, cpu.x87_control, cpu.x87_status),
                                                (0xadff, 0x0b7f, 0x20)
                                            );
                                            assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                            assert_eq!(
                                                (cpu.rip, cpu.fault, cpu.stop, cpu.fuel),
                                                if fault == 0 {
                                                    (end, 0, 3, 0)
                                                } else {
                                                    (0x100, fault, 0, 1)
                                                }
                                            );
                                            checks += 1;
                                        }
                                    }
                                }
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
    println!("direct CVTTSD2SI/VCVTTSD2SI: {checks} native state checks passed");
}
