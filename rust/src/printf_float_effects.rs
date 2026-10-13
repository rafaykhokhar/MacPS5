//! Source-ordered floating operations of FreeBSD 11.4 gdtoa dtoa modes2/3.
//! Derived from David M. Gay's gdtoa; see float_parse/GDTOA-LICENSE.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/contrib/gdtoa/dtoa.c
//! Integer bigint digit generation after these paths performs no FP operations.
//! SET_INEXACT is absent in the audited upstream amd64 configuration. This models
//! scalar SSE binary64 evaluation, not console libc compiler provenance.
use crate::float_parse::decimal_arithmetic as fp;
use crate::scalar_fp::truncate_binary64_i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Control,
    Precision,
    Arithmetic,
}
const SIGN: u64 = 1 << 63;
const ONE: u64 = 0x3ff0000000000000;
const HALF: u64 = 0x3fe0000000000000;
const TENS: [f64; 23] = [
    1., 10., 100., 1000., 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];
const BIG: [f64; 5] = [1e16, 1e32, 1e64, 1e128, 1e256];
struct Trace {
    control: u32,
}
impl Trace {
    fn add(&mut self, a: u64, b: u64) -> Result<u64, Error> {
        let r = fp::add(a, b, self.control).map_err(|_| Error::Arithmetic)?;
        self.control = r.mxcsr;
        Ok(r.bits)
    }
    fn sub(&mut self, a: u64, b: u64) -> Result<u64, Error> {
        let r = fp::subtract(a, b, self.control).map_err(|_| Error::Arithmetic)?;
        self.control = r.mxcsr;
        Ok(r.bits)
    }
    fn mul(&mut self, a: u64, b: u64) -> Result<u64, Error> {
        let r = fp::multiply(a, b, self.control).map_err(|_| Error::Arithmetic)?;
        self.control = r.mxcsr;
        Ok(r.bits)
    }
    fn div(&mut self, a: u64, b: u64) -> Result<u64, Error> {
        let r = fp::divide(a, b, self.control).map_err(|_| Error::Arithmetic)?;
        self.control = r.mxcsr;
        Ok(r.bits)
    }
    fn integer(&mut self, a: u64) -> Result<i32, Error> {
        let r = truncate_binary64_i32(a, self.control).map_err(|_| Error::Arithmetic)?;
        self.control = r.mxcsr;
        Ok(r.bits as i32)
    }
    fn cmp(&mut self, a: u64, b: u64) -> std::cmp::Ordering {
        for x in [a, b] {
            if x & 0x7ff0000000000000 == 0 && x & 0x000fffffffffffff != 0 {
                self.control |= 2;
            }
        }
        let key = |x: u64| {
            if x & !SIGN == 0 {
                SIGN
            } else if x & SIGN != 0 {
                !x
            } else {
                x | SIGN
            }
        };
        key(a).cmp(&key(b))
    }
}
fn integer(value: i32) -> u64 {
    if value == 0 {
        return 0;
    }
    let magnitude = value.unsigned_abs();
    let top = 31 - magnitude.leading_zeros();
    (u64::from(value < 0) << 63)
        | ((u64::from(top) + 1023) << 52)
        | ((u64::from(magnitude) << (52 - top)) & ((1 << 52) - 1))
}
/// Full preserved MXCSR after source dtoa mode2 (significant count) or mode3
/// (fractional count), under masked gradual-underflow controls. Precision is
/// bounded as in printf_float; mode2 count may be4097 for %.4096e.
#[allow(clippy::approx_constant)] // Preserve the upstream approximation constant, not LOG10_2.
pub fn decimal_effects(bits: u64, mode: u8, digits: usize, mxcsr: u32) -> Result<u32, Error> {
    if !matches!(mode, 2 | 3) || digits > 4097 {
        return Err(Error::Precision);
    }
    if mxcsr & !0xffff != 0 || mxcsr & 0x1f80 != 0x1f80 || mxcsr & 0x8040 != 0 {
        return Err(Error::Control);
    }
    let magnitude = bits & !SIGN;
    let encoded = (magnitude >> 52) & 2047;
    if encoded == 2047 {
        return Ok(mxcsr);
    }
    let mut t = Trace { control: mxcsr };
    if t.cmp(magnitude, 0).is_eq() {
        return Ok(t.control);
    }
    let fraction = magnitude & ((1 << 52) - 1);
    let (d2, i, be) = if encoded != 0 {
        let sig = fraction | (1 << 52);
        (
            (magnitude & ((1 << 52) - 1)) | ONE,
            encoded as i32 - 1023,
            encoded as i32 - 1075 + sig.trailing_zeros() as i32,
        )
    } else {
        let top = 63 - fraction.leading_zeros();
        let x = if top > 31 {
            fraction >> (top - 31)
        } else {
            fraction << (31 - top)
        };
        // Source converts the normalized top32 bits exactly, then adjusts exponent.
        let normalized = ONE | ((x & 0x7fffffff) << 21);
        (
            normalized,
            top as i32 - 1074,
            -1074 + fraction.trailing_zeros() as i32,
        )
    };
    let a = t.sub(d2, 1.5f64.to_bits())?;
    let a = t.mul(a, 0.289529654602168f64.to_bits())?;
    let a = t.add(a, 0.1760912590558f64.to_bits())?;
    let b = t.mul(integer(i), 0.301029995663981f64.to_bits())?;
    let ds = t.add(a, b)?;
    let mut k = t.integer(ds)?;
    if t.cmp(ds, 0).is_lt() && !t.cmp(ds, integer(k)).is_eq() {
        k -= 1;
    }
    let mut k_check = true;
    if (0..=22).contains(&k) {
        if t.cmp(magnitude, TENS[k as usize].to_bits()).is_lt() {
            k -= 1;
        }
        k_check = false;
    }
    let ilim = if mode == 2 {
        digits.max(1) as i32
    } else {
        digits as i32 + k + 1
    };
    let ilim1 = if mode == 2 { ilim } else { ilim - 1 };
    // Honor_FLT_ROUNDS maps directed rounding according to sign for digit logic;
    // only nearest (RC0) permits the quick floating path.
    if (0..=14).contains(&ilim)
        && (mxcsr >> 13) & 3 == 0
        && quick(&mut t, magnitude, k, k_check, ilim, ilim1)?
    {
        return Ok(t.control);
    }
    if be >= 0 && k <= 14 {
        let mut d = magnitude;
        let ds = TENS[k as usize].to_bits();
        for iteration in 1..=16 {
            let quotient = t.div(d, ds)?;
            let mut l = t.integer(quotient)?;
            let product = t.mul(integer(l), ds)?;
            d = t.sub(d, product)?;
            if t.cmp(d, 0).is_lt() {
                l -= 1;
                d = t.add(d, ds)?;
            }
            if t.cmp(d, 0).is_eq() {
                return Ok(t.control);
            }
            if iteration == ilim {
                let rc = (mxcsr >> 13) & 3;
                if rc != 0 {
                    return Ok(t.control);
                }
                d = t.add(d, d)?;
                let _ = t.cmp(d, ds);
                let _ = l;
                return Ok(t.control);
            }
            d = t.mul(d, 10f64.to_bits())?;
        }
        return Err(Error::Arithmetic);
    }
    // Remaining path uses bigint arithmetic, assignments and integer tests only.
    Ok(t.control)
}
fn quick(
    t: &mut Trace,
    mut d: u64,
    k: i32,
    k_check: bool,
    mut ilim: i32,
    ilim1: i32,
) -> Result<bool, Error> {
    let mut ieps = 2;
    if k > 0 {
        let mut ds = TENS[(k & 15) as usize].to_bits();
        let mut j = k >> 4;
        let mut i = 0;
        if j & 16 != 0 {
            j &= 15;
            d = t.div(d, BIG[4].to_bits())?;
            ieps += 1;
        }
        while j != 0 {
            if j & 1 != 0 {
                ieps += 1;
                ds = t.mul(ds, BIG[i].to_bits())?;
            }
            j >>= 1;
            i += 1;
        }
        d = t.div(d, ds)?;
    } else if k < 0 {
        let j1 = -k;
        d = t.mul(d, TENS[(j1 & 15) as usize].to_bits())?;
        let mut j = j1 >> 4;
        let mut i = 0;
        while j != 0 {
            if j & 1 != 0 {
                ieps += 1;
                d = t.mul(d, BIG[i].to_bits())?;
            }
            j >>= 1;
            i += 1;
        }
    }
    if k_check && t.cmp(d, ONE).is_lt() && ilim > 0 {
        if ilim1 <= 0 {
            return Ok(false);
        }
        ilim = ilim1;
        d = t.mul(d, 10f64.to_bits())?;
        ieps += 1;
    }
    let product = t.mul(integer(ieps), d)?;
    let eps = t.add(product, 7f64.to_bits())?;
    let mut eps = eps - (52u64 << 52);
    if ilim == 0 {
        d = t.sub(d, 5f64.to_bits())?;
        return Ok(t.cmp(d, eps).is_gt() || t.cmp(d, eps ^ SIGN).is_lt());
    }
    eps = t.mul(eps, TENS[(ilim - 1) as usize].to_bits())?;
    for iteration in 1..=14 {
        let l = t.integer(d)?;
        d = t.sub(d, integer(l))?;
        if t.cmp(d, 0).is_eq() {
            ilim = iteration;
        }
        if iteration == ilim {
            let upper = t.add(HALF, eps)?;
            if t.cmp(d, upper).is_gt() {
                return Ok(true);
            }
            let lower = t.sub(HALF, eps)?;
            return Ok(t.cmp(d, lower).is_lt());
        }
        d = t.mul(d, 10f64.to_bits())?;
    }
    Err(Error::Arithmetic)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_vectors_cover_source_paths() {
        // Unmodified FreeBSD11.4 dtoa, x86 SSE oracle through Rosetta; captured
        // work/arm64-prototype/printf-audit/oracle/results-x86.log.
        for (bits, raised) in [
            (0u64, 0),
            (1, 34),
            (3, 34),
            (1.0f64.to_bits(), 32),
            (1.5f64.to_bits(), 32),
            (0.1f64.to_bits(), 32),
            (f64::MAX.to_bits(), 32),
            (0x7ff0000000000001, 0),
        ] {
            for rc in 0..4 {
                for (mode, p) in [(3, 0), (3, 2), (2, 6)] {
                    let control = 0x1f80 | (rc << 13) | 1;
                    assert_eq!(
                        decimal_effects(bits, mode, p, control),
                        Ok(control | raised)
                    );
                }
            }
        }
    }
    #[test]
    fn long_precision_bigint_path_and_small_integer() {
        for value in [
            1.0f64,
            10.0,
            123456789.0,
            1e14,
            1e100,
            f64::MIN_POSITIVE,
            f64::MAX,
        ] {
            for rc in 0..4 {
                let control = 0x1f80 | (rc << 13);
                assert_eq!(
                    decimal_effects(value.to_bits(), 3, 4096, control),
                    Ok(control | 32)
                );
            }
        }
    }
    #[test]
    fn explicit_controls_and_precision_bounds() {
        for control in [0, 0x9f80, 0x1fc0, 0x11f80] {
            assert_eq!(decimal_effects(0, 2, 6, control), Err(Error::Control));
        }
        assert_eq!(decimal_effects(0, 1, 6, 0x1f80), Err(Error::Precision));
        assert_eq!(
            decimal_effects(0, 2, usize::MAX, 0x1f80),
            Err(Error::Precision)
        );
    }
}
