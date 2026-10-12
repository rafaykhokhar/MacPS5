//! Exact fused binary64 operation, including cancellation across the full range.
//! Intel SDM vol.1 §14.5 FMA arithmetic and operand-order tables; NaN behavior:
//! https://cdrdv2-public.intel.com/843827/253665-sdm-vol-1-dec-24.pdf
//! Shared round/pack implements the documented MXCSR exception/tininess rules.
use super::arithmetic::{finish, round_pack, unpack};
use super::{FpResult, FpUnsupported, MXCSR_VALID};
const SIGN: u64 = 1 << 63;
const INF: u64 = 0x7ff0_0000_0000_0000;
const FRACTION: u64 = (1 << 52) - 1;
const BASE: i32 = -2148;
const LIMBS: usize = 68;

/// VFMADD213SD mathematical result src1*olddst+src2, rounded only once.
/// Error never contains a result to commit; exception status is separately owned.
pub fn fma213_binary64(
    olddst: u64,
    src1: u64,
    src2: u64,
    mxcsr: u32,
) -> Result<FpResult, FpUnsupported> {
    if mxcsr & !MXCSR_VALID != 0 {
        return Err(FpUnsupported::ReservedControl);
    }
    let nan = |x: u64| x & INF == INF && x & FRACTION != 0;
    let operands = [src1, olddst, src2];
    if let Some(bits) = operands.iter().copied().find(|&x| nan(x)) {
        let invalid = operands.iter().any(|&x| nan(x) && x & (1 << 51) == 0);
        return finish(bits | (1 << 51), mxcsr, u32::from(invalid));
    }
    let product_sign = (olddst ^ src1) & SIGN;
    let addend_sign = src2 & SIGN;
    let mut status = 0;
    let (a, ae) = unpack(olddst, mxcsr, &mut status);
    let (b, be) = unpack(src1, mxcsr, &mut status);
    let (c, ce) = unpack(src2, mxcsr, &mut status);
    let ai = olddst & !SIGN == INF;
    let bi = src1 & !SIGN == INF;
    let ci = src2 & !SIGN == INF;
    if (ai && b == 0) || (bi && a == 0) || ((ai || bi) && ci && product_sign != addend_sign) {
        return finish(0xfff8_0000_0000_0000, mxcsr, 1);
    }
    if status & !(mxcsr >> 7) != 0 {
        return finish(0, mxcsr, status);
    }
    if ai || bi {
        return finish(product_sign | INF, mxcsr, status);
    }
    if ci {
        return finish(src2, mxcsr, status);
    }
    let mut product = Wide::from_scaled(a as u128 * b as u128, ae + be);
    let addend = Wide::from_scaled(c as u128, ce);
    let sign = if product_sign == addend_sign {
        product.add(&addend);
        product_sign
    } else {
        match product.compare(&addend) {
            std::cmp::Ordering::Greater => {
                product.subtract(&addend);
                product_sign
            }
            std::cmp::Ordering::Less => {
                let mut larger = addend;
                larger.subtract(&product);
                product = larger;
                addend_sign
            }
            std::cmp::Ordering::Equal => {
                return finish(if (mxcsr >> 13) & 3 == 1 { SIGN } else { 0 }, mxcsr, status);
            }
        }
    };
    let Some((magnitude, scale)) = product.rounding_window() else {
        return finish(sign, mxcsr, status);
    };
    match round_pack(magnitude, scale, sign != 0, mxcsr) {
        Ok((bits, flags)) => finish(sign | bits, mxcsr, status | flags),
        Err(FpUnsupported::Exception { mxcsr, raised }) => Err(FpUnsupported::Exception {
            mxcsr: mxcsr | status,
            raised,
        }),
        Err(error) => Err(error),
    }
}

/// Smallest product quantum2^-2148. Largest exact product has highest bit4195
/// in this coordinate system; adding a binary64 addend cannot reach limb68.
/// The fixed4352-bit accumulator therefore covers every finite operand triple.
struct Wide([u64; LIMBS]);
impl Wide {
    fn from_scaled(value: u128, scale: i32) -> Self {
        let mut wide = Self([0; LIMBS]);
        let shift = (scale - BASE) as usize;
        for bit in 0..128 {
            if value & (1u128 << bit) != 0 {
                let index = shift + bit;
                wide.0[index / 64] |= 1u64 << (index % 64);
            }
        }
        wide
    }
    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
    fn add(&mut self, other: &Self) {
        let mut carry = false;
        for (a, b) in self.0.iter_mut().zip(other.0) {
            let (sum, first) = a.overflowing_add(b);
            let (sum, second) = sum.overflowing_add(u64::from(carry));
            *a = sum;
            carry = first || second;
        }
        assert!(!carry, "proved full-range accumulator bound");
    }
    fn subtract(&mut self, other: &Self) {
        let mut borrow = false;
        for (a, b) in self.0.iter_mut().zip(other.0) {
            let (difference, first) = a.overflowing_sub(b);
            let (difference, second) = difference.overflowing_sub(u64::from(borrow));
            *a = difference;
            borrow = first || second;
        }
        assert!(!borrow, "subtract called only on greater magnitude");
    }
    fn bit(&self, bit: usize) -> bool {
        self.0[bit / 64] & (1u64 << (bit % 64)) != 0
    }
    fn rounding_window(&self) -> Option<(u128, i32)> {
        let limb = self.0.iter().rposition(|&x| x != 0)?;
        let top = limb * 64 + 63 - self.0[limb].leading_zeros() as usize;
        let discard = top.saturating_sub(126);
        let mut window = 0u128;
        for bit in discard..=top {
            if self.bit(bit) {
                window |= 1u128 << (bit - discard);
            }
        }
        // Keep127 bits, much more than53-bit precision plus guard/round. A
        // sticky LSB encodes every lower nonzero bit without changing any tie.
        if (0..discard).any(|bit| self.bit(bit)) {
            window |= 1;
        }
        Some((window, BASE + discard as i32))
    }
}

#[allow(unsafe_code)]
mod abi {
    use super::*;
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// cpu is a unique live guest context; caller checks fault before dst commit.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_fma213_binary64_v1(
        cpu: *mut Cpu,
        olddst: u64,
        src1: u64,
        src2: u64,
    ) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        match fma213_binary64(olddst, src1, src2, super::super::mxcsr(cpu)) {
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
mod tests {
    use super::*;
    use crate::scalar_fp::MXCSR_RESET;
    #[test]
    fn fused_cancellation_and_extreme_scale() {
        // (1+2^-52)*(1-2^-52)-1 = -2^-104, lost by separate mul/add.
        assert_eq!(
            fma213_binary64(
                0x3ff0_0000_0000_0001,
                0x3fef_ffff_ffff_fffe,
                0xbff0_0000_0000_0000,
                MXCSR_RESET
            )
            .unwrap(),
            FpResult {
                bits: 0xb970_0000_0000_0000,
                mxcsr: MXCSR_RESET
            }
        );
        assert_eq!(
            fma213_binary64(
                INF - 1,
                0x4000_0000_0000_0000,
                (INF - 1) | SIGN,
                MXCSR_RESET
            )
            .unwrap()
            .bits,
            INF - 1
        );
        assert_eq!(
            fma213_binary64(1, 1, 0x3ff0_0000_0000_0000, MXCSR_RESET).unwrap(),
            FpResult {
                bits: 0x3ff0_0000_0000_0000,
                mxcsr: MXCSR_RESET | 2 | 32
            }
        );
        assert_eq!(
            fma213_binary64(1, 1, 0x3ff0_0000_0000_0000, MXCSR_RESET | (2 << 13))
                .unwrap()
                .bits,
            0x3ff0_0000_0000_0001
        );
    }
    #[test]
    fn signed_zero_cancellation_and_nan_priority() {
        for rc in 0..4 {
            let control = MXCSR_RESET | rc << 13;
            assert_eq!(
                fma213_binary64(
                    0x3ff0_0000_0000_0000,
                    0x3ff0_0000_0000_0000,
                    0xbff0_0000_0000_0000,
                    control
                )
                .unwrap()
                .bits,
                if rc == 1 { SIGN } else { 0 }
            );
            assert_eq!(
                fma213_binary64(SIGN, 0x3ff0_0000_0000_0000, SIGN, control)
                    .unwrap()
                    .bits,
                SIGN
            );
        }
        assert_eq!(
            fma213_binary64(INF, 0, 0x7ff8_0000_0000_0042, MXCSR_RESET).unwrap(),
            FpResult {
                bits: 0x7ff8_0000_0000_0042,
                mxcsr: MXCSR_RESET
            }
        );
        assert_eq!(
            fma213_binary64(
                0x7ff8_0000_0000_0001,
                0x7ff0_0000_0000_0002,
                0xfff8_0000_0000_0003,
                MXCSR_RESET
            )
            .unwrap(),
            FpResult {
                bits: 0x7ff8_0000_0000_0002,
                mxcsr: MXCSR_RESET | 1
            }
        );
        assert_eq!(
            fma213_binary64(INF, 0x3ff0_0000_0000_0000, INF | SIGN, MXCSR_RESET)
                .unwrap()
                .bits,
            0xfff8_0000_0000_0000
        );
    }
}
