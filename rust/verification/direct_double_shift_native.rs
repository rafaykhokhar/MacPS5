use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
struct Case {
    size: usize,
    right: bool,
    dst: u8,
    src: u8,
    mem: bool,
    imm: Option<u8>,
}
impl Case {
    fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        if self.size == 2 {
            b.push(0x66);
        }
        b.push(
            0x40 | if self.size == 8 { 8 } else { 0 } | ((self.src >> 3) << 2) | (self.dst >> 3),
        );
        b.extend_from_slice(&[
            0x0f,
            0xa4 | if self.right { 8 } else { 0 } | u8::from(self.imm.is_none()),
            (if self.mem { 0x40 } else { 0xc0 }) | ((self.src & 7) << 3) | (self.dst & 7),
        ]);
        if self.mem {
            if self.dst & 7 == 4 {
                b.push(0x24);
            }
            b.push(0);
        }
        if let Some(n) = self.imm {
            b.push(n);
        }
        b.push(0xc3);
        b
    }
    unsafe fn check(
        &self,
        run: unsafe extern "C" fn(*mut Cpu),
        value: u64,
        source: u64,
        count: u64,
        address: u64,
        writable: bool,
    ) {
        let mut data = [0u8; 16];
        data[..8].copy_from_slice(&value.to_le_bytes());
        data[8..].copy_from_slice(&value.to_le_bytes());
        let mut expected_data = data;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, writable, false).unwrap();
        let mut cpu = Cpu {
            rip: 0x100,
            fuel: 2,
            flags: if count & 1 == 0 { 0xadff } else { 0xa002 },
            registers: [0x123456789abcdef0; 16],
            memory: (&mut memory as *mut Memory<'_>) as u64,
            ..Cpu::default()
        };
        cpu.registers[self.dst as usize] = if self.mem { address } else { value };
        cpu.registers[self.src as usize] = source;
        if self.imm.is_none() {
            cpu.registers[1] = count;
        }
        let mut expected = cpu.registers;
        let mut flags = cpu.flags;
        let base = expected[self.dst as usize];
        let valid = !self.mem
            || (base >= 0x1000
                && base
                    .checked_add(self.size as u64)
                    .is_some_and(|end| end <= 0x1010));
        let fault = if !valid {
            1
        } else if self.mem && !writable {
            2
        } else {
            0
        };
        if fault == 0 {
            let original = if self.mem {
                let mut raw = [0u8; 8];
                let at = (base - 0x1000) as usize;
                raw[..self.size].copy_from_slice(&expected_data[at..at + self.size]);
                u64::from_le_bytes(raw)
            } else {
                base
            };
            let count = self.imm.map_or(expected[1], u64::from);
            let (result, f) = reference(
                original,
                expected[self.src as usize],
                count,
                self.size * 8,
                self.right,
                flags,
            );
            flags = f;
            if self.mem {
                let at = (base - 0x1000) as usize;
                expected_data[at..at + self.size]
                    .copy_from_slice(&result.to_le_bytes()[..self.size]);
            } else {
                expected[self.dst as usize] = if self.size == 2 {
                    (original & !0xffff) | result
                } else {
                    result
                };
            }
        }
        unsafe {
            run(&mut cpu);
        }
        assert_eq!(
            cpu.registers, expected,
            "size={} right={} dst={} src={} mem={} imm={:?} count={count:x}",
            self.size, self.right, self.dst, self.src, self.mem, self.imm
        );
        assert_eq!(cpu.flags, flags);
        assert_eq!(memory.read(0x1000, 16).unwrap(), expected_data);
        assert_eq!(
            (cpu.fault, cpu.stop, cpu.fuel, cpu.rip),
            if fault == 0 {
                (0, 3, 0, 0x100 + self.bytes().len() as u64 - 1)
            } else {
                (fault, 0, 1, 0x100)
            }
        );
    }
}
fn reference(value: u64, source: u64, raw: u64, bits: usize, right: bool, old: u64) -> (u64, u64) {
    let count = raw as usize & if bits == 64 { 63 } else { 31 };
    let mut d: Vec<bool> = (0..bits).map(|bit| value & (1u64 << bit) != 0).collect();
    let s: Vec<bool> = (0..bits).map(|bit| source & (1u64 << bit) != 0).collect();
    let mut flags = old;
    let mut carry = false;
    if count > 0 && count <= bits {
        for step in 0..count {
            if right {
                carry = d[0];
                d.remove(0);
                d.push(s[step]);
            } else {
                carry = d[bits - 1];
                d.pop();
                d.insert(0, s[bits - 1 - step]);
            }
        }
        for (bit, on) in [
            (0, carry),
            (2, d[..8].iter().filter(|&&x| x).count() % 2 == 0),
            (6, !d.iter().any(|&x| x)),
            (7, d[bits - 1]),
        ] {
            if on {
                flags |= 1 << bit;
            } else {
                flags &= !(1 << bit);
            }
        }
        if count == 1 {
            if (value >> (bits - 1) & 1 != 0) != d[bits - 1] {
                flags |= 0x800;
            } else {
                flags &= !0x800;
            }
        }
    }
    let result = d
        .iter()
        .enumerate()
        .fold(0, |v, (n, &on)| v | ((on as u64) << n));
    (result, flags)
}
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2, 4, 8] {
            for right in [false, true] {
                for dst in 0..16 {
                    for src in 0..16 {
                        for mem in [false, true] {
                            for imm in [None, Some(0), Some(1), Some(16), Some(255)] {
                                let case = Case {
                                    size,
                                    right,
                                    dst,
                                    src,
                                    mem,
                                    imm,
                                };
                                let code = arena
                                    .publish(
                                        &direct_cpu::translate(&case.bytes(), 0x100, 2).unwrap(),
                                    )
                                    .unwrap();
                                let run: unsafe extern "C" fn(*mut Cpu) =
                                    std::mem::transmute(code.entry(0).unwrap());
                                for count in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 255] {
                                    if imm.is_some() && count != 0 {
                                        continue;
                                    }
                                    for value in
                                        [0, u64::MAX, 0x8000000080000000, 0x5555aaaa5555aaaa]
                                    {
                                        for address in [0x1000, 0x1001, 0x100f, u64::MAX] {
                                            if !mem && address != 0x1000 {
                                                continue;
                                            }
                                            for writable in [false, true] {
                                                if !mem && !writable {
                                                    continue;
                                                }
                                                case.check(
                                                    run, value, !value, count, address, writable,
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
                for imm in 0..=255 {
                    let case = Case {
                        size,
                        right,
                        dst: 0,
                        src: 2,
                        mem: false,
                        imm: Some(imm),
                    };
                    let code = arena
                        .publish(&direct_cpu::translate(&case.bytes(), 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for bit in 0..64 {
                        case.check(run, 1u64 << bit, !(1u64 << bit), 0, 0x1000, true);
                        checks += 1;
                    }
                }
            }
        }
        let mut locked = Case {
            size: 8,
            right: false,
            dst: 0,
            src: 2,
            mem: true,
            imm: Some(1),
        }
        .bytes();
        locked.insert(0, 0xf0);
        assert!(direct_cpu::translate(&locked, 0x100, 2).is_err());
        println!("direct SHLD/SHRD: {checks} native state checks passed; LOCK rejected");
    }
}
