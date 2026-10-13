use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn expected(op: usize, a: u64, b: u64, bits: u32, old: u64) -> (u64, u64) {
    let modulus = 1u128 << bits;
    let mask = (modulus - 1) as u64;
    let sign = 1u64 << (bits - 1);
    let (a, b) = (a & mask, b & mask);
    let result = match op {
        0 => a.wrapping_add(b),
        1 | 2 => a.wrapping_sub(b),
        3 | 6 => a & b,
        4 => a | b,
        5 => a ^ b,
        _ => unreachable!(),
    } & mask;
    let signed = |v: u64| {
        if v & sign == 0 {
            v as i128
        } else {
            v as i128 - modulus as i128
        }
    };
    let exact = if op == 0 {
        signed(a) + signed(b)
    } else {
        signed(a) - signed(b)
    };
    let mut flags = (old & !0x8d5) | 2;
    if if op == 0 {
        a as u128 + b as u128 >= modulus
    } else {
        op <= 2 && a < b
    } {
        flags |= 1;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    if op <= 2
        && if op == 0 {
            (a & 15) + (b & 15) > 15
        } else {
            (a & 15) < (b & 15)
        }
    {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result & sign != 0 {
        flags |= 128;
    }
    if op <= 2 && (exact < -(sign as i128) || exact >= sign as i128) {
        flags |= 2048;
    }
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for (op, digit) in [0, 5, 7, 4, 1, 6, 0].into_iter().enumerate() {
                for dst in 0..16u8 {
                    for form in 0..3 {
                        if (form == 1 && op == 6) || (form == 2 && dst != 0) {
                            continue;
                        }
                        for raw in [
                            0u32, 1, 0x7f, 0x80, 0xff, 0x80000000, 0xffffffff, 0x12345678,
                        ] {
                            let mut bytes = vec![0x40 | if wide { 8 } else { 0 } | (dst >> 3)];
                            if form == 2 {
                                bytes.push([0x05, 0x2d, 0x3d, 0x25, 0x0d, 0x35, 0xa9][op]);
                            } else {
                                bytes.extend_from_slice(&[
                                    if form == 1 {
                                        0x83
                                    } else if op == 6 {
                                        0xf7
                                    } else {
                                        0x81
                                    },
                                    0xc0 | (digit << 3) | (dst & 7),
                                ]);
                            }
                            if form == 1 {
                                bytes.push(raw as u8);
                            } else {
                                bytes.extend_from_slice(&raw.to_le_bytes());
                            }
                            bytes.push(0xc3);
                            let b = if form == 1 {
                                raw as u8 as i8 as i64 as u64
                            } else if wide {
                                raw as i32 as i64 as u64
                            } else {
                                u64::from(raw)
                            };
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for a in [0, 1, 0x7fffffff, 0x80000000, 1 << 63, u64::MAX] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xa55a,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0xfeed);
                                cpu.registers[dst as usize] = a;
                                let (result, flags) =
                                    expected(op, a, b, if wide { 64 } else { 32 }, cpu.flags);
                                let mut registers = cpu.registers;
                                if !matches!(op, 2 | 6) {
                                    registers[dst as usize] = result;
                                }
                                run(&mut cpu);
                                assert_eq!(cpu.registers, registers);
                                assert_eq!(cpu.flags, flags);
                                assert_eq!(
                                    (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                    (0, 3, 0, 0x100 + bytes.len() as u64 - 1)
                                );
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        let bytes = [
            0xb8, 0, 0, 0, 0, 0xb9, 0x10, 0x27, 0, 0, 0x01, 0xc8, 0x83, 0xe9, 1, 0x75, 0xf9, 0xc3,
        ];
        let mut file = memory_fixture();
        file[0x100..0x100 + bytes.len()].copy_from_slice(&bytes);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 40000, 100).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &bytes, 6, &arena)
            .unwrap();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(50_005_000)
        ));
        println!(
            "PASS: {checks} direct immediate arithmetic result/flags/register checks and executor decrement-immediate sum 50005000"
        );
    }
}
