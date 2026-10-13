use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn product(a: u64, b: u64, bits: usize) -> u128 {
    let mut p = 0u128;
    for i in 0..bits / 16 {
        for j in 0..bits / 16 {
            let x = ((a >> (16 * i)) & 65535) as u32;
            let y = ((b >> (16 * j)) & 65535) as u32;
            p += u128::from(x * y) << (16 * (i + j));
        }
    }
    p
}
fn main() {
    unsafe {
        let arena = JitArena::new(32 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for hi in 0..16u8 {
                for lo in 0..16u8 {
                    for src in 0..16u8 {
                        for mem in [false, true] {
                            let mut bytes = vec![
                                0xc4,
                                0x42 | if hi < 8 { 0x80 } else { 0 }
                                    | if src < 8 { 0x20 } else { 0 },
                                ((!lo & 15) << 3) | 3 | if wide { 0x80 } else { 0 },
                                0xf6,
                                (if mem { 0x40 } else { 0xc0 }) | ((hi & 7) << 3) | (src & 7),
                            ];
                            if mem {
                                if src & 7 == 4 {
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
                            let values = [
                                0u64,
                                1,
                                0xffffffff,
                                0x100000000,
                                0x8000000000000000,
                                u64::MAX,
                            ];
                            for a in values {
                                for b in values {
                                    for address in [0x1000u64, 0x1001, 0x100d, u64::MAX - 7] {
                                        if !mem && address != 0x1000 {
                                            continue;
                                        }
                                        let mut data = [0u8; 16];
                                        data[..8].copy_from_slice(&b.to_le_bytes());
                                        data[8..].copy_from_slice(&b.to_le_bytes());
                                        let original = data;
                                        let mut memory = Memory::default();
                                        memory.add(0x1000, &mut data, false, false).unwrap();
                                        let mut cpu = Cpu {
                                            rip: 0x100,
                                            fuel: 2,
                                            flags: 0xadff,
                                            memory: (&mut memory as *mut Memory<'_>) as u64,
                                            registers: [0xabcdef1234567890; 16],
                                            ..Cpu::default()
                                        };
                                        cpu.registers[2] = a;
                                        cpu.registers[src as usize] = b;
                                        if mem {
                                            cpu.registers[src as usize] = address.wrapping_add(8);
                                        }
                                        let mut expected = cpu.registers;
                                        let size = if wide { 8 } else { 4 };
                                        let bits = size * 8;
                                        let mask = u64::MAX >> (64 - bits);
                                        let valid = !mem
                                            || (address >= 0x1000
                                                && address
                                                    .checked_add(size)
                                                    .is_some_and(|end| end <= 0x1010));
                                        if valid {
                                            let rhs = if mem {
                                                let at = (address - 0x1000) as usize;
                                                let mut raw = [0u8; 8];
                                                raw[..size as usize].copy_from_slice(
                                                    &original[at..at + size as usize],
                                                );
                                                u64::from_le_bytes(raw)
                                            } else {
                                                expected[src as usize]
                                            };
                                            let p = product(expected[2], rhs, bits as usize);
                                            expected[lo as usize] = p as u64 & mask;
                                            expected[hi as usize] = (p >> bits) as u64;
                                        }
                                        run(&mut cpu);
                                        assert_eq!(
                                            cpu.registers, expected,
                                            "{wide}/{hi}/{lo}/{src}/{mem}/{a:x}/{b:x}/{address:x}"
                                        );
                                        assert_eq!(cpu.flags, 0xadff);
                                        assert_eq!(memory.read(0x1000, 16).unwrap(), original);
                                        assert_eq!(
                                            (cpu.fault, cpu.fuel, cpu.stop),
                                            if valid { (0, 0, 3) } else { (1, 1, 0) }
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
        println!("direct MULX: {checks} native state checks passed");
    }
}
