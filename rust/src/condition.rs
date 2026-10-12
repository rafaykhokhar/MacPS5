//! x86 condition evaluation shared by branches, CMOV and SETcc.
use crate::Result;
pub fn evaluate(flags: u64, condition: u64) -> Result<bool> {
    let cf = flags & 1 != 0;
    let pf = flags & 4 != 0;
    let zf = flags & 0x40 != 0;
    let sf = flags & 0x80 != 0;
    let of = flags & 0x800 != 0;
    let value = match condition / 2 {
        0 => of,
        1 => cf,
        2 => zf,
        3 => cf || zf,
        4 => sf,
        5 => pf,
        6 => sf != of,
        7 => zf || sf != of,
        _ => return Err("Invalid x86 condition code".into()),
    };
    Ok(value ^ (condition & 1 != 0))
}
#[allow(unsafe_code)]
pub mod ffi {
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// CPU is a live exclusive borrow for this synchronous call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_condition_v1(cpu: *mut Cpu, condition: u64) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match super::evaluate(cpu.flags, condition) {
            Ok(value) => u64::from(value),
            Err(_) => {
                cpu.fault = 4;
                0
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[allow(unsafe_code)]
    fn invalid_condition_faults_without_changing_cpu_state() {
        let mut cpu = crate::legacy_cpu::Cpu {
            flags: 0x8c7,
            fuel: 123,
            rip: 0x100,
            ..Default::default()
        };
        cpu.registers = [99; 16];
        assert_eq!(unsafe { ffi::native_ps5_condition_v1(&mut cpu, 0) }, 1);
        assert_eq!(cpu.fault, 0);
        assert_eq!(unsafe { ffi::native_ps5_condition_v1(&mut cpu, 16) }, 0);
        assert_eq!(cpu.fault, 4);
        cpu.fault = 9;
        assert_eq!(unsafe { ffi::native_ps5_condition_v1(&mut cpu, 0) }, 0);
        assert_eq!(cpu.fault, 9);
        assert_eq!((cpu.flags, cpu.fuel, cpu.rip), (0x8c7, 123, 0x100));
        assert_eq!(cpu.registers, [99; 16]);
    }
    #[test]
    fn all_flag_combinations_match_truth_tables() {
        let tables = [
            0xffff0000u32,
            0x0000ffff,
            0xaaaaaaaa,
            0x55555555,
            0xf0f0f0f0,
            0x0f0f0f0f,
            0xfafafafa,
            0x05050505,
            0xff00ff00,
            0x00ff00ff,
            0xcccccccc,
            0x33333333,
            0x00ffff00,
            0xff0000ff,
            0xf0fffff0,
            0x0f00000f,
        ];
        for index in 0..32 {
            let flags = [1, 4, 0x40, 0x80, 0x800]
                .into_iter()
                .enumerate()
                .fold(0, |flags, (bit, flag)| {
                    flags | if index & (1 << bit) != 0 { flag } else { 0 }
                });
            for (condition, table) in tables.into_iter().enumerate() {
                let expected = (table >> index) & 1 != 0;
                assert_eq!(evaluate(flags, condition as u64).unwrap(), expected);
                assert_eq!(
                    evaluate(flags | !0x8c5, condition as u64).unwrap(),
                    expected
                );
            }
        }
        for invalid in [16, 17, 255, u64::MAX] {
            assert!(evaluate(0, invalid).is_err());
        }
    }
}
