use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn reference(a: u8, b: u8, test: bool, old: u64) -> u64 {
    let result = if test { a & b } else { a.wrapping_sub(b) };
    let mut flags = (old & !0x8d5) | 2;
    if !test && a < b {
        flags |= 1;
    }
    if result.count_ones() % 2 == 0 {
        flags |= 4;
    }
    if !test && (a & 15) < (b & 15) {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result & 128 != 0 {
        flags |= 128;
    }
    let difference = a as i8 as i16 - b as i8 as i16;
    if !test && !(-128..128).contains(&difference) {
        flags |= 2048;
    }
    flags
}
fn main() {
    unsafe {
        let arena = JitArena::new(16 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for form in 0..5 {
            let immediate = form == 0 || form == 3;
            for rex in [false, true] {
                let count = if rex { 16 } else { 8 };
                for base in 0..count {
                    for source in 0..if immediate { 8 } else { count } {
                        let imm = [0, 1, 15, 16, 127, 128, 254, 255][(source % 8) as usize];
                        let regfield = if immediate {
                            if form == 0 { 7 } else { 0 }
                        } else {
                            source & 7
                        };
                        let mut bytes = Vec::new();
                        if rex {
                            bytes.push(
                                0x40 | if !immediate { (source >> 3) << 2 } else { 0 }
                                    | (base >> 3),
                            );
                        }
                        bytes.extend_from_slice(&[
                            [0x80, 0x38, 0x3a, 0xf6, 0x84][form],
                            0x40 | (regfield << 3) | (base & 7),
                        ]);
                        if base & 7 == 4 {
                            bytes.push(0x24);
                        }
                        bytes.push(0xf8);
                        if immediate {
                            bytes.push(imm);
                        }
                        bytes.push(0xc3);
                        let code = arena
                            .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                            .unwrap();
                        let run: unsafe extern "C" fn(*mut Cpu) =
                            std::mem::transmute(code.entry(0).unwrap());
                        for a in 0..256u16 {
                            for b in [0u8, 1, 15, 16, 127, 128, 254, 255] {
                                if immediate && b != 0 {
                                    continue;
                                }
                                for address in [0x1000u64, 0x100f, 0x1010] {
                                    let mut data = [a as u8; 16];
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
                                    cpu.registers.fill(0xdeadbeef12345678);
                                    let (reg, shift) = if !rex && source >= 4 {
                                        (source - 4, 8)
                                    } else {
                                        (source, 0)
                                    };
                                    if !immediate {
                                        cpu.registers[reg as usize] = (cpu.registers[reg as usize]
                                            & !(255 << shift))
                                            | ((b as u64) << shift);
                                    }
                                    cpu.registers[base as usize] = address + 8;
                                    let expected = cpu.registers;
                                    let other = if immediate {
                                        imm
                                    } else {
                                        (expected[reg as usize] >> shift) as u8
                                    };
                                    let valid = address < 0x1010;
                                    let flags = if valid {
                                        let (left, right) = if form == 2 {
                                            (other, a as u8)
                                        } else {
                                            (a as u8, other)
                                        };
                                        reference(left, right, form >= 3, cpu.flags)
                                    } else {
                                        cpu.flags
                                    };
                                    run(&mut cpu);
                                    assert_eq!(cpu.registers, expected);
                                    assert_eq!(
                                        cpu.flags, flags,
                                        "{form}/{rex}/{base}/{source}/{a}/{b}/{address:x}"
                                    );
                                    assert_eq!(
                                        (cpu.fault, cpu.fuel, cpu.stop),
                                        if valid { (0, 0, 3) } else { (1, 1, 0) }
                                    );
                                    assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                    checks += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("direct byte memory comparisons: {checks} native state checks passed");
    }
}
