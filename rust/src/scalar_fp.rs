//! Explicit guest SIMD control; no dependency on host FPCR/FPSR or host TLS.
//! Intel SDM Vol2A (V)LDMXCSR: reset1f80; reserved bits cause #GP and do not
//! commit. Setting pending status while unmasking does not itself raise #XM.
//! https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2a-manual.pdf
//! Chosen guest feature policy supports DAZ, therefore valid bits are0..15.
use crate::legacy_cpu::Cpu;
mod arithmetic;
mod compare;
pub use compare::{compare_binary64, native_compare_binary64};
mod convert_i64;
mod fma;
pub use convert_i64::abi::native_ps5_convert_i64_binary64_v1;
pub use convert_i64::convert_i64_binary64;
pub use fma::fma213_binary64;
mod truncate;
pub use arithmetic::abi::native_ps5_multiply_binary64_v1;
pub use arithmetic::{FpResult, FpUnsupported, multiply_binary64};
pub use truncate::abi::{native_ps5_truncate_i32_v1, native_ps5_truncate_i64_v1};
pub use truncate::{
    Truncate64Result, TruncateError, TruncateResult, truncate_binary64_i32, truncate_binary64_i64,
};
pub const MXCSR_RESET: u32 = 0x1f80;
pub const MXCSR_VALID: u32 = 0xffff;
pub use abi::native_ps5_load_mxcsr_v1;
/// Prototype diagnostic for invalid reserved MXCSR bits, representing #GP.
/// Not a claim that a complete guest exception delivery mechanism exists.
pub const CONTROL_FAULT: u64 = 14;
pub fn mxcsr(cpu: &Cpu) -> u32 {
    cpu.mxcsr
}
pub fn load_mxcsr(cpu: &mut Cpu, value: u32) {
    if cpu.fault != 0 {
        return;
    }
    if value & !MXCSR_VALID != 0 {
        cpu.fault = CONTROL_FAULT;
        return;
    }
    cpu.mxcsr = value;
}
#[allow(unsafe_code)]
mod abi {
    use super::*;
    /// # Safety
    /// cpu points to the live uniquely borrowed matching guest CPU context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_load_mxcsr_v1(cpu: *mut Cpu, value: u64) {
        if let Some(cpu) = unsafe { cpu.as_mut() } {
            load_mxcsr(cpu, value as u32);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guest_control_reset_zero_reserved_and_clone() {
        let mut cpu = Cpu::default();
        assert_eq!(mxcsr(&cpu), MXCSR_RESET);
        for value in [0, 1, 0x40, 0x8040, 0xffff, 0x1f80] {
            load_mxcsr(&mut cpu, value);
            assert_eq!(mxcsr(&cpu), value);
            assert_eq!(mxcsr(&cpu.clone()), value);
            assert_eq!(cpu.fault, 0);
        }
        let before = cpu.mxcsr;
        load_mxcsr(&mut cpu, 0x10000);
        assert_eq!(cpu.fault, CONTROL_FAULT);
        assert_eq!(cpu.mxcsr, before);
        load_mxcsr(&mut cpu, 0);
        assert_eq!(cpu.mxcsr, before);
    }
}
