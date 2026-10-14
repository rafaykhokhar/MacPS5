use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
// Per-rounding-mode expected bits, in nearest/down/up/toward-zero order.
const WIDE: &[(u64, [u64; 4], u32)] = &[
    (0, [0; 4], 0),
    (1, [0x3ff0000000000000; 4], 0),
    (u64::MAX, [0xbff0000000000000; 4], 0),
    (1 << 63, [0xc3e0000000000000; 4], 0),
    (
        i64::MAX as u64,
        [
            0x43e0000000000000,
            0x43dfffffffffffff,
            0x43e0000000000000,
            0x43dfffffffffffff,
        ],
        32,
    ),
    (
        (1 << 53) + 1,
        [
            0x4340000000000000,
            0x4340000000000000,
            0x4340000000000001,
            0x4340000000000000,
        ],
        32,
    ),
    (
        (1 << 53) + 3,
        [
            0x4340000000000002,
            0x4340000000000001,
            0x4340000000000002,
            0x4340000000000001,
        ],
        32,
    ),
    (
        (-((1i64 << 53) + 1)) as u64,
        [
            0xc340000000000000,
            0xc340000000000001,
            0xc340000000000000,
            0xc340000000000000,
        ],
        32,
    ),
];
const NARROW: &[(u64, [u64; 4], u32)] = &[
    (0xaabbccdd00000000, [0; 4], 0),
    (0xaabbccdd00000001, [0x3ff0000000000000; 4], 0),
    (0xaabbccddffffffff, [0xbff0000000000000; 4], 0),
    (0xaabbccdd80000000, [0xc1e0000000000000; 4], 0),
    (0xaabbccdd7fffffff, [0x41dfffffffc00000; 4], 0),
];
fn main() {
    let mut checks = 0;
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        for legacy in [true, false] {
            for wide in [false, true] {
                let size = if wide { 8usize } else { 4 };
                for dst in 0..16u8 {
                    for merge in 0..16u8 {
                        if legacy && merge != dst {
                            continue;
                        }
                        for source in 0..16u8 {
                            for mem in [false, true] {
                                let mut bytes = if legacy {
                                    vec![
                                        0xf2,
                                        0x40 | ((wide as u8) << 3)
                                            | ((dst >> 3) << 2)
                                            | (source >> 3),
                                        0x0f,
                                        0x2a,
                                    ]
                                } else {
                                    vec![
                                        0xc4,
                                        0x41 | if dst < 8 { 0x80 } else { 0 }
                                            | if source < 8 { 0x20 } else { 0 },
                                        ((wide as u8) << 7) | ((!merge & 15) << 3) | 3,
                                        0x2a,
                                    ]
                                };
                                bytes.push(
                                    (if mem { 0x40 } else { 0xc0 })
                                        | ((dst & 7) << 3)
                                        | (source & 7),
                                );
                                if mem {
                                    if source & 7 == 4 {
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
                                for &(input, results, status) in if wide { WIDE } else { NARROW } {
                                    for rc in 0..4 {
                                        for control in [
                                            0x1f80 | (rc << 13),
                                            0x9fc0 | (rc << 13),
                                            0x0fa0 | (rc << 13),
                                            0x10000,
                                        ] {
                                            for (address, valid) in [
                                                (0x1001u64, true),
                                                (0x1000 + 17 - size as u64, false),
                                                (u64::MAX, false),
                                            ] {
                                                if !mem && !valid {
                                                    continue;
                                                }
                                                let mut data = [0xa5u8; 16];
                                                data[1..1 + size]
                                                    .copy_from_slice(&input.to_le_bytes()[..size]);
                                                let original_data = data;
                                                let mut memory = Memory::default();
                                                memory
                                                    .add(0x1000, &mut data, false, false)
                                                    .unwrap();
                                                let mut cpu = Cpu {
                                                    rip: 0x100,
                                                    fuel: 2,
                                                    flags: 0xadff,
                                                    mxcsr: control,
                                                    registers: [0xabcdef; 16],
                                                    memory: (&mut memory as *mut Memory<'_>) as u64,
                                                    x87_control: 0x0b7f,
                                                    x87_status: 0x20,
                                                    ..Cpu::default()
                                                };
                                                for (r, v) in cpu.vectors.iter_mut().enumerate() {
                                                    for (n, b) in v.iter_mut().enumerate() {
                                                        *b = (r * 11 + n * 7) as u8;
                                                    }
                                                }
                                                cpu.registers[source as usize] = if mem {
                                                    address.wrapping_add(8)
                                                } else {
                                                    input
                                                };
                                                let before = cpu.clone();
                                                let mut expected = before.vectors;
                                                let fault = if !valid {
                                                    1
                                                } else if control > 0xffff {
                                                    14
                                                } else if status != 0 && control & 0x1000 == 0 {
                                                    15
                                                } else {
                                                    0
                                                };
                                                if fault == 0 {
                                                    expected[dst as usize][..8].copy_from_slice(
                                                        &results[rc as usize].to_le_bytes(),
                                                    );
                                                    if !legacy {
                                                        expected[dst as usize][8..16]
                                                            .copy_from_slice(
                                                                &before.vectors[merge as usize]
                                                                    [8..16],
                                                            );
                                                        expected[dst as usize][16..].fill(0);
                                                    }
                                                }
                                                run(&mut cpu);
                                                assert_eq!(
                                                    cpu.vectors, expected,
                                                    "legacy={legacy} wide={wide} dst={dst} merge={merge} source={source} mem={mem} input={input:x} control={control:x}"
                                                );
                                                assert_eq!(cpu.registers, before.registers);
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
                                                assert_eq!(
                                                    memory.read(0x1000, 16).unwrap(),
                                                    original_data
                                                );
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
    }
    println!("direct CVTSI2SD/VCVTSI2SD: {checks} native state checks passed");
}
