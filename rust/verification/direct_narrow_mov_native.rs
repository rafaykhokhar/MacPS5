use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn location(encoded: u8, byte: bool, rex: bool) -> (usize, u32) {
    if byte && !rex && encoded >= 4 {
        ((encoded - 4) as usize, 8)
    } else {
        (encoded as usize, 0)
    }
}
fn main() {
    unsafe {
        let arena = JitArena::new(8 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for byte in [false, true] {
            for store in [false, true] {
                for rex in [false, true] {
                    let count = if rex { 16 } else { 8 };
                    let size = if byte { 1 } else { 2 };
                    let mask = if byte { 255 } else { 65535 };
                    for reg in 0..count {
                        for rm in 0..count {
                            for mem in [false, true] {
                                let mut bytes = Vec::new();
                                if !byte {
                                    bytes.push(0x66);
                                }
                                if rex {
                                    bytes.push(0x40 | ((reg >> 3) << 2) | (rm >> 3));
                                }
                                bytes.extend_from_slice(&[
                                    (if store { 0x88 } else { 0x8a }) + u8::from(!byte),
                                    (if mem { 0x40 } else { 0xc0 }) | ((reg & 7) << 3) | (rm & 7),
                                ]);
                                if mem {
                                    if rm & 7 == 4 {
                                        bytes.push(0x24);
                                    }
                                    bytes.push(0xf8);
                                }
                                bytes.push(0xc3);
                                let code = arena
                                    .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                for value in [0u64, 0x80, 0xff, 0x8000, 0xffff, 0x0123456789abcdef]
                                {
                                    for address in [0x1000u64, 0x1001, 0x100f, 0x1010, u64::MAX - 7]
                                    {
                                        for writable in [false, true] {
                                            if !mem && (address != 0x1000 || writable) {
                                                continue;
                                            }
                                            let mut data = [0xa5u8; 16];
                                            let mut expected_data = data;
                                            let mut memory = Memory::default();
                                            memory.add(0x1000, &mut data, writable, false).unwrap();
                                            let mut cpu = Cpu {
                                                rip: 0x100,
                                                fuel: 2,
                                                flags: 0xa55a,
                                                memory: (&mut memory as *mut Memory<'_>) as u64,
                                                ..Cpu::default()
                                            };
                                            for (n, r) in cpu.registers.iter_mut().enumerate() {
                                                *r = 0xfedcba9876543210u64
                                                    .wrapping_mul(n as u64 + 1)
                                                    ^ value;
                                            }
                                            if mem {
                                                cpu.registers[rm as usize] =
                                                    address.wrapping_add(8);
                                            }
                                            let mut expected = cpu.registers;
                                            let valid = !mem
                                                || (address >= 0x1000
                                                    && address
                                                        .checked_add(size)
                                                        .is_some_and(|end| end <= 0x1010)
                                                    && (!store || writable));
                                            if valid {
                                                let (rr, rs) = location(reg, byte, rex);
                                                let (mr, ms) = location(rm, byte, rex);
                                                if mem && store {
                                                    let v = (expected[rr] >> rs) & mask;
                                                    let at = (address - 0x1000) as usize;
                                                    expected_data[at..at + size as usize]
                                                        .copy_from_slice(
                                                            &v.to_le_bytes()[..size as usize],
                                                        );
                                                } else {
                                                    let (dst, shift, source) = if mem {
                                                        (rr, rs, if byte { 0xa5 } else { 0xa5a5 })
                                                    } else if store {
                                                        (mr, ms, (expected[rr] >> rs) & mask)
                                                    } else {
                                                        (rr, rs, (expected[mr] >> ms) & mask)
                                                    };
                                                    expected[dst] = (expected[dst]
                                                        & !(mask << shift))
                                                        | (source << shift);
                                                }
                                            }
                                            run(&mut cpu);
                                            assert_eq!(
                                                cpu.registers, expected,
                                                "{byte}/{store}/{rex}/{reg}/{rm}/{mem}/{address:x}"
                                            );
                                            assert_eq!(cpu.flags, 0xa55a);
                                            assert_eq!(
                                                memory.read(0x1000, 16).unwrap(),
                                                expected_data
                                            );
                                            assert_eq!(
                                                (cpu.fault, cpu.fuel, cpu.stop),
                                                if valid {
                                                    (0, 0, 3)
                                                } else {
                                                    (if store { 2 } else { 1 }, 1, 0)
                                                }
                                            );
                                            checks += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for prefix in [0x64, 0x65, 0x67] {
            assert!(direct_cpu::translate(&[prefix, 0x8a, 0x00, 0xc3], 0x100, 2).is_err());
        }
        println!("direct narrow MOV: {checks} native state checks passed");
    }
}
