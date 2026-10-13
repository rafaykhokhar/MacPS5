use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn bytes(wide: bool, dst: u8, src: u8, control: u8, mem: bool) -> Vec<u8> {
    let mut b = vec![
        0xc4,
        0x42 | if dst < 8 { 0x80 } else { 0 } | if src < 8 { 0x20 } else { 0 },
        ((!control & 15) << 3) | if wide { 0x80 } else { 0 },
        0xf7,
        (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7),
    ];
    if mem {
        if src & 7 == 4 {
            b.push(0x24);
        }
        b.push(0xf8);
    }
    b.push(0xc3);
    b
}
fn reference(value: u64, control: u64, bits: usize) -> u64 {
    let start = (control & 255) as usize;
    let len = ((control >> 8) & 255) as usize;
    let mut out = 0;
    for n in 0..bits {
        if n < len && start + n < bits && value & (1 << (start + n)) != 0 {
            out |= 1 << n;
        }
    }
    out
}
unsafe fn check(
    run: unsafe extern "C" fn(*mut Cpu),
    wide: bool,
    dst: u8,
    src: u8,
    ctrl: u8,
    mem: bool,
    value: u64,
    control: u64,
    address: u64,
) {
    let mut data = [0u8; 16];
    data[..8].copy_from_slice(&value.to_le_bytes());
    data[8..].copy_from_slice(&value.to_le_bytes());
    let original = data;
    let mut memory = Memory::default();
    memory.add(0x1000, &mut data, false, false).unwrap();
    let mut cpu = Cpu {
        rip: 0x100,
        fuel: 2,
        flags: 0xa55b,
        memory: (&mut memory as *mut Memory<'_>) as u64,
        ..Cpu::default()
    };
    cpu.registers.fill(0xdeadbeefabcdef01);
    cpu.registers[src as usize] = value;
    cpu.registers[ctrl as usize] = control;
    if mem {
        cpu.registers[src as usize] = address.wrapping_add(8);
    }
    let mut expected = cpu.registers;
    let size = if wide { 8 } else { 4 };
    let valid =
        !mem || (address >= 0x1000 && address.checked_add(size).is_some_and(|end| end <= 0x1010));
    let mut flags = cpu.flags;
    if valid {
        let value = if mem {
            let at = (address - 0x1000) as usize;
            let mut raw = [0u8; 8];
            raw[..size as usize].copy_from_slice(&original[at..at + size as usize]);
            u64::from_le_bytes(raw)
        } else {
            expected[src as usize]
        };
        let result = reference(value, expected[ctrl as usize], size as usize * 8);
        expected[dst as usize] = result;
        flags = (flags & !0x841) | if result == 0 { 64 } else { 0 };
    }
    unsafe { run(&mut cpu) };
    assert_eq!(
        cpu.registers, expected,
        "{wide}/{dst}/{src}/{ctrl}/{mem}/{value:x}/{control:x}/{address:x}"
    );
    assert_eq!(cpu.flags, flags);
    assert_eq!(
        (cpu.fault, cpu.fuel, cpu.stop),
        if valid { (0, 0, 3) } else { (1, 1, 0) }
    );
    assert_eq!(memory.read(0x1000, 16).unwrap(), original);
}
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for dst in 0..16 {
                for src in 0..16 {
                    for ctrl in 0..16 {
                        for mem in [false, true] {
                            let code = arena
                                .publish(
                                    &direct_cpu::translate(
                                        &bytes(wide, dst, src, ctrl, mem),
                                        0x100,
                                        2,
                                    )
                                    .unwrap(),
                                )
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for control in [0u64, 1, 0x0100, 0x2020, 0xffff, 0xff00] {
                                for value in [0u64, 0x81234567abcdef01] {
                                    for address in [0x1000u64, 0x1001, 0x100d, u64::MAX - 7] {
                                        if !mem && address != 0x1000 {
                                            continue;
                                        }
                                        check(
                                            run,
                                            wide,
                                            dst,
                                            src,
                                            ctrl,
                                            mem,
                                            value,
                                            control | 0xaabbccddffff0000,
                                            address,
                                        );
                                        checks += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let code = arena
                .publish(&direct_cpu::translate(&bytes(wide, 0, 2, 1, false), 0x100, 2).unwrap())
                .unwrap();
            let run: unsafe extern "C" fn(*mut Cpu) = std::mem::transmute(code.entry(0).unwrap());
            for control in 0..65536u64 {
                for value in [
                    0u64,
                    1,
                    u64::MAX,
                    0x8000000000000000,
                    0x80000000,
                    0x123456789abcdef0,
                ] {
                    check(run, wide, 0, 2, 1, false, value, control, 0x1000);
                    checks += 1;
                }
            }
        }
        println!("direct BEXTR: {checks} native state checks passed");
    }
}
