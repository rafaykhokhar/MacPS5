use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(a: u64, b: u64, size: usize, op: usize, old: u64) -> (u64, u64) {
    let bits = size * 8;
    let modulus = 1i128 << bits;
    let mask = (modulus - 1) as u64;
    let sign = 1u64 << (bits - 1);
    let a = a & mask;
    let b = b & mask;
    let result = match op {
        0 => a.wrapping_add(b),
        1 => a.wrapping_sub(b),
        2 => a & b,
        3 => a | b,
        _ => a ^ b,
    } & mask;
    let mut flags = (old & !0x8d5) | 2;
    if (op == 0 && a as u128 + b as u128 > mask as u128) || (op == 1 && a < b) {
        flags |= 1;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    if (op == 0 && (a & 15) + (b & 15) > 15) || (op == 1 && (a & 15) < (b & 15)) {
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
    let exact = if op == 0 {
        signed(a) + signed(b)
    } else {
        signed(a) - signed(b)
    };
    if op < 2 && (exact < -(sign as i128) || exact >= sign as i128) {
        flags |= 2048;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        let values = [
            0u64,
            1,
            0x7fffffff,
            0x80000000,
            0x7fffffffffffffff,
            u64::MAX,
        ];
        for op in 0..5usize {
            for size in [1usize, 2, 4, 8] {
                for base in 0..16u8 {
                    for mode in 0..3 {
                        for source in 0..if mode == 0 { 16 } else { 6 } {
                            if size == 1 && mode == 2 {
                                continue;
                            }
                            let mut bytes = Vec::new();
                            if size == 2 {
                                bytes.push(0x66);
                            }
                            bytes.push(
                                0x40 | if size == 8 { 8 } else { 0 }
                                    | if mode == 0 { (source >> 3) << 2 } else { 0 }
                                    | (base >> 3),
                            );
                            let regfield = if mode == 0 {
                                source & 7
                            } else {
                                [0, 5, 4, 1, 6][op]
                            };
                            let opcode = if mode == 0 {
                                [0x00, 0x28, 0x20, 0x08, 0x30][op] + u8::from(size != 1)
                            } else if size == 1 {
                                0x80
                            } else if mode == 1 {
                                0x83
                            } else {
                                0x81
                            };
                            bytes.extend_from_slice(&[opcode, 0x40 | (regfield << 3) | (base & 7)]);
                            if base & 7 == 4 {
                                bytes.push(0x24);
                            }
                            bytes.push(0xf8);
                            let immediate = if mode == 1 {
                                let v = [0u8, 1, 15, 127, 128, 255][source as usize];
                                bytes.push(v);
                                if size == 1 {
                                    v as u64
                                } else {
                                    v as i8 as i64 as u64
                                }
                            } else if mode == 2 {
                                let v = values[source as usize];
                                let n = if size == 2 { 2 } else { 4 };
                                bytes.extend_from_slice(&v.to_le_bytes()[..n]);
                                if size == 8 { v as i32 as i64 as u64 } else { v }
                            } else {
                                0
                            };
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for a in values {
                                for address in [0x1000u64, 0x1001, 0x100f, 0x1010] {
                                    for writable in [false, true] {
                                        let mut data = [0u8; 16];
                                        data[..8].copy_from_slice(&a.to_le_bytes());
                                        data[8..].copy_from_slice(&a.to_le_bytes());
                                        let mut expected_data = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, writable, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fuel: 2,
                                            flags: 0xa55b,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            ..Cpu::default()
                                        };
                                        cpu.registers.fill(a.rotate_left(17) ^ 0x123456789abcdef0);
                                        cpu.registers[base as usize] = address + 8;
                                        let registers = cpu.registers;
                                        let rhs = if mode == 0 {
                                            registers[source as usize]
                                        } else {
                                            immediate
                                        };
                                        let readable = address + size as u64 <= 0x1010;
                                        let valid = readable && writable;
                                        let mut flags = cpu.flags;
                                        if valid {
                                            let at = (address - 0x1000) as usize;
                                            let mut raw = [0u8; 8];
                                            raw[..size]
                                                .copy_from_slice(&expected_data[at..at + size]);
                                            let (result, f) = reference(
                                                u64::from_le_bytes(raw),
                                                rhs,
                                                size,
                                                op,
                                                flags,
                                            );
                                            flags = f;
                                            expected_data[at..at + size]
                                                .copy_from_slice(&result.to_le_bytes()[..size]);
                                        }
                                        run(&mut cpu);
                                        assert_eq!(cpu.registers, registers);
                                        assert_eq!(
                                            cpu.flags, flags,
                                            "{op}/{size}/{base}/{mode}/{source}/{a:x}/{address:x}/{writable}"
                                        );
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
                                        assert_eq!(
                                            (cpu.fault, cpu.fuel, cpu.stop),
                                            if valid {
                                                (0, 0, 3)
                                            } else {
                                                (if readable { 2 } else { 1 }, 1, 0)
                                            }
                                        );
                                        checks += 1;
                                    }
                                }
                            }
                            let mut locked = vec![0xf0];
                            locked.extend_from_slice(&bytes);
                            assert!(direct_cpu::translate(&locked, 0x100, 2).is_err());
                        }
                    }
                }
            }
        }
        // Legacy high-byte register sources, including aliases with the address base.
        for (op, opcode) in [0x00, 0x28, 0x20, 0x08, 0x30].into_iter().enumerate() {
            for base in 0..8u8 {
                for src in 4..8u8 {
                    let mut bytes = vec![opcode, 0x40 | (src << 3) | base];
                    if base == 4 {
                        bytes.push(0x24);
                    }
                    bytes.extend_from_slice(&[0xf8, 0xc3]);
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    let mut data = [0x7f];
                    let mut memory = Memory::default();
                    memory.add(0x1000, &mut data, true, false).unwrap();
                    let mut cpu = Cpu {
                        rip: 0x100,
                        fuel: 2,
                        flags: 0xa55b,
                        memory: (&mut memory as *mut Memory<'_>) as u64,
                        registers: [0xabcdef8001; 16],
                        ..Cpu::default()
                    };
                    cpu.registers[base as usize] = 0x1008;
                    let registers = cpu.registers;
                    let (result, flags) = reference(
                        0x7f,
                        (registers[(src - 4) as usize] >> 8) & 255,
                        1,
                        op,
                        cpu.flags,
                    );
                    run(&mut cpu);
                    assert_eq!(memory.read(0x1000, 1).unwrap(), [result as u8]);
                    assert_eq!(cpu.flags, flags);
                    assert_eq!(cpu.registers, registers);
                    assert_eq!((cpu.fault, cpu.stop), (0, 3));
                    checks += 1;
                }
            }
        }
        use native_ps5::guest_memory::ffi::native_ps5_module_integer_rmw_v1 as rmw;
        rmw(std::ptr::null_mut(), 0, 1, 4, 0);
        for (size, op, prior, expected) in [
            (0, 0, 0, 3),
            (3, 0, 0, 3),
            (8, 6, 0, 3),
            (4, 0, 0, 1),
            (4, 0, 9, 9),
        ] {
            let mut cpu = Cpu {
                flags: 0xa55b,
                fault: prior,
                ..Cpu::default()
            };
            rmw(&mut cpu, 0x1000, 1, size, op);
            assert_eq!((cpu.flags, cpu.fault), (0xa55b, expected));
            checks += 1;
        }
        println!("direct memory arithmetic: {checks} native state checks passed");
    }
}
