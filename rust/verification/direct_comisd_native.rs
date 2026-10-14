use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
// a,b,relation flags,DAZ relation,ordered status,quiet status.
const CASES: &[(u64, u64, u64, u64, u32, u32)] = &[
    (0, 0, 0x40, 0x40, 0, 0),
    (0, 1 << 63, 0x40, 0x40, 0, 0),
    (0x3ff0000000000000, 0x4000000000000000, 1, 1, 0, 0),
    (0x4000000000000000, 0x3ff0000000000000, 0, 0, 0, 0),
    (0xbff0000000000000, 0xc000000000000000, 0, 0, 0, 0),
    (0xc000000000000000, 0xbff0000000000000, 1, 1, 0, 0),
    (0x7ff0000000000000, 0x7fefffffffffffff, 0, 0, 0, 0),
    (0xfff0000000000000, 0xffefffffffffffff, 1, 1, 0, 0),
    (0x7ff0000000000000, 0x7ff0000000000000, 0x40, 0x40, 0, 0),
    (1, 0, 0, 0x40, 2, 2),
    (0, 1, 1, 0x40, 2, 2),
    (0x8000000000000001, 0, 1, 0x40, 2, 2),
    (1, 2, 1, 0x40, 2, 2),
    (1, 1, 0x40, 0x40, 2, 2),
    (0x7ff8123456789abc, 1, 0x45, 0x45, 1, 0),
    (1, 0x7ff8123456789abc, 0x45, 0x45, 1, 0),
    (0x7ff0123456789abc, 1, 0x45, 0x45, 1, 1),
    (1, 0x7ff0123456789abc, 0x45, 0x45, 1, 1),
    (0x7ff8123456789abc, 0x7ff8123456789abc, 0x45, 0x45, 1, 0),
    (0x7ff0123456789abc, 0x7ff0123456789abc, 0x45, 0x45, 1, 1),
];
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        for vex in [false, true] {
            for quiet in [false, true] {
                for first in 0..16u8 {
                    for second in 0..16u8 {
                        for mem in [false, true] {
                            let opcode = if quiet { 0x2e } else { 0x2f };
                            let mut bytes = if vex {
                                vec![
                                    0xc4,
                                    0x41 | if first < 8 { 0x80 } else { 0 }
                                        | if second < 8 { 0x20 } else { 0 },
                                    0x79,
                                    opcode,
                                ]
                            } else {
                                vec![
                                    0x66,
                                    0x40 | ((first >> 3) << 2) | (second >> 3),
                                    0x0f,
                                    opcode,
                                ]
                            };
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 }) | ((first & 7) << 3) | (second & 7),
                            );
                            if mem {
                                if second & 7 == 4 {
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
                            for &(a, b, flags, dazflags, ordered, unordered) in CASES {
                                if !mem && first == second && a != b {
                                    continue;
                                }
                                for control in
                                    [0x1f80u32, 0x9fc0, 0x1f00, 0x1e80, 0x1e00, 0x6003, 0x10000]
                                {
                                    for address in [0x1001u64, 0x1008, 0x1009, u64::MAX] {
                                        if !mem && address != 0x1001 {
                                            continue;
                                        }
                                        let valid = !mem || address == 0x1001 || address == 0x1008;
                                        let mut data = [0xa5u8; 16];
                                        if valid {
                                            let at =
                                                if mem { (address - 0x1000) as usize } else { 1 };
                                            data[at..at + 8].copy_from_slice(&b.to_le_bytes());
                                        }
                                        let original = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, false, false).unwrap();
                                        let mut cpu = Cpu {
                                            registers: [0xabcdef; 16],
                                            vectors: [[0xa5; 32]; 16],
                                            rip: 0x100,
                                            fuel: 2,
                                            flags: 0xadff,
                                            mxcsr: control,
                                            x87_control: 0x0b7f,
                                            x87_status: 0x20,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            ..Cpu::default()
                                        };
                                        cpu.registers[second as usize] = address.wrapping_add(8);
                                        cpu.vectors[first as usize][..8]
                                            .copy_from_slice(&a.to_le_bytes());
                                        if !mem {
                                            cpu.vectors[second as usize][..8]
                                                .copy_from_slice(&b.to_le_bytes());
                                        }
                                        let before = cpu.clone();
                                        let mut status = if quiet { unordered } else { ordered };
                                        if control & 0x40 != 0 && status == 2 {
                                            status = 0;
                                        }
                                        let fault = if !valid {
                                            1
                                        } else if control > 0xffff {
                                            14
                                        } else if status & !(control >> 7) != 0 {
                                            15
                                        } else {
                                            0
                                        };
                                        let resultflags =
                                            if control & 0x40 != 0 { dazflags } else { flags };
                                        run(&mut cpu);
                                        assert_eq!(
                                            cpu.flags,
                                            if fault == 0 {
                                                (before.flags & !0x8d5) | resultflags
                                            } else {
                                                before.flags
                                            },
                                            "vex={vex} quiet={quiet} first={first} second={second} mem={mem} a={a:x} b={b:x} control={control:x}"
                                        );
                                        assert_eq!(
                                            cpu.mxcsr,
                                            if valid && control <= 0xffff {
                                                control | status
                                            } else {
                                                control
                                            }
                                        );
                                        assert_eq!(cpu.vectors, before.vectors);
                                        assert_eq!(cpu.registers, before.registers);
                                        assert_eq!(
                                            (cpu.x87_control, cpu.x87_status),
                                            (0x0b7f, 0x20)
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
                            for (fault, fuel) in [(7, 2), (0, 0)] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fault,
                                    fuel,
                                    mxcsr: 0x9fc1,
                                    flags: 0xadff,
                                    registers: [0xabcdef; 16],
                                    vectors: [[0xa5; 32]; 16],
                                    ..Cpu::default()
                                };
                                run(&mut cpu);
                                assert_eq!(
                                    (cpu.rip, cpu.fault, cpu.fuel, cpu.mxcsr, cpu.flags),
                                    (0x100, fault, fuel, 0x9fc1, 0xadff)
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
    println!("direct COMISD/UCOMISD: {checks} native state checks passed");
}
