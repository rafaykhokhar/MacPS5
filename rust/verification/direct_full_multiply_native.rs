use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(a: u64, b: u64, bits: u32, signed: bool) -> (u128, bool) {
    let mask = u64::MAX >> (64 - bits);
    let sign = 1u64 << (bits - 1);
    let a = a & mask;
    let b = b & mask;
    let negative = signed && (a ^ b) & sign != 0;
    let magnitude = |v: u64| {
        if signed && v & sign != 0 {
            v.wrapping_neg() & mask
        } else {
            v
        }
    };
    let a = magnitude(a);
    let b = magnitude(b);
    let mut product = 0u128;
    for bit in 0..bits {
        if b & (1u64 << bit) != 0 {
            product += u128::from(a) << bit;
        }
    }
    let overflow = if signed {
        if negative {
            product > u128::from(sign)
        } else {
            product >= u128::from(sign)
        }
    } else {
        product >> bits != 0
    };
    (
        if negative {
            product.wrapping_neg()
        } else {
            product
        },
        overflow,
    )
}
fn main() {
    unsafe {
        let arena = JitArena::new(8 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [1usize, 2, 4, 8] {
            let bits = (size * 8) as u32;
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            for signed in [false, true] {
                for rex in [false, true] {
                    for src in 0..if rex { 16u8 } else { 8 } {
                        for mem in [false, true] {
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            if rex || size == 8 {
                                bytes.push(0x40 | if size == 8 { 8 } else { 0 } | (src >> 3));
                            }
                            bytes.push(if size == 1 { 0xf6 } else { 0xf7 });
                            bytes.push(
                                (if mem { 0x40 } else { 0xc0 })
                                    | if signed { 0x28 } else { 0x20 }
                                    | (src & 7),
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
                            for a in [0u64, 1, 2, sign - 1, sign, mask] {
                                for b in [0u64, 1, 2, sign - 1, sign, mask] {
                                    for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX] {
                                        if !mem && address != 0x1000 {
                                            continue;
                                        }
                                        for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 9)] {
                                            let mut data = [0u8; 16];
                                            data[..8].copy_from_slice(&b.to_le_bytes());
                                            data[8..].copy_from_slice(&b.to_le_bytes());
                                            let original = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, false, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel,
                                                fault: prior,
                                                flags: 0xadff,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                registers: [0x123456789abcdef0; 16],
                                                ..Cpu::default()
                                            };
                                            cpu.registers[0] = (cpu.registers[0] & !mask) | a;
                                            let (slot, shift) = if size == 1 && !rex && src >= 4 {
                                                ((src - 4) as usize, 8)
                                            } else {
                                                (src as usize, 0)
                                            };
                                            if mem {
                                                cpu.registers[src as usize] =
                                                    address.wrapping_add(8);
                                            } else {
                                                cpu.registers[slot] = (cpu.registers[slot]
                                                    & !(mask << shift))
                                                    | (b << shift);
                                            }
                                            let mut expected = cpu.registers;
                                            let mut flags = cpu.flags;
                                            let readable = !mem
                                                || (address >= 0x1000
                                                    && address
                                                        .checked_add(size as u64)
                                                        .is_some_and(|end| end <= 0x1010));
                                            let executed = fuel > 0 && prior == 0;
                                            if readable && executed {
                                                let input = if mem {
                                                    let mut bytes = [0u8; 8];
                                                    let at = (address - 0x1000) as usize;
                                                    bytes[..size]
                                                        .copy_from_slice(&original[at..at + size]);
                                                    u64::from_le_bytes(bytes)
                                                } else {
                                                    expected[slot] >> shift
                                                };
                                                let (p, overflow) =
                                                    reference(expected[0], input, bits, signed);
                                                let lo = p as u64 & mask;
                                                let hi = (p >> bits) as u64 & mask;
                                                if size == 1 {
                                                    expected[0] =
                                                        (expected[0] & !65535) | (p as u64 & 65535);
                                                } else if size == 2 {
                                                    expected[0] = (expected[0] & !mask) | lo;
                                                    expected[2] = (expected[2] & !mask) | hi;
                                                } else {
                                                    expected[0] = lo;
                                                    expected[2] = hi;
                                                }
                                                flags = (flags & !0x801)
                                                    | if overflow { 0x801 } else { 0 };
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.registers, expected,
                                                "register {size}/{signed}/{rex}/{src}/{mem}/{a}/{b}/{address:x}"
                                            );
                                            assert_eq!(
                                                cpu.flags, flags,
                                                "flags {size}/{signed}/{rex}/{src}/{mem}/{a}/{b}"
                                            );
                                            assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                            let fault =
                                                if executed && !readable { 1 } else { prior };
                                            assert_eq!(
                                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                                (
                                                    fault,
                                                    if fault != 0 {
                                                        0
                                                    } else if fuel < 2 {
                                                        4
                                                    } else {
                                                        3
                                                    },
                                                    if prior != 0 {
                                                        fuel
                                                    } else if executed && !readable {
                                                        fuel - 1
                                                    } else {
                                                        0
                                                    },
                                                    if executed && readable { end } else { 0x100 }
                                                )
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
        for signed in [false, true] {
            let code = arena
                .publish(
                    &direct_cpu::translate(
                        &[0xf6, if signed { 0xe9 } else { 0xe1 }, 0xc3],
                        0x100,
                        2,
                    )
                    .unwrap(),
                )
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for a in 0..256u64 {
                for b in 0..256u64 {
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel: 2,
                        flags: 0xadff,
                        ..Cpu::default()
                    };
                    cpu.registers[0] = a;
                    cpu.registers[1] = b;
                    let (p, overflow) = reference(a, b, 8, signed);
                    run(&mut cpu);
                    assert_eq!(cpu.registers[0], p as u64 & 65535);
                    assert_eq!(
                        cpu.flags,
                        (0xadff & !0x801) | if overflow { 0x801 } else { 0 }
                    );
                    checks += 1;
                }
            }
        }
        println!("direct full MUL/IMUL: {checks} native state checks passed");
    }
}
