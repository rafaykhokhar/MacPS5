use native_ps5::{
    direct_cpu,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    unsafe {
        let arena = JitArena::new(4 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            let bits = if wide { 64 } else { 32 };
            let modulus = 1u128 << bits;
            let mask = (modulus - 1) as u64;
            let sign = 1u64 << (bits - 1);
            for (op, opcode) in [0x01, 0x29, 0x39, 0x21, 0x09, 0x31, 0x85]
                .into_iter()
                .enumerate()
            {
                for dst in 0..16u8 {
                    for src in 0..16u8 {
                        let bytes = [
                            0x40 | if wide { 8 } else { 0 } | ((src >> 3) << 2) | (dst >> 3),
                            opcode,
                            0xc0 | ((src & 7) << 3) | (dst & 7),
                            0xc3,
                        ];
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for a in [0, 1, sign - 1, sign, mask, 0x123456789abcdef0] {
                            for b in [0, 1, sign, mask] {
                                let mut cpu = Cpu {
                                    rip: 0x100,
                                    fuel: 2,
                                    flags: 0xa55a,
                                    ..Cpu::default()
                                };
                                cpu.registers.fill(0xfeed);
                                cpu.registers[dst as usize] = a;
                                cpu.registers[src as usize] = b;
                                let old = cpu.clone();
                                let a = old.registers[dst as usize] & mask;
                                let b = old.registers[src as usize] & mask;
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
                                let overflow =
                                    op <= 2 && (exact < -(sign as i128) || exact >= sign as i128);
                                let carry = if op == 0 {
                                    (a as u128 + b as u128) >= modulus
                                } else {
                                    op <= 2 && a < b
                                };
                                let mut flags = (old.flags & !0x8d5) | 2;
                                if carry {
                                    flags |= 1;
                                }
                                if (result as u8).count_ones() % 2 == 0 {
                                    flags |= 4;
                                }
                                if op <= 2
                                    && (if op == 0 {
                                        (a & 15) + (b & 15) > 15
                                    } else {
                                        (a & 15) < (b & 15)
                                    })
                                {
                                    flags |= 16;
                                }
                                if result == 0 {
                                    flags |= 64;
                                }
                                if result & sign != 0 {
                                    flags |= 128;
                                }
                                if overflow {
                                    flags |= 2048;
                                }
                                let mut registers = old.registers;
                                if !matches!(op, 2 | 6) {
                                    registers[dst as usize] = result;
                                }
                                run(&mut cpu);
                                assert_eq!(cpu.registers, registers);
                                assert_eq!(cpu.flags, flags);
                                assert_eq!(
                                    (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                    (0, 3, 0, 0x103)
                                );
                                checks += 1;
                            }
                        }
                    }
                }
            }
        }
        // MOV EAX,0; MOV ECX,10000; MOV EDX,1; ADD EAX,ECX;
        // SUB ECX,EDX; JNE loop; RET: sum 1..10000.
        let bytes = [
            0xb8, 0, 0, 0, 0, 0xb9, 0x10, 0x27, 0, 0, 0xba, 1, 0, 0, 0, 0x01, 0xc8, 0x29, 0xd1,
            0x75, 0xfa, 0xc3,
        ];
        let mut file = memory_fixture();
        file[0x100..0x100 + bytes.len()].copy_from_slice(&bytes);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 40000, 100).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &bytes, 7, &arena)
            .unwrap();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(50_005_000)
        ));
        println!(
            "PASS: {checks} direct arithmetic result/flag/alias checks and actual executor sum loop 50005000"
        );
    }
}
