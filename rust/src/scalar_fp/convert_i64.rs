//! Signed64 integer to binary64, using guest MXCSR rather than host FP state.
//! Intel SDM CVTSI2SD: only precision can be raised; RC selects rounding.
//! https://cdrdv2-public.intel.com/782156/325383-sdm-vol-2abcd.pdf
//! Integer inputs and normal finite outputs make DAZ/FTZ irrelevant.
use super::{FpResult, FpUnsupported, MXCSR_VALID, arithmetic::finish};

pub fn convert_i64_binary64(value: i64, mxcsr: u32) -> Result<FpResult, FpUnsupported> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(FpUnsupported::ReservedControl);
    }
    let magnitude = value.unsigned_abs();
    if magnitude == 0 {
        return finish(0, mxcsr, 0);
    }
    let negative = value < 0;
    let mut exponent = 63 - magnitude.leading_zeros();
    let (mut significand, remainder, shift) = if exponent <= 52 {
        (magnitude << (52 - exponent), 0, 0)
    } else {
        let shift = exponent - 52;
        (magnitude >> shift, magnitude & ((1 << shift) - 1), shift)
    };
    if remainder != 0 {
        let increment = match (mxcsr >> 13) & 3 {
            0 => {
                let halfway = 1 << (shift - 1);
                remainder > halfway || (remainder == halfway && significand & 1 != 0)
            }
            1 => negative,
            2 => !negative,
            _ => false,
        };
        significand += u64::from(increment);
        if significand == 1 << 53 {
            significand >>= 1;
            exponent += 1;
        }
    }
    let bits = (u64::from(negative) << 63)
        | (u64::from(exponent + 1023) << 52)
        | (significand & ((1 << 52) - 1));
    finish(bits, mxcsr, if remainder == 0 { 0 } else { 32 })
}

#[allow(unsafe_code)]
mod abi {
    use super::*;
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// cpu is a uniquely borrowed live CPU. Caller checks fault before merging
    /// or committing any destination lane; an unmasked precision exception only
    /// updates status and reports the prototype SIMD fault, not signal delivery.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_convert_i64_binary64_v1(cpu: *mut Cpu, value: i64) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match convert_i64_binary64(value, cpu.mxcsr) {
            Ok(result) => {
                cpu.mxcsr = result.mxcsr;
                result.bits
            }
            Err(FpUnsupported::ReservedControl) => {
                cpu.fault = super::super::CONTROL_FAULT;
                0
            }
            Err(FpUnsupported::Exception { mxcsr, .. }) => {
                cpu.mxcsr = mxcsr;
                cpu.fault = 15;
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_ties_extremes_and_control() {
        for (value, expected) in [
            (0, 0),
            (1, 0x3ff0000000000000),
            (-1, 0xbff0000000000000),
            (i64::MIN, 0xc3e0000000000000),
        ] {
            for rc in 0..4 {
                assert_eq!(
                    convert_i64_binary64(value, 0x1f80 | (rc << 13)).unwrap(),
                    FpResult {
                        bits: expected,
                        mxcsr: 0x1f80 | (rc << 13)
                    }
                );
            }
        }
        for (value, nearest) in [
            ((1i64 << 53) + 1, 0x4340000000000000),
            ((1i64 << 53) + 3, 0x4340000000000002),
            (i64::MAX, 0x43e0000000000000),
        ] {
            assert_eq!(
                convert_i64_binary64(value, 0x1f80).unwrap(),
                FpResult {
                    bits: nearest,
                    mxcsr: 0x1fa0
                }
            );
        }
        assert_eq!(
            convert_i64_binary64(i64::MAX, 0x7f80).unwrap().bits,
            0x43dfffffffffffff
        );
        assert_eq!(
            convert_i64_binary64(-((1i64 << 53) + 1), 0x3f80)
                .unwrap()
                .bits,
            0xc340000000000001
        );
        assert_eq!(
            convert_i64_binary64((1i64 << 53) + 1, 0x0f80),
            Err(FpUnsupported::Exception {
                mxcsr: 0x0fa0,
                raised: 32
            })
        );
        assert_eq!(convert_i64_binary64(1, 0x20).unwrap().mxcsr, 0x20); // sticky doesn't re-raise
        assert_eq!(
            convert_i64_binary64(0, 0x10000),
            Err(FpUnsupported::ReservedControl)
        );
    }
}
