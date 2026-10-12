//! Integer-only CVTTSD2SI signed-doubleword semantics; not instruction lowering.
//! Intel SDM vol.1 §§11.5.2.2,11.5.3.2–3 and vol.2 CVTTSD2SI:
//! https://cdrdv2-public.intel.com/819710/252046-sdm-change-document.pdf
//! Truncation ignores RC. Conversion raises invalid or precision, never denormal.
//! DAZ replaces subnormal inputs with zero; FTZ does not affect integer results.
use super::MXCSR_VALID;
const INVALID: u32 = 1;
const PRECISION: u32 = 32;
const INDEFINITE: u32 = 0x8000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TruncateResult {
    /// Architectural 32-bit destination bits; a GPR writer must zero-extend.
    pub bits: u32,
    pub mxcsr: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TruncateError {
    ReservedControl,
    /// No destination value is available. Caller must retain destination and
    /// instruction pointer, publish status, and handle the guest exception.
    UnmaskedException {
        status: u32,
        mxcsr: u32,
    },
}

pub fn truncate_binary64_i32(bits: u64, mxcsr: u32) -> Result<TruncateResult, TruncateError> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(TruncateError::ReservedControl);
    }
    let negative = bits >> 63 != 0;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (value, status) = if exponent == 0x7ff {
        (INDEFINITE, INVALID)
    } else if exponent == 0 {
        (
            0,
            if fraction != 0 && mxcsr & 0x40 == 0 {
                PRECISION
            } else {
                0
            },
        )
    } else {
        let power = exponent - 1023;
        if power < 0 {
            (0, PRECISION)
        } else if power > 31 {
            (INDEFINITE, INVALID)
        } else {
            let significand = (1u64 << 52) | fraction;
            let shift = (52 - power) as u32;
            let magnitude = significand >> shift;
            let limit = if negative {
                1u64 << 31
            } else {
                (1u64 << 31) - 1
            };
            if magnitude > limit {
                (INDEFINITE, INVALID)
            } else {
                let value = magnitude as u32;
                let value = if negative {
                    value.wrapping_neg()
                } else {
                    value
                };
                (
                    value,
                    if significand & ((1u64 << shift) - 1) != 0 {
                        PRECISION
                    } else {
                        0
                    },
                )
            }
        }
    };
    if status & !(mxcsr >> 7) != 0 {
        return Err(TruncateError::UnmaskedException {
            status,
            mxcsr: mxcsr | status,
        });
    }
    Ok(TruncateResult {
        bits: value,
        mxcsr: mxcsr | status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar_fp::MXCSR_RESET;
    #[test]
    fn truncate_limits_fraction_and_specials() {
        let cases = [
            (0, 0, 0),
            (1u64 << 63, 0, 0),
            (0x3ff8_0000_0000_0000, 1, PRECISION),
            (0xbff8_0000_0000_0000, u32::MAX, PRECISION),
            (0x41df_ffff_ffc0_0000, 0x7fff_ffff, 0),
            (0x41df_ffff_ffe0_0000, 0x7fff_ffff, PRECISION),
            (0x41e0_0000_0000_0000, INDEFINITE, INVALID),
            (0xc1e0_0000_0000_0000, INDEFINITE, 0),
            (0xc1e0_0000_0010_0000, INDEFINITE, PRECISION),
            (0xc1e0_0000_0020_0000, INDEFINITE, INVALID),
            (0x7ff0_0000_0000_0000, INDEFINITE, INVALID),
            (0xfff0_0000_0000_0000, INDEFINITE, INVALID),
            (0x7ff8_1234_5678_9abc, INDEFINITE, INVALID),
            (0x7ff0_0000_0000_0001, INDEFINITE, INVALID),
        ];
        for rc in 0..4 {
            for (input, bits, status) in cases {
                let control = MXCSR_RESET | (rc << 13) | 4;
                assert_eq!(
                    truncate_binary64_i32(input, control),
                    Ok(TruncateResult {
                        bits,
                        mxcsr: control | status
                    })
                );
            }
        }
    }
    #[test]
    fn truncate_daz_sticky_and_unmasked() {
        for input in [1, 0x000f_ffff_ffff_ffff, 0x8000_0000_0000_0001] {
            for ftz in [0, 0x8000] {
                let control = (MXCSR_RESET & !(1 << 8)) | ftz;
                assert_eq!(
                    truncate_binary64_i32(input, control),
                    Ok(TruncateResult {
                        bits: 0,
                        mxcsr: control | PRECISION
                    })
                );
                assert_eq!(
                    truncate_binary64_i32(input, control | 0x40),
                    Ok(TruncateResult {
                        bits: 0,
                        mxcsr: control | 0x40
                    })
                );
            }
        }
        for (input, status) in [
            (0x3fe0_0000_0000_0000, PRECISION),
            (0x7ff8_0000_0000_0000, INVALID),
        ] {
            let control = (MXCSR_RESET & !(status << 7)) | 8;
            assert_eq!(
                truncate_binary64_i32(input, control),
                Err(TruncateError::UnmaskedException {
                    status,
                    mxcsr: control | status
                })
            );
        }
        // Previously sticky flags do not retrigger on an exact conversion.
        assert_eq!(
            truncate_binary64_i32(0x3ff0_0000_0000_0000, 0x21),
            Ok(TruncateResult {
                bits: 1,
                mxcsr: 0x21
            })
        );
        assert_eq!(
            truncate_binary64_i32(0, 0x10000),
            Err(TruncateError::ReservedControl)
        );
    }
}

/// Prototype #XM diagnostic; full guest exception delivery remains unimplemented.
pub const TRUNCATE_SIMD_FAULT: u64 = 15;
#[allow(unsafe_code)]
mod abi {
    use super::*;
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// `cpu` points to the uniquely borrowed live matching CPU context. The
    /// caller must inspect fault before committing the returned GPR value.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_truncate_i32_v1(cpu: *mut Cpu, bits: u64) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match truncate_binary64_i32(bits, super::super::mxcsr(cpu)) {
            Ok(result) => {
                cpu.mxcsr = result.mxcsr;
                result.bits as u64
            }
            Err(TruncateError::ReservedControl) => {
                cpu.fault = super::super::CONTROL_FAULT;
                0
            }
            Err(TruncateError::UnmaskedException { mxcsr, .. }) => {
                cpu.mxcsr = mxcsr;
                cpu.fault = TRUNCATE_SIMD_FAULT;
                0
            }
        }
    }
}
