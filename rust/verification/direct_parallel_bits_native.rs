use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};

fn encode(wide: bool, deposit: bool, dst: u8, src: u8, mask: u8, mem: bool) -> Vec<u8> {
    let mut b = vec![
        0xc4,
        0x42 | if dst < 8 { 0x80 } else { 0 } | if mask < 8 { 0x20 } else { 0 },
        ((!src & 15) << 3) | if wide { 0x80 } else { 0 } | if deposit { 3 } else { 2 },
        0xf5,
        (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (mask & 7),
    ];
    if mem {
        if mask & 7 == 4 {
            b.push(0x24);
        }
        b.push(0xf8);
    }
    b.push(0xc3);
    b
}
fn reference(value: u64, mask: u64, bits: usize, deposit: bool) -> u64 {
    let positions: Vec<usize> = (0..bits).filter(|n| mask & (1u64 << n) != 0).collect();
    positions
        .iter()
        .enumerate()
        .fold(0, |out, (packed, &selected)| {
            let (input, output) = if deposit {
                (packed, selected)
            } else {
                (selected, packed)
            };
            out | (((value >> input) & 1) << output)
        })
}
struct Case {
    wide: bool,
    deposit: bool,
    dst: u8,
    src: u8,
    mask: u8,
    mem: bool,
}
impl Case {
    fn bytes(&self) -> Vec<u8> {
        encode(
            self.wide,
            self.deposit,
            self.dst,
            self.src,
            self.mask,
            self.mem,
        )
    }
    unsafe fn check(
        &self,
        run: unsafe extern "C" fn(*mut Cpu),
        value: u64,
        mask: u64,
        address: u64,
    ) {
        let size = if self.wide { 8usize } else { 4 };
        let mut data = [0u8; 16];
        data[..8].copy_from_slice(&mask.to_le_bytes());
        data[8..].copy_from_slice(&mask.to_le_bytes());
        let original = data;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, false, false).unwrap();
        let mut cpu = Cpu {
            rip: 0x100,
            fuel: 2,
            flags: if value & 1 == 0 { 0xadff } else { 0xa002 },
            memory: (&mut memory as *mut Memory<'_>) as u64,
            registers: [0xdeadbeefabcdef01; 16],
            ..Cpu::default()
        };
        cpu.registers[self.src as usize] = value;
        cpu.registers[self.mask as usize] = if self.mem {
            address.wrapping_add(8)
        } else {
            mask
        };
        let mut expected = cpu.registers;
        let flags = cpu.flags;
        let valid = !self.mem
            || (address >= 0x1000
                && address
                    .checked_add(size as u64)
                    .is_some_and(|end| end <= 0x1010));
        if valid {
            let mask = if self.mem {
                let mut raw = [0u8; 8];
                let at = (address - 0x1000) as usize;
                raw[..size].copy_from_slice(&original[at..at + size]);
                u64::from_le_bytes(raw)
            } else {
                expected[self.mask as usize]
            };
            expected[self.dst as usize] =
                reference(expected[self.src as usize], mask, size * 8, self.deposit);
        }
        unsafe {
            run(&mut cpu);
        }
        assert_eq!(
            cpu.registers, expected,
            "wide={} deposit={} dst={} src={} mask={} mem={} value={value:x} mask={mask:x} addr={address:x}",
            self.wide, self.deposit, self.dst, self.src, self.mask, self.mem
        );
        assert_eq!(cpu.flags, flags);
        assert_eq!(
            (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
            if valid {
                (0, 3, 0, 0x100 + self.bytes().len() as u64 - 1)
            } else {
                (1, 0, 1, 0x100)
            }
        );
        assert_eq!(memory.read(0x1000, 16).unwrap(), original);
    }
}
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0u64;
        for wide in [false, true] {
            for deposit in [false, true] {
                for dst in 0..16 {
                    for src in 0..16 {
                        for mask in 0..16 {
                            for mem in [false, true] {
                                let case = Case {
                                    wide,
                                    deposit,
                                    dst,
                                    src,
                                    mask,
                                    mem,
                                };
                                let code = arena
                                    .publish(
                                        &direct_cpu::translate(&case.bytes(), 0x100, 2).unwrap(),
                                    )
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                for pattern in [
                                    0,
                                    1,
                                    u64::MAX,
                                    0xaaaaaaaaaaaaaaaa,
                                    0x5555555555555555,
                                    0x8000000000000000,
                                    0x80000000,
                                    0x123456789abcdef0,
                                ] {
                                    for value in [pattern, !pattern] {
                                        for address in [0x1000, 0x1001, 0x100d, u64::MAX] {
                                            if !mem && address != 0x1000 {
                                                continue;
                                            }
                                            case.check(run, value, pattern, address);
                                            checks += 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                let case = Case {
                    wide,
                    deposit,
                    dst: 0,
                    src: 1,
                    mask: 2,
                    mem: false,
                };
                let code = arena
                    .publish(&direct_cpu::translate(&case.bytes(), 0x100, 2).unwrap())
                    .unwrap();
                let run: unsafe extern "C" fn(*mut Cpu) =
                    std::mem::transmute(code.entry(0).unwrap());
                for mask in 0..256 {
                    for value in 0..256 {
                        case.check(run, value, mask, 0x1000);
                        checks += 1;
                    }
                }
                for bit in 0..64 {
                    for mask_bit in 0..64 {
                        case.check(run, 1u64 << bit, 1u64 << mask_bit, 0x1000);
                        checks += 1;
                    }
                }
            }
        }
        println!("direct PDEP/PEXT: {checks} native state checks passed");
    }
}
