//! Explicit guest SIMD control; no dependency on host FPCR/FPSR or host TLS.
//! Intel SDM Vol2A (V)LDMXCSR: reset1f80; reserved bits cause #GP and do not
//! commit. Setting pending status while unmasking does not itself raise #XM.
//! https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2a-manual.pdf
//! Chosen guest feature policy supports DAZ, therefore valid bits are0..15.
use crate::legacy_cpu::Cpu;
mod arithmetic;
mod truncate;
pub use truncate::{TruncateError, TruncateResult, truncate_binary64_i32};
pub use arithmetic::{FpResult, FpUnsupported, multiply_binary64};
pub const MXCSR_RESET: u32 = 0x1f80;
pub const MXCSR_VALID: u32 = 0xffff;
/// Prototype diagnostic for invalid reserved MXCSR bits, representing #GP.
/// Not a claim that a complete guest exception delivery mechanism exists.
pub const CONTROL_FAULT: u64 = 14;
pub fn mxcsr(cpu: &Cpu) -> u32 {
    cpu.padding as u32
}
pub fn load_mxcsr(cpu: &mut Cpu, value: u32) {
    if cpu.fault != 0 {
        return;
    }
    if value & !MXCSR_VALID != 0 {
        cpu.fault = CONTROL_FAULT;
        return;
    }
    cpu.padding = value as u64;
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
        let before = cpu.padding;
        load_mxcsr(&mut cpu, 0x10000);
        assert_eq!(cpu.fault, CONTROL_FAULT);
        assert_eq!(cpu.padding, before);
        load_mxcsr(&mut cpu, 0);
        assert_eq!(cpu.padding, before);
    }
}
