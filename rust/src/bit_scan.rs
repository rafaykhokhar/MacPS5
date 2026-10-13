//! Bit scans, counts and BMI1 lowest-bit operations with undefined-state preservation.
use crate::legacy_cpu::Cpu;

/// PDEP (deposit != 0) or PEXT (deposit == 0). Callers truncate both inputs
/// for dword operations. Iteration is bounded by the mask's 64 set bits.
pub extern "C" fn parallel_bits(mut mask: u64, source: u64, deposit: u64) -> u64 {
    let mut packed = 1u64;
    let mut result = 0;
    while mask != 0 {
        let selected = mask & mask.wrapping_neg();
        let (input, output) = if deposit != 0 {
            (packed, selected)
        } else {
            (selected, packed)
        };
        if source & input != 0 {
            result |= output;
        }
        mask &= mask - 1;
        packed = packed.wrapping_shl(1);
    }
    result
}

/// Operations 0=BSF, 1=BSR, 2=LZCNT, 3=TZCNT, 4=POPCNT, 5=BLSI, 6=BLSMSK, 7=BLSR.
pub fn execute(cpu: &mut Cpu, value: u64, destination: u64, size: u64, operation: u64) {
    if cpu.fault != 0 {
        return;
    }
    if destination >= 16
        || ![2, 4, 8].contains(&size)
        || operation > 7
        || (operation >= 5 && size == 2)
    {
        cpu.fault = 3;
        return;
    }
    let bits = (size * 8) as u32;
    let mask = u64::MAX >> (64 - bits);
    let value = value & mask;
    if operation >= 5 {
        let result = match operation {
            5 => value & value.wrapping_neg(),
            6 => value ^ value.wrapping_sub(1),
            _ => value & value.wrapping_sub(1),
        } & mask;
        let carry = if operation == 5 {
            value != 0
        } else {
            value == 0
        };
        let zero = operation != 6 && result == 0;
        cpu.flags = (cpu.flags & !0x8c1)
            | u64::from(carry)
            | (u64::from(zero) << 6)
            | (((result >> (bits - 1)) & 1) << 7);
        cpu.registers[destination as usize] = result;
        return;
    }
    if operation <= 1 && value == 0 {
        cpu.flags |= 64;
        return; // Architecturally undefined destination: preserve it deterministically.
    }
    let result = match operation {
        0 => value.trailing_zeros(),
        1 => 63 - value.leading_zeros(),
        2 => value.leading_zeros() - (64 - bits),
        3 => value.trailing_zeros().min(bits),
        _ => value.count_ones(),
    } as u64;
    cpu.flags = match operation {
        0 | 1 => cpu.flags & !64,
        2 | 3 => (cpu.flags & !65) | u64::from(value == 0) | (u64::from(result == 0) << 6),
        _ => (cpu.flags & !0x8d5) | (u64::from(value == 0) << 6),
    };
    let dst = &mut cpu.registers[destination as usize];
    *dst = if size == 2 {
        (*dst & !mask) | result
    } else {
        result
    };
}

#[allow(unsafe_code)]
pub mod ffi {
    /// BT/BTS/BTR/BTC (operation 0..3). Mode 0 is a register slot, 1 is
    /// memory with an immediate index, and 2 is memory with a signed register index.
    /// # Safety
    /// CPU and its Memory must be live, exclusive and disjoint for this call.
    pub unsafe extern "C" fn bit_test(
        cpu: *mut super::Cpu,
        mut target: u64,
        index: u64,
        size: u64,
        operation: u64,
        mode: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if ![2, 4, 8].contains(&size) || operation > 3 || mode > 2 || (mode == 0 && target >= 16) {
            cpu.fault = 3;
            return;
        }
        let bits = size * 8;
        if mode == 2 {
            let signed = match size {
                2 => index as i16 as i64,
                4 => index as i32 as i64,
                _ => index as i64,
            };
            target = target.wrapping_add((signed.div_euclid(bits as i64) * size as i64) as u64);
        }
        let selected = 1u64 << (index & (bits - 1));
        let value = if mode == 0 {
            cpu.registers[target as usize]
        } else {
            if cpu.memory == 0 {
                cpu.fault = 1;
                return;
            }
            match unsafe { &*(cpu.memory as *const crate::guest_memory::Memory<'_>) }
                .load(target, size)
            {
                Ok(v) => v,
                Err(_) => {
                    cpu.fault = 1;
                    return;
                }
            }
        };
        if operation != 0 {
            let result = match operation {
                1 => value | selected,
                2 => value & !selected,
                _ => value ^ selected,
            };
            if mode == 0 {
                cpu.registers[target as usize] = if size == 4 {
                    result as u32 as u64
                } else {
                    result
                };
            } else if unsafe { &mut *(cpu.memory as *mut crate::guest_memory::Memory<'_>) }
                .store(target, result, size)
                .is_err()
            {
                cpu.fault = 2;
                return;
            }
        }
        cpu.flags = (cpu.flags & !1) | u64::from(value & selected != 0);
    }

    /// # Safety
    /// CPU must be live and exclusively owned for this synchronous call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_bit_scan_v1(
        cpu: *mut super::Cpu,
        value: u64,
        destination: u64,
        size: u64,
        operation: u64,
    ) {
        if let Some(cpu) = unsafe { cpu.as_mut() } {
            super::execute(cpu, value, destination, size, operation);
        }
    }
}
