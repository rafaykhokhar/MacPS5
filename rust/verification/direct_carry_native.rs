use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn location(n: u8, size: usize, rex: bool) -> (usize, u32) {
    if size == 1 && !rex && n >= 4 {
        ((n - 4) as usize, 8)
    } else {
        (n as usize, 0)
    }
}
fn reference(a: u64, b: u64, size: usize, sub: bool, old: u64) -> (u64, u64) {
    let modulus = 1i128 << (size * 8);
    let mask = (modulus - 1) as u64;
    let sign = 1u64 << (size * 8 - 1);
    let a = a & mask;
    let b = b & mask;
    let carry = old & 1;
    let exact = if sub {
        a as i128 - b as i128 - carry as i128
    } else {
        a as i128 + b as i128 + carry as i128
    };
    let result = exact.rem_euclid(modulus) as u64;
    let mut flags = (old & !0x8d5) | 2;
    if exact < 0 || exact >= modulus {
        flags |= 1;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    let nibble = if sub {
        (a & 15) as i32 - (b & 15) as i32 - carry as i32
    } else {
        (a & 15) as i32 + (b & 15) as i32 + carry as i32
    };
    if !(0..16).contains(&nibble) {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result & sign != 0 {
        flags |= 128;
    }
    let signed = |n: u64| {
        if n & sign != 0 {
            n as i128 - modulus
        } else {
            n as i128
        }
    };
    let full = if sub {
        signed(a) - signed(b) - carry as i128
    } else {
        signed(a) + signed(b) + carry as i128
    };
    if full < -(sign as i128) || full >= sign as i128 {
        flags |= 2048;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        let mut checks = 0;
        let values = [0u64, 1, 15, 127, 128, 255, 0x80000000, u64::MAX];
        for sub in [false, true] {
            for size in [1usize, 2, 4, 8] {
                for rex in [false, true] {
                    let count = if rex { 16u8 } else { 8 };
                    for dst in 0..count {
                        for src in 0..count {
                            for mode in 0..7 {
                                let imm = mode >= 3;
                                let memory = mode == 1 || mode == 2 || mode == 4 || mode == 6;
                                let store = mode == 2 || mode == 4 || mode == 6;
                                if imm && src >= 8 {
                                    continue;
                                }
                                let mut bytes = Vec::new();
                                if size == 2 {
                                    bytes.push(0x66);
                                }
                                let field = if imm {
                                    if sub { 3 } else { 2 }
                                } else if mode == 1 {
                                    dst
                                } else {
                                    src
                                };
                                let rm = if mode == 1 { src } else { dst };
                                if rex || size == 8 {
                                    bytes.push(
                                        0x40 | if size == 8 { 8 } else { 0 }
                                            | if imm { 0 } else { (field >> 3) << 2 }
                                            | (rm >> 3),
                                    );
                                }
                                let opcode = if imm {
                                    if size == 1 {
                                        0x80
                                    } else if mode <= 4 {
                                        0x83
                                    } else {
                                        0x81
                                    }
                                } else {
                                    (if sub { 0x18 } else { 0x10 })
                                        + u8::from(size != 1)
                                        + if mode == 1 { 2 } else { 0 }
                                };
                                bytes.extend_from_slice(&[
                                    opcode,
                                    (if memory { 0x40 } else { 0xc0 })
                                        | ((field & 7) << 3)
                                        | (rm & 7),
                                ]);
                                if memory {
                                    if rm & 7 == 4 {
                                        bytes.push(0x24);
                                    }
                                    bytes.push(0xf8);
                                }
                                let mut immediate = values[(src % 8) as usize];
                                if imm {
                                    let n = if size == 1 || mode <= 4 {
                                        1
                                    } else if size == 2 {
                                        2
                                    } else {
                                        4
                                    };
                                    bytes.extend_from_slice(&immediate.to_le_bytes()[..n]);
                                    if n == 1 && size > 1 {
                                        immediate = immediate as i8 as i64 as u64;
                                    } else if n == 4 && size == 8 {
                                        immediate = immediate as i32 as i64 as u64;
                                    }
                                }
                                bytes.push(0xc3);
                                let code = arena
                                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                let mask = u64::MAX >> (64 - size * 8);
                                let sign = 1u64 << (size * 8 - 1);
                                for value in [0u64, 1, 15, sign - 1, sign, mask] {
                                    for carry in 0..2 {
                                        for address in [0x1000u64, 0x1001, 0x100f, 0x1010] {
                                            for writable in [false, true] {
                                                if !memory && (address != 0x1000 || writable) {
                                                    continue;
                                                }
                                                let mut data = [0u8; 16];
                                                data[..8].copy_from_slice(&value.to_le_bytes());
                                                data[8..].copy_from_slice(&value.to_le_bytes());
                                                let mut expected_data = data;
                                                let mut memory_map = Memory::default();
                                                memory_map
                                                    .add(0x1000, &mut data, writable, false)
                                                    .unwrap();
                                                let old = 0xadfe | carry;
                                                let mut cpu = Cpu {
                                                    rip: 0x100,
                                                    fuel: 2,
                                                    flags: old,
                                                    memory: (&mut memory_map as *mut Memory<'_>)
                                                        as u64,
                                                    registers: [value; 16],
                                                    ..Cpu::default()
                                                };
                                                let (dr, ds) = location(dst, size, rex);
                                                let (sr, ss) = location(src, size, rex);
                                                if !imm && mode != 1 {
                                                    cpu.registers[sr] = (cpu.registers[sr]
                                                        & !(mask << ss))
                                                        | ((mask << ss) & 0xa55aa55aa55aa55a);
                                                }
                                                if memory {
                                                    cpu.registers[rm as usize] = address + 8;
                                                }
                                                let mut expected = cpu.registers;
                                                let readable =
                                                    !memory || address + size as u64 <= 0x1010;
                                                let valid = readable && (!store || writable);
                                                let mut flags = old;
                                                if valid {
                                                    let mut mem_value = 0;
                                                    if memory {
                                                        let at = (address - 0x1000) as usize;
                                                        let mut raw = [0u8; 8];
                                                        raw[..size].copy_from_slice(
                                                            &expected_data[at..at + size],
                                                        );
                                                        mem_value = u64::from_le_bytes(raw);
                                                    }
                                                    let a = if store {
                                                        mem_value
                                                    } else {
                                                        expected[dr] >> ds
                                                    };
                                                    let b = if imm {
                                                        immediate
                                                    } else if mode == 1 {
                                                        mem_value
                                                    } else {
                                                        expected[sr] >> ss
                                                    };
                                                    let (result, f) =
                                                        reference(a, b, size, sub, old);
                                                    flags = f;
                                                    if store {
                                                        let at = (address - 0x1000) as usize;
                                                        expected_data[at..at + size]
                                                            .copy_from_slice(
                                                                &result.to_le_bytes()[..size],
                                                            );
                                                    } else {
                                                        expected[dr] = if size == 4 {
                                                            result
                                                        } else {
                                                            (expected[dr] & !(mask << ds))
                                                                | (result << ds)
                                                        };
                                                    }
                                                }
                                                run(&mut cpu);
                                                assert_eq!(
                                                    cpu.registers, expected,
                                                    "{sub}/{size}/{rex}/{dst}/{src}/{mode}/{value:x}/{carry}/{address:x}"
                                                );
                                                assert_eq!(
                                                    cpu.flags, flags,
                                                    "flags {sub}/{size}/{mode}/{value:x}/{carry}"
                                                );
                                                assert_eq!(
                                                    memory_map.read(0x1000, 16).unwrap(),
                                                    expected_data
                                                );
                                                assert_eq!(
                                                    (cpu.fault, cpu.fuel, cpu.stop),
                                                    if valid {
                                                        (0, 0, 3)
                                                    } else {
                                                        (
                                                            if store && readable { 2 } else { 1 },
                                                            1,
                                                            0,
                                                        )
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
        // Exhaust both byte operands and incoming carry independently of the
        // encoding matrix above, including every signed-overflow boundary.
        for sub in [false, true] {
            let code = arena
                .publish(
                    &direct_cpu::translate(&[if sub { 0x18 } else { 0x10 }, 0xc8, 0xc3], 0x100, 2)
                        .unwrap(),
                )
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for a in 0..256u64 {
                for b in 0..256u64 {
                    for carry in 0..2 {
                        let old = 0xadfe | carry;
                        let mut cpu = Cpu {
                            rip: 0x100,
                            fuel: 2,
                            flags: old,
                            ..Cpu::default()
                        };
                        cpu.registers[0] = 0x123456789abcde00 | a;
                        cpu.registers[1] = b;
                        let mut expected = cpu.registers;
                        let (result, flags) = reference(a, b, 1, sub, old);
                        expected[0] = (expected[0] & !255) | result;
                        run(&mut cpu);
                        assert_eq!(cpu.registers, expected);
                        assert_eq!(cpu.flags, flags, "exhaustive {sub}/{a}/{b}/{carry}");
                        assert_eq!((cpu.fault, cpu.fuel, cpu.stop), (0, 0, 3));
                        checks += 1;
                    }
                }
            }
        }
        println!("direct ADC/SBB: {checks} native state checks passed");
    }
}
