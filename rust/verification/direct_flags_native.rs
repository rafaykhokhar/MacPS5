use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(op: usize, flags: u64, rax: u64) -> (u64, u64) {
    let mut f = flags;
    let mut bytes = rax.to_le_bytes();
    match op {
        0 => f &= !1,
        1 => f |= 1,
        2 => f ^= 1,
        3 => f &= !(1 << 10),
        4 => f |= 1 << 10,
        5 => {
            bytes[1] = 2;
            for bit in [0, 2, 4, 6, 7] {
                if flags & (1 << bit) != 0 {
                    bytes[1] |= 1 << bit;
                }
            }
        }
        _ => {
            for bit in [0, 2, 4, 6, 7] {
                if bytes[1] & (1 << bit) != 0 {
                    f |= 1 << bit;
                } else {
                    f &= !(1 << bit);
                }
            }
        }
    }
    (f, u64::from_le_bytes(bytes))
}
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for (op, opcode) in [0xf8, 0xf9, 0xf5, 0xfc, 0xfd, 0x9f, 0x9e]
            .into_iter()
            .enumerate()
        {
            let code = arena
                .publish(&direct_cpu::translate(&[opcode, 0xc3], 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for low in 0..4096u64 {
                for ah in [0, 1, 0x55, 0xaa, 0xff] {
                    for mode in 0..4 {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            flags: (low & !0x28) | 2 | 0x240000,
                            registers: [0xabcdef0123456789; 16],
                            fuel: if mode == 3 { 2 } else { mode },
                            fault: if mode == 3 { 9 } else { 0 },
                            ..Cpu::default()
                        };
                        cpu.registers[0] = (cpu.registers[0] & !0xff00) | (ah << 8);
                        let mut expected = cpu.registers;
                        let mut flags = cpu.flags;
                        if mode == 1 || mode == 2 {
                            (flags, expected[0]) = reference(op, flags, expected[0]);
                        }
                        run(&mut cpu);
                        assert_eq!(
                            cpu.registers, expected,
                            "op={op} flags={low:x} ah={ah:x} mode={mode}"
                        );
                        assert_eq!(cpu.flags, flags);
                        assert_eq!(
                            (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                            match mode {
                                0 => (0, 4, 0, 0x100),
                                1 => (0, 4, 0, 0x101),
                                2 => (0, 3, 0, 0x101),
                                _ => (9, 0, 2, 0x100),
                            }
                        );
                        checks += 1;
                    }
                }
            }
            if op == 6 {
                for ah in 0..256u64 {
                    for low in 0..256u64 {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            fuel: 2,
                            flags: 0xad02 | (low & 0xd5),
                            registers: [u64::MAX; 16],
                            ..Cpu::default()
                        };
                        cpu.registers[0] = (cpu.registers[0] & !0xff00) | (ah << 8);
                        let (expected, rax) = reference(op, cpu.flags, cpu.registers[0]);
                        run(&mut cpu);
                        assert_eq!(cpu.flags, expected);
                        assert_eq!(cpu.registers[0], rax);
                        checks += 1;
                    }
                }
            }
            assert!(direct_cpu::translate(&[0xf0, opcode, 0xc3], 0x100, 2).is_err());
        }
        println!("direct flag control: {checks} native state checks passed; LOCK forms rejected");
    }
}
