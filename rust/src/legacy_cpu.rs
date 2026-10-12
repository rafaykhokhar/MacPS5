//! Temporary layout shared with the retained C++ guest CPU. No pointer ownership.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct Cpu {
    pub registers: [u64; 16],
    pub rip: u64,
    pub flags: u64,
    pub bias: u64,
    pub fault: u64,
    pub fuel: u64,
    pub stop: u64,
    pub target: u64,
    pub memory: u64,
    pub next_rip: u64,
    /// Independent guest SIMD and x87 environments, preserving the720-byte ABI.
    pub mxcsr: u32,
    pub x87_control: u16,
    pub x87_status: u16,
    pub vectors: [[u8; 32]; 16],
}
impl Default for Cpu {
    fn default() -> Self {
        Self {
            registers: [0; 16],
            rip: 0,
            flags: 2,
            bias: 0,
            fault: 0,
            fuel: 0,
            stop: 0,
            target: 0,
            memory: 0,
            next_rip: 0,
            mxcsr: crate::scalar_fp::MXCSR_RESET,
            x87_control: 0x037f,
            x87_status: 0,
            vectors: [[0; 32]; 16],
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};
    #[test]
    fn legacy_layout() {
        assert_eq!(size_of::<Cpu>(), 720);
        assert_eq!(align_of::<Cpu>(), 16);
        assert_eq!(offset_of!(Cpu, flags), 136);
        assert_eq!(offset_of!(Cpu, fault), 152);
        assert_eq!(offset_of!(Cpu, memory), 184);
        assert_eq!(offset_of!(Cpu, vectors), 208);
        assert_eq!(offset_of!(Cpu, mxcsr), 200);
        assert_eq!(offset_of!(Cpu, x87_control), 204);
        assert_eq!(offset_of!(Cpu, x87_status), 206);
    }
}
