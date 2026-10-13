use native_ps5::{
    arm64_code::{Arm64Code, Branch},
    guest_memory::{
        Memory,
        ffi::{native_ps5_module_load_v1, native_ps5_module_store_v1},
    },
    jit_memory::JitArena,
    legacy_cpu::Cpu,
};
fn stub(store: bool) -> Vec<u32> {
    let mut code = Arm64Code::new(32);
    code.emit(0xa9bf7bf3).unwrap(); // stp x19,x30,[sp,#-16]!
    code.emit(0xaa0003f3).unwrap(); // mov x19,x0
    code.call_absolute(if store {
        native_ps5_module_store_v1 as *const () as u64
    } else {
        native_ps5_module_load_v1 as *const () as u64
    })
    .unwrap();
    if !store {
        code.context_access(9, 19, std::mem::offset_of!(Cpu, fault), true, false)
            .unwrap();
        code.branch(
            1,
            Branch::CompareZero {
                register: 9,
                wide: true,
                nonzero: true,
            },
        )
        .unwrap();
        code.context_access(0, 19, 0, true, true).unwrap();
    }
    code.bind(1).unwrap();
    code.emit(0xa8c17bf3).unwrap(); // ldp x19,x30,[sp],#16
    code.emit(0xd65f03c0).unwrap();
    code.finish().unwrap()
}
fn main() {
    // SAFETY: standalone host thread, known generated Cpu/helper ABI. Memory
    // and Cpu remain exclusive/live throughout each synchronous native call.
    unsafe {
        let arena = JitArena::new(16384).unwrap();
        let load = arena.publish(&stub(false)).unwrap();
        let store = arena.publish(&stub(true)).unwrap();
        let load_fn: unsafe extern "C" fn(*mut Cpu, u64, u64) =
            std::mem::transmute(load.entry(0).unwrap());
        let store_fn: unsafe extern "C" fn(*mut Cpu, u64, u64, u64) =
            std::mem::transmute(store.entry(0).unwrap());
        let mut count = 0;
        for writable in [false, true] {
            for size in [1, 2, 4, 8, 0, 3, 16] {
                for address in [0xfff, 0x1000, 0x1001, 0x1008, 0x100f, 0x1010, u64::MAX] {
                    for prior_fault in [0, 7] {
                        let mut bytes = [0xa5; 16];
                        let mut memory = Memory::default();
                        memory.add(0x1000, &mut bytes, writable, false).unwrap();
                        let mut cpu = Cpu {
                            memory: (&mut memory as *mut Memory<'_>) as u64,
                            fault: prior_fault,
                            ..Cpu::default()
                        };
                        cpu.registers.fill(0x123456789abcdef0);
                        let expected = memory.load(address, size);
                        load_fn(&mut cpu, address, size);
                        assert_eq!(
                            cpu.fault,
                            if prior_fault != 0 {
                                prior_fault
                            } else if expected.is_ok() {
                                0
                            } else {
                                1
                            }
                        );
                        assert_eq!(
                            cpu.registers[0],
                            if prior_fault == 0 {
                                expected.unwrap_or(0x123456789abcdef0)
                            } else {
                                0x123456789abcdef0
                            }
                        );
                        assert!(cpu.registers[1..].iter().all(|v| *v == 0x123456789abcdef0));
                        cpu.fault = prior_fault;
                        let old = memory.read(0x1000, 16).unwrap().to_vec();
                        let valid = writable
                            && [1, 2, 4, 8].contains(&size)
                            && address >= 0x1000
                            && address.checked_add(size).is_some_and(|end| end <= 0x1010);
                        store_fn(&mut cpu, address, 0x1122334455667788, size);
                        let success = prior_fault == 0 && valid;
                        assert_eq!(
                            cpu.fault,
                            if prior_fault != 0 {
                                prior_fault
                            } else if valid {
                                0
                            } else {
                                2
                            }
                        );
                        let mut expected = old;
                        if success {
                            let at = (address - 0x1000) as usize;
                            expected[at..at + size as usize].copy_from_slice(
                                &0x1122334455667788u64.to_le_bytes()[..size as usize],
                            );
                        }
                        assert_eq!(memory.read(0x1000, 16).unwrap(), expected);
                        count += 2;
                    }
                }
            }
        }
        println!(
            "PASS: {count} direct guarded load/store calls, context state, prior faults, permissions, bounds and unchanged failed writes"
        );
    }
}
