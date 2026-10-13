use native_ps5::{
    direct_cpu,
    guest_memory::Memory,
    jit_memory::JitArena,
    legacy_cpu::Cpu,
    loader::Image,
    module_executor::{Execution, NativeExecutor, memory_fixture},
    modules::Arguments,
};
fn main() {
    unsafe {
        let arena = JitArena::new(2 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for reg in 0..16u8 {
            for indirect_memory in [false, true] {
                let mut bytes = vec![0x40 | (reg >> 3), 0xff];
                bytes.push(if indirect_memory {
                    0x50 | (reg & 7)
                } else {
                    0xd0 | (reg & 7)
                });
                if indirect_memory {
                    if reg & 7 == 4 {
                        bytes.push(0x24);
                    }
                    bytes.push(0xf8); // [register-8], including pre-push RSP
                }
                let code = arena
                    .publish(&direct_cpu::translate(&bytes, 0x100, 1).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for address in [0x1000u64, 0x1001, 0x1008, 0x1009, u64::MAX] {
                    for fuel in [0, 1, 10] {
                        for prior_fault in [0, 9] {
                            let mut data = [0xa5; 16];
                            let mut memory = Memory::default();
                            memory.add(0x1000, &mut data, false, false).unwrap();
                            let mut cpu = Cpu {
                                rip: 0x100,
                                bias: 0x700000,
                                fuel,
                                fault: prior_fault,
                                flags: 0xa55a,
                                target: 77,
                                next_rip: 88,
                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                ..Cpu::default()
                            };
                            cpu.registers.fill(0x12345678);
                            cpu.registers[reg as usize] = if indirect_memory {
                                address.wrapping_add(8)
                            } else {
                                address
                            };
                            let old = cpu.clone();
                            run(&mut cpu);
                            assert_eq!(cpu.registers, old.registers);
                            assert_eq!(cpu.flags, old.flags);
                            assert_eq!(cpu.rip, 0x100);
                            assert_eq!(memory.read(0x1000, 16).unwrap(), [0xa5; 16]);
                            if prior_fault != 0 || fuel == 0 {
                                assert_eq!(cpu.fault, prior_fault);
                                assert_eq!(cpu.fuel, fuel);
                                assert_eq!(cpu.stop, if prior_fault == 0 { 4 } else { 0 });
                                assert_eq!((cpu.target, cpu.next_rip), (77, 88));
                            } else if indirect_memory && !(0x1000..=0x1008).contains(&address) {
                                assert_eq!(
                                    (cpu.fault, cpu.fuel, cpu.stop, cpu.target, cpu.next_rip),
                                    (1, fuel - 1, 0, 77, 88)
                                );
                            } else {
                                assert_eq!((cpu.fault, cpu.fuel, cpu.stop), (0, fuel - 1, 1));
                                assert_eq!(
                                    cpu.target,
                                    if indirect_memory {
                                        0xa5a5a5a5a5a5a5a5
                                    } else {
                                        address
                                    }
                                );
                                assert_eq!(cpu.next_rip, 0x100 + bytes.len() as u64);
                            }
                            checks += 1;
                        }
                    }
                }
            }
        }
        // Actual executor: register, RIP-relative table, and pre-push [RSP].
        for form in 0..3 {
            let mut caller = vec![0x48, 0xb8];
            caller.extend_from_slice(&0x100140u64.to_le_bytes());
            let mut resume = Vec::new();
            if form == 2 {
                caller.extend_from_slice(&[
                    0x48, 0x89, 0x44, 0x24, 0xf8, // mov [rsp-8],rax
                    0x48, 0x83, 0xec, 8, // sub rsp,8
                    0xff, 0x14, 0x24, // call [rsp]
                ]);
                resume.extend_from_slice(&[0x48, 0x83, 0xc4, 8]); // discard pointer
            } else if form == 1 {
                caller.extend_from_slice(&[0xff, 0x15, 0x70, 0, 0, 0]);
            } else {
                caller.extend_from_slice(&[0xff, 0xd0]);
            }
            let continuation = 0x100 + caller.len();
            resume.extend_from_slice(&[0x83, 0xc0, 1, 0xc3]);
            let leaf = [0xb8, 41, 0, 0, 0, 0xc3];
            let mut file = memory_fixture();
            file[0x100..continuation].copy_from_slice(&caller);
            file[continuation..continuation + resume.len()].copy_from_slice(&resume);
            file[0x140..0x146].copy_from_slice(&leaf);
            file[0x180..0x188].copy_from_slice(&0x100140u64.to_le_bytes());
            let mut image = Image::load(&file, 0x100000, 4096).unwrap();
            let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
            for (at, bytes) in [
                (0x100, &caller[..]),
                (continuation, &resume[..]),
                (0x140, &leaf[..]),
            ] {
                executor
                    .register_direct_in_image(&image, at as u64, bytes, 4, &arena)
                    .unwrap();
            }
            assert!(matches!(
                executor
                    .run(&mut image, 0x100100, Arguments::default(), &mut [])
                    .unwrap(),
                Execution::Returned(42)
            ));
        }
        assert!(direct_cpu::translate(&[0xff, 0xd0, 0xc3], 0x100, 2).is_err());
        assert!(direct_cpu::translate(&[0x64, 0xff, 0x10], 0x100, 1).is_err());
        println!(
            "PASS: {checks} indirect CALL register/memory state, fuel and fault checks; register, RIP-relative and pre-push RSP executor calls return 42"
        );
    }
}
