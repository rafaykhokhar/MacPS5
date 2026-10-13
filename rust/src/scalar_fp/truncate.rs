//! Integer-only CVTTSD2SI signed 32/64-bit semantics; not instruction lowering.
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
pub struct Truncate64Result {
    pub bits: u64,
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

pub fn truncate_binary64_i64(bits: u64, mxcsr: u32) -> Result<Truncate64Result, TruncateError> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(TruncateError::ReservedControl);
    }
    let indefinite = 1u64 << 63;
    let negative = bits >> 63 != 0;
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let (value, status) = if exponent == 0x7ff {
        (indefinite, INVALID)
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
        } else if power > 63 {
            (indefinite, INVALID)
        } else {
            let significand = (1u64 << 52) | fraction;
            // The exponent guard bounds left shifts to 11; the largest
            // 53-bit significand therefore still fits an unsigned 64-bit word.
            let (magnitude, discarded) = if power >= 52 {
                (significand << (power - 52), false)
            } else {
                let shift = (52 - power) as u32;
                (
                    significand >> shift,
                    significand & ((1u64 << shift) - 1) != 0,
                )
            };
            let limit = if negative { indefinite } else { indefinite - 1 };
            if magnitude > limit {
                (indefinite, INVALID)
            } else {
                (
                    if negative {
                        magnitude.wrapping_neg()
                    } else {
                        magnitude
                    },
                    if discarded { PRECISION } else { 0 },
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
    Ok(Truncate64Result {
        bits: value,
        mxcsr: mxcsr | status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar_fp::MXCSR_RESET;
    #[test]
    fn truncate64_boundaries_and_every_exponent() {
        let indefinite = 1u64 << 63;
        for (input, bits, status) in [
            (0x43df_ffff_ffff_ffff, 0x7fff_ffff_ffff_fc00, 0),
            (0x43e0_0000_0000_0000, indefinite, INVALID),
            (0xc3df_ffff_ffff_ffff, 0x8000_0000_0000_0400, 0),
            (0xc3e0_0000_0000_0000, indefinite, 0),
            (0xc3e0_0000_0000_0001, indefinite, INVALID),
            (0x3fff_ffff_ffff_ffff, 1, PRECISION),
            (0xbfff_ffff_ffff_ffff, u64::MAX, PRECISION),
            (0x7ff0_0000_0000_0001, indefinite, INVALID),
            (0xfff8_0000_0000_0001, indefinite, INVALID),
        ] {
            for rc in 0..4 {
                let control = MXCSR_RESET | (rc << 13);
                assert_eq!(
                    truncate_binary64_i64(input, control),
                    Ok(Truncate64Result {
                        bits,
                        mxcsr: control | status
                    })
                );
            }
        }
        // An independent u128 fixed-point oracle exercises all binary64
        // exponents, both signs, and significand extremes around every shift.
        for exponent in 0..=2047u64 {
            for fraction in [0, 1, 0x7_ffff_ffff_ffff, 0xf_ffff_ffff_ffff] {
                for negative in [false, true] {
                    let input = (u64::from(negative) << 63) | (exponent << 52) | fraction;
                    let (bits, status) = if exponent == 2047 || exponent > 1086 {
                        (indefinite, INVALID)
                    } else if exponent < 1023 {
                        (
                            0,
                            if exponent == 0 && fraction == 0 {
                                0
                            } else {
                                PRECISION
                            },
                        )
                    } else {
                        let numerator = (u128::from(fraction) + (1u128 << 52)) << (exponent - 1023);
                        let magnitude = numerator / (1u128 << 52);
                        let limit = if negative {
                            1u128 << 63
                        } else {
                            (1u128 << 63) - 1
                        };
                        if magnitude > limit {
                            (indefinite, INVALID)
                        } else {
                            let signed = if negative {
                                -(magnitude as i128)
                            } else {
                                magnitude as i128
                            };
                            (
                                signed as u64,
                                if numerator % (1u128 << 52) != 0 {
                                    PRECISION
                                } else {
                                    0
                                },
                            )
                        }
                    };
                    assert_eq!(
                        truncate_binary64_i64(input, MXCSR_RESET),
                        Ok(Truncate64Result {
                            bits,
                            mxcsr: MXCSR_RESET | status
                        }),
                        "input {input:016x}"
                    );
                }
            }
        }
    }
    #[test]
    fn truncate64_masks_daz_ftz_sticky_and_reserved() {
        for masks in 0..64u32 {
            for rc in 0..4 {
                for ftz in [0, 0x8000] {
                    for daz in [0, 0x40] {
                        let control = (masks << 7) | (rc << 13) | ftz | daz | 0x1e;
                        for (input, bits, status) in [
                            (1, 0, if daz == 0 { PRECISION } else { 0 }),
                            (
                                0x800f_ffff_ffff_ffff,
                                0,
                                if daz == 0 { PRECISION } else { 0 },
                            ),
                            (0x3ff8_0000_0000_0000, 1, PRECISION),
                            (0x43e0_0000_0000_0000, 1u64 << 63, INVALID),
                            (0x7ff8_0000_0000_0000, 1u64 << 63, INVALID),
                            (0x3ff0_0000_0000_0000, 1, 0),
                        ] {
                            let expected = if status & !masks != 0 {
                                Err(TruncateError::UnmaskedException {
                                    status,
                                    mxcsr: control | status,
                                })
                            } else {
                                Ok(Truncate64Result {
                                    bits,
                                    mxcsr: control | status,
                                })
                            };
                            assert_eq!(truncate_binary64_i64(input, control), expected);
                        }
                    }
                }
            }
        }
        assert_eq!(
            truncate_binary64_i64(0x3ff0_0000_0000_0000, 0x21),
            Ok(Truncate64Result {
                bits: 1,
                mxcsr: 0x21
            })
        );
        for bit in 16..32 {
            assert_eq!(
                truncate_binary64_i64(0, 1 << bit),
                Err(TruncateError::ReservedControl)
            );
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn truncate64_ffi_faults_and_status() {
        use crate::legacy_cpu::Cpu;
        unsafe { assert_eq!(abi::native_ps5_truncate_i64_v1(std::ptr::null_mut(), 0), 0) };
        let mut cpu = Cpu {
            mxcsr: MXCSR_RESET,
            ..Cpu::default()
        };
        unsafe {
            assert_eq!(
                abi::native_ps5_truncate_i64_v1(&mut cpu, 0xc3e0_0000_0000_0000),
                1u64 << 63
            )
        };
        assert_eq!(cpu.fault, 0);
        cpu.mxcsr = MXCSR_RESET & !(PRECISION << 7);
        unsafe {
            assert_eq!(
                abi::native_ps5_truncate_i64_v1(&mut cpu, 0x3ff8_0000_0000_0000),
                0
            )
        };
        assert_eq!(cpu.fault, TRUNCATE_SIMD_FAULT);
        assert_eq!(cpu.mxcsr, (MXCSR_RESET & !(PRECISION << 7)) | PRECISION);
        let prior = cpu.mxcsr;
        unsafe {
            assert_eq!(
                abi::native_ps5_truncate_i64_v1(&mut cpu, 0x7ff0_0000_0000_0000),
                0
            )
        };
        assert_eq!(cpu.mxcsr, prior);
        assert_eq!(cpu.fault, TRUNCATE_SIMD_FAULT);
        cpu.fault = 0;
        cpu.mxcsr = 0x10000;
        unsafe { assert_eq!(abi::native_ps5_truncate_i64_v1(&mut cpu, 0), 0) };
        assert_eq!(cpu.fault, super::super::CONTROL_FAULT);
        assert_eq!(cpu.mxcsr, 0x10000);
    }
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
    /// Same context and fault-before-result-commit contract as the i32 helper.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_truncate_i64_v1(cpu: *mut Cpu, bits: u64) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match truncate_binary64_i64(bits, super::super::mxcsr(cpu)) {
            Ok(result) => {
                cpu.mxcsr = result.mxcsr;
                result.bits
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
