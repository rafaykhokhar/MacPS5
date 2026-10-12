//! Guarded input/control planning for the supported FreeBSD strtod subset.
//! No host floating point or guest memory writes occur while preparing a call.
use crate::{Result, float_parse::Rounding, guest_memory::Memory, legacy_cpu::Cpu};
pub fn rounding(cpu: &Cpu) -> Result<Rounding> {
    // Initial callable domain deliberately excludes unmasked traps, DAZ/FTZ,
    // reserved control words and inconsistent SSE/x87 rounding environments.
    if cpu.mxcsr & !0xffff != 0
        || cpu.mxcsr & 0x1f80 != 0x1f80
        || cpu.mxcsr & 0x8040 != 0
        || cpu.x87_control & 0xe0c0 != 0x0040
        || cpu.x87_control & 0x0300 == 0x0100
        || cpu.x87_control & 0x003f != 0x003f
        || (cpu.mxcsr >> 13) & 3 != u32::from((cpu.x87_control >> 10) & 3)
    {
        return Err("Unsupported float conversion control environment".into());
    }
    Ok(match (cpu.x87_control >> 10) & 3 {
        0 => Rounding::NearestEven,
        1 => Rounding::Down,
        2 => Rounding::Up,
        _ => Rounding::TowardZero,
    })
}
pub fn input(memory: &Memory<'_>, address: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for index in 0..65_536 {
        let value = memory.load(
            address
                .checked_add(index)
                .ok_or("Float input address overflow")?,
            1,
        )? as u8;
        bytes.push(value);
        if value == 0 {
            return Ok(bytes);
        }
    }
    Err("Float conversion requires a NUL-terminated input within 64KiB".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_rejections_are_independent() {
        let reset = Cpu::default();
        for precision in [0u16, 0x0200, 0x0300] {
            for (mode, expected) in [
                (0, Rounding::NearestEven),
                (1, Rounding::Down),
                (2, Rounding::Up),
                (3, Rounding::TowardZero),
            ] {
                let cpu = Cpu {
                    mxcsr: 0x1fbf | (mode << 13),
                    x87_control: 0x107f | precision | ((mode as u16) << 10),
                    x87_status: 0x003f,
                    ..Cpu::default()
                };
                assert_eq!(rounding(&cpu).unwrap(), expected);
            }
        }
        for control in [
            reset.mxcsr | 0x40,
            reset.mxcsr | 0x8000,
            reset.mxcsr | 0x10000,
            reset.mxcsr ^ 0x80,
            reset.mxcsr | 0x2000,
        ] {
            let cpu = Cpu {
                mxcsr: control,
                ..Cpu::default()
            };
            assert!(rounding(&cpu).is_err(), "MXCSR {control:x}");
        }
        for control in [0x003f, 0x017f, 0x037e, 0x03ff, 0x237f, 0x077f] {
            let cpu = Cpu {
                x87_control: control,
                ..Cpu::default()
            };
            assert!(rounding(&cpu).is_err(), "x87 CW {control:x}");
        }
    }
    #[test]
    fn input_scan_stops_at_nul_and_enforces_exact_budget() {
        let mut bytes = vec![b'1'; 65_536];
        bytes[65_535] = 0;
        let mut memory = Memory::default();
        memory.add(0x1_0000_0000, &mut bytes, false, false).unwrap();
        assert_eq!(input(&memory, 0x1_0000_0000).unwrap().len(), 65_536);
        assert_eq!(input(&memory, 0x1_0000_ffff).unwrap(), [0]);
        drop(memory);
        bytes[65_535] = b'1';
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, false, false).unwrap();
        assert!(input(&memory, 0x1000).is_err());
        assert!(input(&memory, 0x10fff).is_err());
    }
    #[test]
    fn independent_controls_and_guarded_input() {
        let mut cpu = Cpu::default();
        for mode in 0..4 {
            cpu.mxcsr = 0x1f80 | (mode << 13);
            cpu.x87_control = 0x037f | ((mode as u16) << 10);
            assert!(rounding(&cpu).is_ok());
        }
        for control in [0x1fc0, 0x9f80, 0x0f80, 0x10000, 0x1f80] {
            cpu.mxcsr = control;
            assert!(rounding(&cpu).is_err());
        }
        let mut data = *b"12\0";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, false, false).unwrap();
        assert_eq!(input(&memory, 0x1000).unwrap(), b"12\0");
        assert!(input(&memory, u64::MAX).is_err());
    }
}
