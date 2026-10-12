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
    /// Former ABI padding at offset200: low32 now guest MXCSR, high32 zero.
    /// Retained name preserves existing fixture literals. Zero means actual
    /// MXCSR=0 (unmasked), never an implicit request for the reset default.
    pub padding: u64,
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
            padding: crate::scalar_fp::MXCSR_RESET as u64,
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
        assert_eq!(offset_of!(Cpu, padding), 200);
    }
}
