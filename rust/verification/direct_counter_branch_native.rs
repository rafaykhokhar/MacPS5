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
        let arena = JitArena::new(1024 * 1024).unwrap();
        let mut checks = 0;
        for narrow in [false, true] {
            for kind in 0..4u8 {
                for layout in 0..3 {
                    let len = if narrow { 3u64 } else { 2 };
                    let mut bytes = Vec::new();
                    if narrow {
                        bytes.push(0x67);
                    }
                    bytes.extend_from_slice(&[
                        0xe0 + kind,
                        match layout {
                            0 => 0x20 - len as u8,
                            1 => (0u8).wrapping_sub(len as u8),
                            _ => 5,
                        },
                    ]);
                    if layout == 1 {
                        bytes.push(0xc3);
                    } else if layout == 2 {
                        bytes.extend_from_slice(&[0xb8, 42, 0, 0, 0, 0xc3]);
                    }
                    let target = match layout {
                        0 => 0x120,
                        1 => 0x100,
                        _ => 0x100 + len + 5,
                    };
                    let code = arena
                        .publish(&direct_cpu::translate(&bytes, 0x100, 3).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for count in [
                        0,
                        1,
                        2,
                        3,
                        17,
                        u32::MAX as u64,
                        1u64 << 32,
                        (1u64 << 32) + 1,
                        (1u64 << 32) + 2,
                        u64::MAX,
                    ] {
                        for flag_bits in 0..32u64 {
                            let flags = 0xa002
                                | (flag_bits & 1)
                                | ((flag_bits >> 1 & 1) << 2)
                                | ((flag_bits >> 2 & 1) << 6)
                                | ((flag_bits >> 3 & 1) << 7)
                                | ((flag_bits >> 4 & 1) << 11);
                            for fuel in [0, 1, 2, 3, 20, 100] {
                                for prior in [0, 9] {
                                    let mut cpu = Cpu {
                                        rip: 0x100,
                                        flags,
                                        fuel,
                                        fault: prior,
                                        registers: [99; 16],
                                        ..Cpu::default()
                                    };
                                    cpu.registers[1] = count;
                                    let mut regs = cpu.registers;
                                    let mut rip = cpu.rip;
                                    let mut remaining = fuel;
                                    let mut stop = 0;
                                    let mut out_target = 0;
                                    if prior == 0 {
                                        loop {
                                            if remaining == 0 {
                                                stop = 4;
                                                break;
                                            }
                                            remaining -= 1;
                                            if rip == 0x100 {
                                                let value = if narrow {
                                                    regs[1] as u32 as u64
                                                } else {
                                                    regs[1]
                                                };
                                                let taken = if kind == 3 {
                                                    value == 0
                                                } else {
                                                    regs[1] = if narrow {
                                                        (value as u32).wrapping_sub(1) as u64
                                                    } else {
                                                        value.wrapping_sub(1)
                                                    };
                                                    regs[1] != 0
                                                        && (kind == 2
                                                            || ((flags & 64 != 0) == (kind == 1)))
                                                };
                                                rip = if taken { target } else { 0x100 + len };
                                                if layout == 0 {
                                                    stop = 2;
                                                    out_target = rip;
                                                    break;
                                                }
                                            } else if layout == 2 && rip == 0x100 + len {
                                                regs[0] = 42;
                                                rip += 5;
                                            } else {
                                                stop = 3;
                                                break;
                                            }
                                        }
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, regs,
                                        "narrow={narrow} kind={kind} layout={layout} count={count:x} flags={flags:x} fuel={fuel}"
                                    );
                                    assert_eq!(cpu.flags, flags);
                                    assert_eq!(
                                        (cpu.rip, cpu.fuel, cpu.stop, cpu.fault, cpu.target),
                                        (rip, remaining, stop, prior, out_target)
                                    );
                                    checks += 1;
                                }
                            }
                        }
                    }
                    let mut invalid = Vec::new();
                    if narrow {
                        invalid.push(0x67);
                    }
                    invalid.extend_from_slice(&[0xe0 + kind, 0xff, 0xc3]);
                    assert!(direct_cpu::translate(&invalid, 0x100, 3).is_err());
                }
            }
        }
        let mut file = memory_fixture();
        let start = [0xb9, 3, 0, 0, 0, 0xe2, 0xfe];
        let finish = [0xb8, 42, 0, 0, 0, 0xc3];
        file[0x100..0x107].copy_from_slice(&start);
        file[0x107..0x10d].copy_from_slice(&finish);
        let mut image = Image::load(&file, 0x100000, 4096).unwrap();
        let mut executor = NativeExecutor::new(0x900000, 4096, 100, 1000).unwrap();
        executor
            .register_direct_in_image(&image, 0x100, &start, 2, &arena)
            .unwrap();
        executor
            .register_direct_in_image(&image, 0x107, &finish, 2, &arena)
            .unwrap();
        assert!(matches!(
            executor
                .run(&mut image, 0x100100, Arguments::default(), &mut [])
                .unwrap(),
            Execution::Returned(42)
        ));
        println!(
            "direct counter branches: {checks} native state checks; cross-block executor returned 42"
        );
    }
}
