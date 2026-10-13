//! Bit scans, counts and BMI1 lowest-bit operations with undefined-state preservation.
use crate::legacy_cpu::Cpu;

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
