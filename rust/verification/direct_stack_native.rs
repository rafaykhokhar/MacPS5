use native_ps5::{
    direct_cpu,
    guest_memory::Memory,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
use std::rc::Rc;
fn main() {
    unsafe {
        let arena = Rc::new(JitArena::new(2 * 1024 * 1024).unwrap());
        let mut checks = 0;
        for push in [false, true] {
            for reg in 0..16u8 {
                let bytes = [
                    0x40 | (reg >> 3),
                    (if push { 0x50 } else { 0x58 }) | (reg & 7),
                    0xc3,
                ];
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for sp in [0, 0x1000, 0x1001, 0x1008, 0x1009, 0x1010, u64::MAX] {
                    for writable in [false, true] {
                        for fuel in [0, 1, 2] {
                            let mut data = [0xa5; 16];
                            let mut memory = Memory::default();
                            memory.add(0x1000, &mut data, writable, false).unwrap();
                            let mut cpu = Cpu {
                                rip: 0x100,
                                flags: 0xa55a,
                                fuel,
                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                ..Cpu::default()
                            };
                            cpu.registers.fill(0xfedcba9876543210);
                            cpu.registers[4] = sp;
                            let old = cpu.clone();
                            let at = if push { sp.wrapping_sub(8) } else { sp };
                            let valid = (0x1000..=0x1008).contains(&at) && (!push || writable);
                            run(&mut cpu);
                            let mut registers = old.registers;
                            let mut expected = [0xa5; 16];
                            if fuel > 0 && valid {
                                registers[4] = if push {
                                    sp.wrapping_sub(8)
                                } else {
                                    sp.wrapping_add(8)
                                };
                                if push {
                                    expected[(at - 0x1000) as usize..(at - 0x1000) as usize + 8]
                                        .copy_from_slice(
                                            &old.registers[reg as usize].to_le_bytes(),
                                        );
                                } else {
                                    registers[reg as usize] = 0xa5a5a5a5a5a5a5a5;
                                }
                            }
                            assert_eq!(cpu.registers, registers);
                            assert_eq!(cpu.flags, old.flags);
                            assert_eq!(memory.read(0x1000, 16).unwrap(), expected);
                            assert_eq!(
                                cpu.fault,
                                if fuel > 0 && !valid {
                                    if push { 2 } else { 1 }
                                } else {
                                    0
                                }
                            );
                            assert_eq!(cpu.fuel, if fuel > 0 && !valid { fuel - 1 } else { 0 });
                            assert_eq!(
                                cpu.stop,
                                if fuel == 0 || (valid && fuel == 1) {
                                    4
                                } else if valid {
                                    3
                                } else {
                                    0
                                }
                            );
                            checks += 1;
                        }
                    }
                }
            }
        }
        // Discover a conventional frame, save/restore a register, and return.
        let body = [
            0x55, 0x48, 0x89, 0xe5, 0x53, 0xbb, 42, 0, 0, 0, 0x89, 0xd8, 0x5b, 0x5d, 0xc3,
        ];
        let mut file = memory_fixture();
        file[0x100..0x100 + body.len()].copy_from_slice(&body);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_source(&image, 8, arena.clone())
            .unwrap();
        executor.set_automatic_translation(true);
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(42)
        ));
        println!(
            "PASS: {checks} register PUSH/POP cases covering all registers, RSP aliases, guarded faults, boundaries and fuel; automatic frame returns 42"
        );
    }
}
