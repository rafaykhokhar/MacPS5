use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for reg in 0..16u8 {
                let bytes = [
                    0x40 | if wide { 8 } else { 0 } | (reg >> 3),
                    0x0f,
                    0xc8 | (reg & 7),
                    0xc3,
                ];
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for position in 0..8 {
                    for byte in 0..256u64 {
                        for (fuel, prior) in [(0, 0), (1, 0), (2, 0), (2, 9)] {
                            let value = (0x123456789abcdef0u64 & !(255 << (position * 8)))
                                | (byte << (position * 8));
                            let mut cpu = Cpu {
                                rip: 0x100,
                                fuel,
                                fault: prior,
                                flags: 0xadff,
                                registers: [0xfedcba9876543210; 16],
                                ..Cpu::default()
                            };
                            cpu.registers[reg as usize] = value;
                            let mut expected = cpu.registers;
                            let executed = fuel > 0 && prior == 0;
                            if executed {
                                let size = if wide { 8 } else { 4 };
                                let mut result = 0u64;
                                for index in 0..size {
                                    result = result * 256 + ((value >> (index * 8)) & 255);
                                }
                                expected[reg as usize] = result;
                            }
                            run(&mut cpu);
                            assert_eq!(cpu.registers, expected);
                            assert_eq!(cpu.flags, 0xadff);
                            assert_eq!(
                                (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
                                (
                                    prior,
                                    if prior != 0 {
                                        0
                                    } else if fuel < 2 {
                                        4
                                    } else {
                                        3
                                    },
                                    if prior != 0 { fuel } else { 0 },
                                    if executed { 0x103 } else { 0x100 }
                                )
                            );
                            checks += 1;
                        }
                    }
                }
            }
        }
        println!("direct BSWAP: {checks} native state checks passed");
    }
}
