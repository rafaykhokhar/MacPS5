//! Integer-only binary64 multiplication. Guest exception delivery is a separate
//! runtime responsibility: errors contain updated status but no destination.
//! MXCSR layout/rounding: Intel SDM vol.1 §10.2.3 and AMD APM vol.4
//! https://www.amd.com/content/dam/amd/en/documents/processor-tech-docs/programmer-references/26568.pdf
//! NaN source priority: AMD APM vol.1 table4-5 (source1 wins when both NaN).
//! https://docs.amd.com/api/khub/documents/sfvvekC9mDflu6vd3R0NXA/content
//! Exception ordering: Intel FP reference sheet page2 stops on unmasked DE
//! before arithmetic, and on unmasked OE/UE before setting PE.
//! https://www.intel.com/content/www/us/en/content-details/786447/floating-point-reference-sheet-for-intel-architecture.html
//! Tininess uses rounding to53bits with unbounded exponent, Intel vol.1 §4.9.1.5:
//! https://cdrdv2-public.intel.com/843820/325462-sdm-vol-1-2abcd-3abcd-4-1.pdf
//! AMD vol.1 §4.2.2: FZ+UM flushes rounded tiny results, even exact ones, and
//! sets UE+PE; FZ is ignored when UM=0:
//! https://docs.amd.com/api/khub/documents/68GKiN0gMEd6bMddsmhPwg/content
use super::MXCSR_VALID;
const SIGN: u64 = 1 << 63;
const FRACTION: u64 = (1 << 52) - 1;
const INFINITY: u64 = 0x7ff0_0000_0000_0000;
const DENORMAL: u32 = 2;
const OVERFLOW: u32 = 8;
const UNDERFLOW: u32 = 16;
const PRECISION: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpResult {
    pub bits: u64,
    pub mxcsr: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpUnsupported {
    ReservedControl,
    Exception { mxcsr: u32, raised: u32 },
}

/// Pure calculation: callers must perform guest exception/commit handling.
/// Existing sticky flags never cause a new exception by themselves.
pub fn multiply_binary64(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, FpUnsupported> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(FpUnsupported::ReservedControl);
    }
    let nan = |bits: u64| bits & INFINITY == INFINITY && bits & FRACTION != 0;
    if nan(a) || nan(b) {
        // AMD APM vol.1 table4-5: prefer source1 when both are NaNs,
        // regardless of quiet/signaling class; either signaling input sets IE.
        let invalid = (nan(a) && a & (1 << 51) == 0) || (nan(b) && b & (1 << 51) == 0);
        return finish(
            (if nan(a) { a } else { b }) | (1 << 51),
            mxcsr,
            u32::from(invalid),
        );
    }
    let sign = (a ^ b) & SIGN;
    let mut status = 0;
    let (av, ae) = unpack(a, mxcsr, &mut status);
    let (bv, be) = unpack(b, mxcsr, &mut status);
    let ai = a & !SIGN == INFINITY;
    let bi = b & !SIGN == INFINITY;
    // Invalid-operation precedes denormal handling (including DAZ zero*inf).
    if (ai && bv == 0) || (bi && av == 0) {
        return finish(0xfff8_0000_0000_0000, mxcsr, 1);
    }
    if status & !(mxcsr >> 7) != 0 {
        return finish(0, mxcsr, status);
    }
    if ai || bi {
        return finish(sign | INFINITY, mxcsr, status);
    }
    let (a, b) = (av, bv);
    let (magnitude, rounding_status) = if a == 0 || b == 0 {
        (0, 0)
    } else {
        match round_pack(a as u128 * b as u128, ae + be, sign != 0, mxcsr) {
            Ok(result) => result,
            Err(FpUnsupported::Exception {
                mxcsr: control,
                raised,
            }) => {
                return Err(FpUnsupported::Exception {
                    mxcsr: control | status,
                    raised,
                });
            }
            Err(error) => return Err(error),
        }
    };
    status |= rounding_status;
    finish(sign | magnitude, mxcsr, status)
}

pub(super) fn finish(bits: u64, mxcsr: u32, status: u32) -> Result<FpResult, FpUnsupported> {
    let unmasked = status & !(mxcsr >> 7) & 0x3f;
    if unmasked != 0 {
        return Err(FpUnsupported::Exception {
            mxcsr: mxcsr | status,
            raised: unmasked,
        });
    }
    Ok(FpResult {
        bits,
        mxcsr: mxcsr | status,
    })
}

pub(super) fn unpack(bits: u64, control: u32, status: &mut u32) -> (u64, i32) {
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

/// Round a positive integer times2^scale once. Multiplication passes its exact
/// 106-bit product; FMA passes127 leading bits with the low bit made sticky.
/// That retained width exceeds destination precision plus guard/round bits.
pub(super) fn round_pack(
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
        // This extra rounding determines only tininess; never feed its rounded
        // value to final packing (which would introduce double rounding).
        let (unbounded, _) = round_integer(value, top - 52, negative, mode);
        let unbounded_exponent = exponent + i32::from(unbounded >= 1u128 << 53);
        let tiny = unbounded_exponent < -1022;
        if tiny && control & (1 << 11) == 0 {
            return Err(FpUnsupported::Exception {
                mxcsr: control | UNDERFLOW,
                raised: UNDERFLOW,
            });
        }
        if tiny && control & 0x8000 != 0 {
            return Ok((0, UNDERFLOW | PRECISION));
        }
        // rounded can equal1<<52: smallest normal encoded directly. Its final
        // normal encoding does not itself suppress a detected tiny condition.
        return Ok((
            rounded as u64,
            if inexact {
                PRECISION | if tiny { UNDERFLOW } else { 0 }
            } else {
                0
            },
        ));
    }
    let mut output_exponent = exponent;
    if rounded >= 1u128 << 53 {
        rounded >>= 1;
        output_exponent += 1;
    }
    if output_exponent > 1023 {
        if control & (1 << 10) == 0 {
            return Err(FpUnsupported::Exception {
                mxcsr: control | OVERFLOW,
                raised: OVERFLOW,
            });
        }
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
            Err(FpUnsupported::Exception {
                mxcsr: (MXCSR_RESET & !0x100) | DENORMAL,
                raised: DENORMAL
            })
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
    fn gradual_underflow_ftz_and_unbounded_tininess() {
        for rc in 0..4 {
            for sign in [0, SIGN] {
                let control = MXCSR_RESET | rc << 13;
                let away = (rc == 1 && sign != 0) || (rc == 2 && sign == 0);
                let result = multiply_binary64(sign | 1, 0x3fe0_0000_0000_0000, control).unwrap();
                assert_eq!(result.bits, sign | u64::from(away));
                assert_eq!(result.mxcsr, control | DENORMAL | UNDERFLOW | PRECISION);
                let flushed =
                    multiply_binary64(sign | 1, 0x3ff0_0000_0000_0000, control | 0x8000).unwrap();
                assert_eq!(flushed.bits, sign);
                assert_eq!(
                    flushed.mxcsr,
                    control | 0x8000 | DENORMAL | UNDERFLOW | PRECISION
                );
            }
        }
        // Midpoint below smallestnormal is exactly53bits with unbounded
        // exponent: still tiny, even though gradual rounding gives normal.
        assert_eq!(
            round_pack((1u128 << 53) - 1, -1075, false, MXCSR_RESET),
            Ok((1 << 52, UNDERFLOW | PRECISION))
        );
        // Quarterulp below smallestnormal rounds to normal at unbounded53bits:
        // precision but no underflow, and FTZ must not flush it.
        for ftz in [0, 0x8000] {
            assert_eq!(
                round_pack((1u128 << 54) - 1, -1076, false, MXCSR_RESET | ftz),
                Ok((1 << 52, PRECISION))
            );
        }
        for ftz in [0, 0x8000] {
            assert_eq!(
                multiply_binary64(1, 0x3ff0_0000_0000_0000, (MXCSR_RESET & !(1 << 11)) | ftz),
                Err(FpUnsupported::Exception {
                    mxcsr: (MXCSR_RESET & !(1 << 11)) | ftz | DENORMAL | UNDERFLOW,
                    raised: UNDERFLOW
                })
            );
        }
        assert_eq!(
            multiply_binary64(
                1,
                0x3ff0_0000_0000_0000,
                (MXCSR_RESET & !(1 << 12)) | 0x8000
            ),
            Err(FpUnsupported::Exception {
                mxcsr: (MXCSR_RESET & !(1 << 12)) | 0x8000 | DENORMAL | UNDERFLOW | PRECISION,
                raised: PRECISION
            })
        );
    }
    #[test]
    fn unsupported_cases_and_sticky_flags_are_explicit() {
        assert_eq!(
            multiply_binary64(INFINITY, 0, MXCSR_RESET),
            Ok(FpResult {
                bits: 0xfff8_0000_0000_0000,
                mxcsr: MXCSR_RESET | 1
            })
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

#[allow(unsafe_code)]
mod abi {
    use super::*;
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// cpu is a live uniquely borrowed guest context matching the720byte ABI.
    /// Caller must check cpu.fault before committing returned bits to a register.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_multiply_binary64_v1(cpu: *mut Cpu, a: u64, b: u64) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match multiply_binary64(a, b, super::super::mxcsr(cpu)) {
            Ok(result) => {
                cpu.padding = result.mxcsr as u64;
                result.bits
            }
            Err(FpUnsupported::ReservedControl) => {
                cpu.fault = super::super::CONTROL_FAULT;
                0
            }
            Err(FpUnsupported::Exception { mxcsr, .. }) => {
                cpu.padding = mxcsr as u64;
                cpu.fault = 15;
                0
            }
        }
    }
}

#[cfg(test)]
mod special_tests {
    use super::*;
    use crate::scalar_fp::MXCSR_RESET;
    #[test]
    fn nan_priority_signaling_and_infinity() {
        let nans = [
            0x7ff8_0000_0000_0001,
            0xfff8_0000_0000_0042,
            0x7ff0_0000_0000_0003,
            0xfff0_0000_0000_0099,
        ];
        for a in nans {
            for b in nans {
                let invalid = u32::from(a & (1 << 51) == 0 || b & (1 << 51) == 0);
                assert_eq!(
                    multiply_binary64(a, b, MXCSR_RESET).unwrap(),
                    FpResult {
                        bits: a | (1 << 51),
                        mxcsr: MXCSR_RESET | invalid
                    }
                );
                if invalid != 0 {
                    assert_eq!(
                        multiply_binary64(a, b, MXCSR_RESET & !128),
                        Err(FpUnsupported::Exception {
                            mxcsr: (MXCSR_RESET & !128) | 1,
                            raised: 1
                        })
                    );
                }
            }
        }
        for a in nans {
            let status = u32::from(a & (1 << 51) == 0);
            for (a, b) in [(a, 1), (1, a)] {
                assert_eq!(
                    multiply_binary64(a, b, MXCSR_RESET & !256).unwrap().mxcsr,
                    (MXCSR_RESET & !256) | status
                );
            }
        }
        for sign in [0, SIGN] {
            assert_eq!(
                multiply_binary64(INFINITY | sign, 0x4000_0000_0000_0000, MXCSR_RESET)
                    .unwrap()
                    .bits,
                INFINITY | sign
            );
            assert_eq!(
                multiply_binary64(INFINITY, sign, MXCSR_RESET).unwrap().bits,
                0xfff8_0000_0000_0000
            );
        }
        assert_eq!(
            multiply_binary64(INFINITY, 1, MXCSR_RESET | 0x40)
                .unwrap()
                .mxcsr,
            MXCSR_RESET | 0x40 | 1
        );
        assert_eq!(
            multiply_binary64(INFINITY, 1, MXCSR_RESET).unwrap().mxcsr,
            MXCSR_RESET | DENORMAL
        );
    }
    #[test]
    fn exceptions_stop_before_lower_priority_arithmetic() {
        let dm = MXCSR_RESET & !256;
        assert_eq!(
            multiply_binary64(1, 1, dm),
            Err(FpUnsupported::Exception {
                mxcsr: dm | DENORMAL,
                raised: DENORMAL
            })
        );
        let om = MXCSR_RESET & !1024;
        assert_eq!(
            multiply_binary64(INFINITY - 1, 0x4000_0000_0000_0000, om),
            Err(FpUnsupported::Exception {
                mxcsr: om | OVERFLOW,
                raised: OVERFLOW
            })
        );
        let um = MXCSR_RESET & !2048;
        assert_eq!(
            multiply_binary64(1, 1, um),
            Err(FpUnsupported::Exception {
                mxcsr: um | DENORMAL | UNDERFLOW,
                raised: UNDERFLOW
            })
        );
    }
}
