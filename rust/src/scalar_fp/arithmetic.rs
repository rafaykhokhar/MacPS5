//! Integer-only finite binary64 multiplication. This is not translator support.
//! MXCSR layout/rounding: Intel SDM vol.1 §10.2.3 and AMD APM vol.4
//! https://www.amd.com/content/dam/amd/en/documents/processor-tech-docs/programmer-references/26568.pdf
//! NaN/infinity priority, unmasked exception delivery and ambiguous tiny-result
//! policy deliberately remain explicit unsupported outcomes, with no state commit.
use super::MXCSR_VALID;
const SIGN: u64 = 1 << 63;
const FRACTION: u64 = (1 << 52) - 1;
const INFINITY: u64 = 0x7ff0_0000_0000_0000;
const DENORMAL: u32 = 2;
const OVERFLOW: u32 = 8;
const PRECISION: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpResult {
    pub bits: u64,
    pub mxcsr: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpUnsupported {
    ReservedControl,
    NonFinite,
    TinyInexact,
    TinyUnmasked,
    FlushTiny,
    UnmaskedException(u32),
}

/// Pure calculation: callers must perform guest exception/commit handling.
/// Existing sticky flags never cause a new exception by themselves.
pub fn multiply_binary64(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, FpUnsupported> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(FpUnsupported::ReservedControl);
    }
    if a & INFINITY == INFINITY || b & INFINITY == INFINITY {
        return Err(FpUnsupported::NonFinite);
    }
    let sign = (a ^ b) & SIGN;
    let mut status = 0;
    let (a, ae) = unpack(a, mxcsr, &mut status);
    let (b, be) = unpack(b, mxcsr, &mut status);
    let (magnitude, rounding_status) = if a == 0 || b == 0 {
        (0, 0)
    } else {
        round_pack(a as u128 * b as u128, ae + be, sign != 0, mxcsr)?
    };
    status |= rounding_status;
    let unmasked = status & !(mxcsr >> 7) & 0x3f;
    if unmasked != 0 {
        return Err(FpUnsupported::UnmaskedException(unmasked));
    }
    Ok(FpResult {
        bits: sign | magnitude,
        mxcsr: mxcsr | status,
    })
}

fn unpack(bits: u64, control: u32, status: &mut u32) -> (u64, i32) {
    let exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & FRACTION;
    if exponent == 0 {
        if fraction != 0 && control & 0x40 == 0 {
            *status |= DENORMAL;
            (fraction, -1074)
        } else {
            (0, -1074)
        }
    } else {
        ((1 << 52) | fraction, exponent - 1023 - 52)
    }
}

/// Round a positive exact integer times 2^scale once. Product is at most106bits.
/// Kept separate so subsequent exact arithmetic can share packing rules.
fn round_pack(
    value: u128,
    scale: i32,
    negative: bool,
    control: u32,
) -> Result<(u64, u32), FpUnsupported> {
    debug_assert!(value != 0);
    let top = 127 - value.leading_zeros() as i32;
    let exponent = top + scale;
    let mode = (control >> 13) & 3;
    // Quantize at the normal53-bit precision, or minimum subnormal quantum.
    let quantum = (exponent - 52).max(-1074);
    let shift = quantum - scale;
    let (mut rounded, inexact) = round_integer(value, shift, negative, mode);
    if exponent < -1022 {
        // Tininess-before/after-rounding and FTZ exact-tiny behavior still need
        // AMD-specific evidence; do not publish guessed exception flags.
        if control & (1 << 11) == 0 {
            return Err(FpUnsupported::TinyUnmasked);
        }
        if inexact {
            return Err(FpUnsupported::TinyInexact);
        }
        if control & 0x8000 != 0 {
            return Err(FpUnsupported::FlushTiny);
        }
        return Ok((rounded as u64, 0));
    }
    let mut output_exponent = exponent;
    if rounded >= 1u128 << 53 {
        rounded >>= 1;
        output_exponent += 1;
    }
    if output_exponent > 1023 {
        let infinity = mode == 0 || (mode == 1 && negative) || (mode == 2 && !negative);
        return Ok((
            if infinity { INFINITY } else { INFINITY - 1 },
            OVERFLOW | PRECISION,
        ));
    }
    Ok((
        ((output_exponent + 1023) as u64) << 52 | (rounded as u64 & FRACTION),
        if inexact { PRECISION } else { 0 },
    ))
}

fn round_integer(value: u128, shift: i32, negative: bool, mode: u32) -> (u128, bool) {
    if shift <= 0 {
        return (value << (-shift as u32), false);
    }
    let (integer, remainder, half_order) = if shift < 128 {
        let remainder = value & ((1u128 << shift) - 1);
        (
            value >> shift,
            remainder,
            remainder.cmp(&(1u128 << (shift - 1))),
        )
    } else if shift == 128 {
        (0, value, value.cmp(&(1u128 << 127)))
    } else {
        (0, value, std::cmp::Ordering::Less)
    };
    let inexact = remainder != 0;
    let increment = inexact
        && match mode {
            0 => half_order.is_gt() || (half_order.is_eq() && integer & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    (integer + u128::from(increment), inexact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalar_fp::MXCSR_RESET;
    #[test]
    fn multiply_exact_sign_daz_and_denormal() {
        for sign in [0, SIGN] {
            assert_eq!(
                multiply_binary64(
                    0x3ff8_0000_0000_0000 | sign,
                    0x4000_0000_0000_0000,
                    MXCSR_RESET
                )
                .unwrap()
                .bits,
                0x4008_0000_0000_0000 | sign
            );
            assert_eq!(
                multiply_binary64(sign, 0x4000_0000_0000_0000, MXCSR_RESET)
                    .unwrap()
                    .bits,
                sign
            );
        }
        assert_eq!(
            multiply_binary64(1, 0x3ff0_0000_0000_0000, MXCSR_RESET).unwrap(),
            FpResult {
                bits: 1,
                mxcsr: MXCSR_RESET | DENORMAL
            }
        );
        assert_eq!(
            multiply_binary64(SIGN | 1, 0x3ff0_0000_0000_0000, MXCSR_RESET | 0x40)
                .unwrap()
                .bits,
            SIGN
        );
        assert_eq!(
            multiply_binary64(1, 0x3ff0_0000_0000_0000, MXCSR_RESET & !0x100),
            Err(FpUnsupported::UnmaskedException(DENORMAL))
        );
    }
    #[test]
    fn rounding_ties_modes_and_overflow() {
        for (mode, positive, negative) in [(0, 2, 2), (1, 2, 3), (2, 3, 2), (3, 2, 2)] {
            assert_eq!(round_integer(5, 1, false, mode), (positive, true));
            assert_eq!(round_integer(5, 1, true, mode), (negative, true));
        }
        assert_eq!(round_integer(7, 1, false, 0), (4, true));
        for mode in 0..4 {
            for sign in [0, SIGN] {
                let c = MXCSR_RESET | mode << 13;
                let r = multiply_binary64((INFINITY - 1) | sign, 0x4000_0000_0000_0000, c).unwrap();
                let inf = mode == 0 || (mode == 1 && sign != 0) || (mode == 2 && sign == 0);
                assert_eq!(r.bits, sign | if inf { INFINITY } else { INFINITY - 1 });
                assert_eq!(r.mxcsr, c | OVERFLOW | PRECISION);
            }
        }
    }
    #[test]
    fn unsupported_cases_and_sticky_flags_are_explicit() {
        assert_eq!(
            multiply_binary64(INFINITY, 0, MXCSR_RESET),
            Err(FpUnsupported::NonFinite)
        );
        assert_eq!(
            multiply_binary64(1, 0x3fe0_0000_0000_0000, MXCSR_RESET),
            Err(FpUnsupported::TinyInexact)
        );
        assert_eq!(
            multiply_binary64(1, 0x3ff0_0000_0000_0000, MXCSR_RESET | 0x8000),
            Err(FpUnsupported::FlushTiny)
        );
        assert_eq!(
            multiply_binary64(0, 0, 0x10000),
            Err(FpUnsupported::ReservedControl)
        );
        let c = MXCSR_RESET & !0x1000 | PRECISION;
        assert_eq!(
            multiply_binary64(0x3ff0_0000_0000_0000, 0x3ff0_0000_0000_0000, c)
                .unwrap()
                .mxcsr,
            c
        );
    }
}
