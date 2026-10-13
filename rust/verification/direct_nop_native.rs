use native_ps5::{direct_cpu, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for prefix in [vec![], vec![0x66], vec![0x48], vec![0x64], vec![0x67]] {
            for operand in [
                vec![],
                vec![0],
                vec![0x44, 0x24, 0x80],
                vec![0x84, 0x8d, 0xff, 0xff, 0xff, 0x7f],
                vec![0xc0],
            ] {
                let mut bytes = prefix.clone();
                if operand.is_empty() {
                    bytes.push(0x90);
                } else {
                    bytes.extend_from_slice(&[0x0f, 0x1f]);
                }
                bytes.extend_from_slice(&operand);
                let end = 0x100 + bytes.len() as u64;
                bytes.push(0xc3);
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for fuel in [0, 1, 2] {
                    for fault in [0, 9] {
                        let mut cpu = Cpu {
                            rip: 0x100,
                            fuel,
                            fault,
                            flags: 0xa55a,
                            memory: 0,
                            ..Cpu::default()
                        };
                        cpu.registers.fill(u64::MAX);
                        run(&mut cpu);
                        assert_eq!(cpu.registers, [u64::MAX; 16]);
                        assert_eq!(cpu.flags, 0xa55a);
                        assert_eq!(cpu.fault, fault);
                        assert_eq!(cpu.fuel, if fault != 0 { fuel } else { 0 });
                        assert_eq!(cpu.rip, if fault != 0 || fuel == 0 { 0x100 } else { end });
                        assert_eq!(
                            cpu.stop,
                            if fault != 0 {
                                0
                            } else if fuel < 2 {
                                4
                            } else {
                                3
                            }
                        );
                        checks += 1;
                    }
                }
            }
        }
        println!(
            "PASS: {checks} multi-byte NOP cases with memory/register encodings, null memory, preserved flags/registers, fuel and prior faults"
        );
    }
}
