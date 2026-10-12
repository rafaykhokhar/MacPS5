//! x87 control/status transport only; no x87 register stack or arithmetic.
//! Primary: https://cdrdv2-public.intel.com/874240/325462-090-sdm-vol-1-2abcd-3abcd-4.pdf
//! Intel SDM Vol1 section8.1.5: reset037f, RC11:10, PC9:8; reserved PC01.
//! Vol2 FLDCW/FNSTCW/WAIT: FLDCW checks pending unmasked exceptions before
//! loading; newly unmasked flags wait until the next waiting instruction.
//! FNSTCW does not check pending exceptions. No guest exception delivery exists.
//! Reserved-bit patterns outside canonical reset values stop explicitly rather
//! than guessing readback: bit6=1, bits7/13:15=0, PC!=01. IC12 is retained.
use crate::legacy_cpu::Cpu;
pub const PENDING_FAULT: u64 = 18;
pub const UNSUPPORTED_CONTROL: u64 = 19;
pub fn pending(cpu: &Cpu) -> bool {
    cpu.x87_status & !cpu.x87_control & 0x3f != 0
}
pub fn wait(cpu: &mut Cpu) {
    if cpu.fault == 0 && pending(cpu) {
        cpu.fault = PENDING_FAULT;
    }
}
pub fn load(cpu: &mut Cpu, value: u16) {
    wait(cpu);
    if cpu.fault != 0 {
        return;
    }
    if value & 0xe0c0 != 0x0040 || value & 0x0300 == 0x0100 {
        cpu.fault = UNSUPPORTED_CONTROL;
        return;
    }
    cpu.x87_control = value;
}
#[allow(unsafe_code)]
mod abi {
    use super::*;
    /// # Safety
    /// cpu is the live uniquely borrowed matching Rust CPU context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_x87_wait_v1(cpu: *mut Cpu) {
        if let Some(cpu) = unsafe { cpu.as_mut() } {
            wait(cpu);
        }
    }
    /// # Safety
    /// cpu is the live uniquely borrowed matching Rust CPU context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_x87_load_control_v1(cpu: *mut Cpu, value: u64) {
        if let Some(cpu) = unsafe { cpu.as_mut() } {
            load(cpu, value as u16);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_rounding_reserved_and_delayed_exception() {
        let mut cpu = Cpu::default();
        assert_eq!((cpu.x87_control, cpu.x87_status), (0x037f, 0));
        for rounding in 0..4 {
            load(&mut cpu, 0x037f | (rounding << 10));
            assert_eq!(cpu.x87_control, 0x037f | (rounding << 10));
            crate::scalar_fp::load_mxcsr(&mut cpu, 0x1f80 | ((3 - rounding as u32) << 13));
            assert_eq!(cpu.x87_control, 0x037f | (rounding << 10));
        }
        cpu.x87_status = 1;
        load(&mut cpu, 0x037e); // previously masked sticky invalid; deferred trap
        assert_eq!(cpu.fault, 0);
        assert_eq!(cpu.x87_status, 1);
        wait(&mut cpu);
        assert_eq!(cpu.fault, PENDING_FAULT);
        for value in [0, 0x013f, 0xffff, 0x03ff] {
            let mut cpu = Cpu::default();
            load(&mut cpu, value);
            assert_eq!(cpu.fault, UNSUPPORTED_CONTROL);
            assert_eq!(cpu.x87_control, 0x037f);
        }
    }
}
