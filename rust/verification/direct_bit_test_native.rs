use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn bytes(size: usize, op: u8, target: u8, index: u8, mem: bool, imm: Option<u8>) -> Vec<u8> {
    let mut b = Vec::new();
    if size == 2 {
        b.push(0x66);
    }
    b.push(
        0x40 | if size == 8 { 8 } else { 0 }
            | if imm.is_none() { (index >> 3) << 2 } else { 0 }
            | (target >> 3),
    );
    b.extend_from_slice(&[
        0x0f,
        if imm.is_some() {
            0xba
        } else {
            [0xa3, 0xab, 0xb3, 0xbb][op as usize]
        },
        (if mem { 0x40 } else { 0xc0 })
            | (if imm.is_some() { op + 4 } else { index & 7 } << 3)
            | (target & 7),
    ]);
    if mem {
        if target & 7 == 4 {
            b.push(0x24);
        }
        b.push(0);
    }
    if let Some(imm) = imm {
        b.push(imm);
    }
    b.push(0xc3);
    b
}
struct Case {
    size: usize,
    op: u8,
    target: u8,
    index: u8,
    mem: bool,
    imm: Option<u8>,
}
impl Case {
    fn bytes(&self) -> Vec<u8> {
        bytes(
            self.size,
            self.op,
            self.target,
            self.index,
            self.mem,
            self.imm,
        )
    }
    unsafe fn check(
        &self,
        run: unsafe extern "C" fn(*mut Cpu),
        value: u64,
        index: u64,
        writable: bool,
        base: u64,
    ) {
        let mut data = [0u8; 32];
        for chunk in data.chunks_mut(8) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        let mut expected_data = data;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, writable, false).unwrap();
        let mut cpu = Cpu {
            rip: 0x100,
            fuel: 2,
            flags: if value & 1 == 0 { 0xadff } else { 0xa002 },
            memory: (&mut memory as *mut Memory<'_>) as u64,
            registers: [0x123456789abcdef0; 16],
            ..Cpu::default()
        };
        cpu.registers[self.index as usize] = index;
        cpu.registers[self.target as usize] = if self.mem { base } else { value };
        let mut expected = cpu.registers;
        let idx = self.imm.map_or(expected[self.index as usize], u64::from);
        let bits = self.size * 8;
        let selected = idx as usize % bits;
        let signed = match self.size {
            2 => idx as i16 as i64,
            4 => idx as i32 as i64,
            _ => idx as i64,
        };
        // Independent floor division via byte offset and alignment within an operand.
        let byte_offset = if self.imm.is_some() {
            0
        } else {
            signed.div_euclid(8).div_euclid(self.size as i64) * self.size as i64
        };
        let address = base.wrapping_add(byte_offset as u64);
        let valid = !self.mem
            || (address >= 0x1000
                && address
                    .checked_add(self.size as u64)
                    .is_some_and(|end| end <= 0x1020));
        let fault = if !valid {
            1
        } else if self.mem && self.op != 0 && !writable {
            2
        } else {
            0
        };
        let mut flags = cpu.flags;
        if fault == 0 {
            let mut raw = if self.mem {
                let mut r = [0u8; 8];
                let at = (address - 0x1000) as usize;
                r[..self.size].copy_from_slice(&expected_data[at..at + self.size]);
                r
            } else {
                expected[self.target as usize].to_le_bytes()
            };
            let byte = selected / 8;
            let bit = 1u8 << (selected % 8);
            flags = (flags & !1) | u64::from(raw[byte] & bit != 0);
            match self.op {
                1 => raw[byte] |= bit,
                2 => raw[byte] &= !bit,
                3 => raw[byte] ^= bit,
                _ => {}
            }
            if self.op != 0 {
                if self.mem {
                    let at = (address - 0x1000) as usize;
                    expected_data[at..at + self.size].copy_from_slice(&raw[..self.size]);
                } else {
                    if self.size == 4 {
                        raw[4..].fill(0);
                    }
                    expected[self.target as usize] = u64::from_le_bytes(raw);
                }
            }
        }
        unsafe {
            run(&mut cpu);
        }
        assert_eq!(
            cpu.registers, expected,
            "registers size={} op={} target={} index={} mem={} imm={:?} value={value:x} idx={idx:x}",
            self.size, self.op, self.target, self.index, self.mem, self.imm
        );
        assert_eq!(cpu.flags, flags);
        assert_eq!(memory.read(0x1000, 32).unwrap(), expected_data);
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
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for size in [2, 4, 8] {
            for op in 0..4 {
                for target in 0..16 {
                    for index in 0..16 {
                        for mem in [false, true] {
                            for imm in [None, Some(0), Some(255)] {
                                let case = Case {
                                    size,
                                    op,
                                    target,
                                    index,
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
                                for value in [0, u64::MAX, 0x8000000080000000, 0x5555aaaaaaaa5555] {
                                    for idx in [
                                        0,
                                        1,
                                        15,
                                        16,
                                        31,
                                        32,
                                        63,
                                        64,
                                        255,
                                        u64::MAX,
                                        u64::MAX - 16,
                                        u64::MAX - 64,
                                        0x8000,
                                        0x80000000,
                                        0x8000000000000000,
                                    ] {
                                        if imm.is_some() && idx != 0 {
                                            continue;
                                        }
                                        for writable in [false, true] {
                                            if !mem && !writable {
                                                continue;
                                            }
                                            case.check(run, value, idx, writable, 0x1010);
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
        // Every encoded immediate, including ignored high index bits.
        for size in [2, 4, 8] {
            for op in 0..4 {
                for imm in 0..=255 {
                    let case = Case {
                        size,
                        op,
                        target: 0,
                        index: 1,
                        mem: false,
                        imm: Some(imm),
                    };
                    let code = arena
                        .publish(&direct_cpu::translate(&case.bytes(), 0x100, 2).unwrap())
                        .unwrap();
                    let run: unsafe extern "C" fn(*mut Cpu) =
                        std::mem::transmute(code.entry(0).unwrap());
                    for bit in 0..64 {
                        case.check(run, 1u64 << bit, 0, true, 0x1010);
                        checks += 1;
                    }
                }
            }
        }
        let mut locked = bytes(8, 1, 0, 1, true, None);
        locked.insert(0, 0xf0);
        assert!(direct_cpu::translate(&locked, 0x100, 2).is_err());
        println!("direct BT/BTS/BTR/BTC: {checks} native state checks passed; LOCK rejected");
    }
}
