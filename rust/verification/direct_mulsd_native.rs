use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
// Hand-specified IEEE binary64 results and guest status effects. No helper oracle.
const CASES: &[(u64, u64, u32, u64, u32, u64)] = &[
    (
        0x4008000000000000,
        0x4008000000000000,
        0x1f80,
        0x4022000000000000,
        0,
        0,
    ),
    (
        0x4000000000000000,
        0x4008000000000000,
        0x1f80,
        0x4018000000000000,
        0,
        0,
    ),
    (
        0x8000000000000000,
        0x4008000000000000,
        0x1f80,
        0x8000000000000000,
        0,
        0,
    ),
    (
        0x7ff0000000000000,
        0x4000000000000000,
        0x1f80,
        0x7ff0000000000000,
        0,
        0,
    ),
    (0, 0x7ff0000000000000, 0x1f80, 0xfff8000000000000, 1, 0),
    (0, 0x7ff0000000000000, 0x1f00, 0, 1, 15),
    (
        0x7ff8123456789abc,
        0xfff056789abcdef0,
        0x1f80,
        0x7ff8123456789abc,
        1,
        0,
    ),
    (
        0xfff056789abcdef0,
        0x7ff8123456789abc,
        0x1f80,
        0xfff856789abcdef0,
        1,
        0,
    ),
    (1, 0x3ff0000000000000, 0x1f80, 1, 2, 0),
    (1, 0x3ff0000000000000, 0x1e80, 0, 2, 15),
    (1, 0x3ff0000000000000, 0x1fc0, 0, 0, 0),
    (
        0x0010000000000000,
        0x3fe0000000000000,
        0x1f80,
        0x0008000000000000,
        0,
        0,
    ),
    (0x0010000000000000, 0x3fe0000000000000, 0x9f80, 0, 48, 0),
    (
        0x7fefffffffffffff,
        0x4000000000000000,
        0x1f80,
        0x7ff0000000000000,
        40,
        0,
    ),
    (0x7fefffffffffffff, 0x4000000000000000, 0x1b80, 0, 8, 15),
    (
        0x3ff0000000000001,
        0x3ff0000000000001,
        0x1f80,
        0x3ff0000000000002,
        32,
        0,
    ),
    (
        0x3ff0000000000001,
        0x3ff0000000000001,
        0x3f80,
        0x3ff0000000000002,
        32,
        0,
    ),
    (
        0x3ff0000000000001,
        0x3ff0000000000001,
        0x5f80,
        0x3ff0000000000003,
        32,
        0,
    ),
    (
        0x3ff0000000000001,
        0x3ff0000000000001,
        0x7f80,
        0x3ff0000000000002,
        32,
        0,
    ),
    (0x3ff0000000000001, 0x3ff0000000000001, 0x0f80, 0, 32, 15),
    (
        0x3ff0000000000000,
        0x3ff0000000000000,
        0x0fa0,
        0x3ff0000000000000,
        0,
        0,
    ),
    (0x3ff0000000000000, 0x3ff0000000000000, 0x10000, 0, 0, 14),
];
fn main() {
    let mut checks = 0;
    let mut guards = 0;
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        for legacy in [true, false] {
            for dst in 0..16u8 {
                for first in 0..16u8 {
                    if legacy && first != dst {
                        continue;
                    }
                    for second in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = if legacy {
                                vec![0xf2, 0x40 | ((dst >> 3) << 2) | (second >> 3), 0x0f, 0x59]
                            } else {
                                vec![
                                    0xc4,
                                    0x41 | if dst < 8 { 0x80 } else { 0 }
                                        | if second < 8 { 0x20 } else { 0 },
                                    ((!first & 15) << 3) | 3,
                                    0x59,
                                ]
                            };
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (second & 7),
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
                            for (fault, fuel) in [(7, 2), (0, 0)] {
                                let mut cpu = Cpu {
                                    registers: [0xabcdef; 16],
                                    vectors: [[0xa5; 32]; 16],
                                    rip: 0x100,
                                    flags: 0xadff,
                                    mxcsr: 0x9fc1,
                                    fault,
                                    fuel,
                                    ..Cpu::default()
                                };
                                run(&mut cpu);
                                assert_eq!(cpu.registers, [0xabcdef; 16]);
                                assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                                assert_eq!((cpu.flags, cpu.mxcsr), (0xadff, 0x9fc1));
                                assert_eq!((cpu.rip, cpu.fault, cpu.fuel), (0x100, fault, fuel));
                                assert_eq!(cpu.stop, if fault == 0 { 4 } else { 0 });
                                guards += 1;
                            }
                            for &(a, b, control, result, status, fault) in CASES {
                                if !mem && first == second && a != b {
                                    continue;
                                }
                                for (address, valid) in
                                    [(0x1001u64, true), (0x1009, false), (u64::MAX, false)]
                                {
                                    if !mem && !valid {
                                        continue;
                                    }
                                    let mut data = [0xa5; 16];
                                    data[1..9].copy_from_slice(&b.to_le_bytes());
                                    let original_data = data;
                                    let mut memory = Memory::default();
                                    memory.add(0x1000, &mut data, false, false).unwrap();
                                    let mut cpu = Cpu {
                                        registers: [0xabcdef; 16],
                                        rip: 0x100,
                                        fuel: 2,
                                        flags: 0xadff,
                                        mxcsr: control,
                                        x87_control: 0x0b7f,
                                        x87_status: 0x20,
                                        memory: (&mut memory as *mut Memory<'_>) as u64,
                                        ..Cpu::default()
                                    };
                                    for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                        for (n, b) in v.iter_mut().enumerate() {
                                            *b = (r * 11 + n * 7) as u8;
                                        }
                                    }
                                    cpu.registers[second as usize] = address.wrapping_add(8);
                                    cpu.vectors[first as usize][..8]
                                        .copy_from_slice(&a.to_le_bytes());
                                    if !mem {
                                        cpu.vectors[second as usize][..8]
                                            .copy_from_slice(&b.to_le_bytes());
                                    }
                                    let before = cpu.clone();
                                    let mut expected = before.vectors;
                                    if valid && fault == 0 {
                                        expected[dst as usize][..8]
                                            .copy_from_slice(&result.to_le_bytes());
                                        if !legacy {
                                            expected[dst as usize][8..16].copy_from_slice(
                                                &before.vectors[first as usize][8..16],
                                            );
                                            expected[dst as usize][16..].fill(0);
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.vectors, expected,
                                        "legacy={legacy} dst={dst} first={first} second={second} mem={mem} a={a:x} b={b:x} control={control:x}"
                                    );
                                    assert_eq!(cpu.registers, before.registers);
                                    assert_eq!(
                                        (cpu.flags, cpu.x87_control, cpu.x87_status),
                                        (0xadff, 0x0b7f, 0x20)
                                    );
                                    assert_eq!(
                                        cpu.mxcsr,
                                        if valid { control | status } else { control }
                                    );
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), original_data);
                                    let expected_fault = if valid { fault } else { 1 };
                                    assert_eq!(
                                        (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                        if expected_fault == 0 {
                                            (0, 3, 0, end)
                                        } else {
                                            (expected_fault, 0, 1, 0x100)
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
    println!(
        "direct MULSD/VMULSD: {checks} native state checks and {guards} fault/fuel guards passed"
    );
}
