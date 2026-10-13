use native_ps5::{direct_cpu, guest_memory::Memory, jit_memory::JitArena, legacy_cpu::Cpu};
fn main() {
    unsafe {
        let arena = JitArena::new(64 * 1024 * 1024).unwrap();
        let mut checks = 0;
        for wide in [false, true] {
            for dst in 0..16u8 {
                for src in 0..16u8 {
                    for mem in [false, true] {
                        for count in 0..256u16 {
                            // Exhaust all counts for one register pair; boundary counts for all aliases.
                            if (dst != 0 || src != 1)
                                && ![0, 1, 31, 32, 63, 64, 65, 255].contains(&count)
                            {
                                continue;
                            }
                            let mut bytes = vec![
                                0xc4,
                                0x43 | if dst < 8 { 0x80 } else { 0 }
                                    | if src < 8 { 0x20 } else { 0 },
                                0x7b | if wide { 0x80 } else { 0 },
                                0xf0,
                                (if mem { 0x40 } else { 0xc0 }) | ((dst & 7) << 3) | (src & 7),
                            ];
                            if mem {
                                if src & 7 == 4 {
                                    bytes.push(0x24);
                                }
                                bytes.push(0xf8);
                            }
                            bytes.push(count as u8);
                            bytes.push(0xc3);
                            let code = arena
                                .publish(&direct_cpu::translate(&bytes, 0x100, 2).unwrap())
                                .unwrap();
                            let run: unsafe extern "C" fn(*mut Cpu) =
                                std::mem::transmute(code.entry(0).unwrap());
                            for value in [
                                0u64,
                                1,
                                u64::MAX,
                                0x80000000,
                                0x8000000000000000,
                                0x123456789abcdef0,
                            ] {
                                for address in [0x1000u64, 0x1001, 0x100d, u64::MAX - 7] {
                                    if !mem && address != 0x1000 {
                                        continue;
                                    }
                                    let mut data = [0u8; 16];
                                    data[..8].copy_from_slice(&value.to_le_bytes());
                                    data[8..].copy_from_slice(&value.to_le_bytes());
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
                                    cpu.registers[src as usize] = value;
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
                                        let mut result = if mem {
                                            let at = (address - 0x1000) as usize;
                                            let mut raw = [0u8; 8];
                                            raw[..size as usize]
                                                .copy_from_slice(&original[at..at + size as usize]);
                                            u64::from_le_bytes(raw)
                                        } else {
                                            expected[src as usize]
                                        } & mask;
                                        for _ in 0..u64::from(count) % bits {
                                            result =
                                                result / 2 + (result % 2) * (1u64 << (bits - 1));
                                        }
                                        expected[dst as usize] = result;
                                    }
                                    run(&mut cpu);
                                    assert_eq!(
                                        cpu.registers, expected,
                                        "{wide}/{dst}/{src}/{mem}/{count}/{address:x}"
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
        println!("direct RORX: {checks} native state checks passed");
    }
}
