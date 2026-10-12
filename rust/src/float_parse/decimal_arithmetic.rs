//! Masked, gradual-underflow binary64 operations for gdtoa's decimal algorithm.
//! No host floating point. Unsupported controls and operands fail explicitly.
use crate::scalar_fp::{fma213_binary64, multiply_binary64, FpResult};
const SIGN: u64 = 1 << 63;
const INF: u64 = 0x7ff0_0000_0000_0000;
const FRAC: u64 = (1 << 52) - 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Control,
    NonFinite,
    ZeroDivisor,
}
fn validate(a: u64, b: u64, mxcsr: u32) -> Result<(), Error> {
    if mxcsr & !0xffff != 0 || mxcsr & 0x1f80 != 0x1f80 || mxcsr & 0x8040 != 0 {
        return Err(Error::Control);
    }
    if a & INF == INF || b & INF == INF {
        return Err(Error::NonFinite);
    }
    Ok(())
}
pub fn multiply(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, Error> {
    validate(a, b, mxcsr)?;
    multiply_binary64(a, b, mxcsr).map_err(|_| Error::Control)
}
pub fn add(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, Error> {
    validate(a, b, mxcsr)?;
    // Multiplication by exactly one is exact for every finite binary64 value;
    // hence a*1+b has exactly the ADD operation's single rounding and flags.
    fma213_binary64(a, 0x3ff0_0000_0000_0000, b, mxcsr).map_err(|_| Error::Control)
}
pub fn subtract(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, Error> {
    add(a, b ^ SIGN, mxcsr)
}
fn unpack(bits: u64) -> (u64, i32, u32) {
    let e = ((bits >> 52) & 2047) as i32;
    let f = bits & FRAC;
    if e == 0 {
        (f, -1074, if f != 0 { 2 } else { 0 })
    } else {
        (f | (1 << 52), e - 1075, 0)
    }
}
// Exact rational quantization. For normal precision numerator/denominator
// need <=106 bits; extreme underflow has quotient zero without a giant shift.
fn quantize(a: u64, b: u64, shift: i32, negative: bool, mode: u32) -> (u64, bool) {
    let (q, rem, half) = if shift >= 0 {
        let n = (a as u128) << shift;
        let d = b as u128;
        let rem = n % d;
        ((n / d) as u64, rem, (rem * 2).cmp(&d))
    } else if -shift >= 128 || (64 - b.leading_zeros()) as i32 - shift > 127 {
        (0, 1, std::cmp::Ordering::Less)
    } else {
        let n = a as u128;
        let d = (b as u128) << (-shift);
        let rem = n % d;
        ((n / d) as u64, rem, (rem * 2).cmp(&d))
    };
    let inexact = rem != 0;
    let inc = inexact
        && match mode {
            0 => half.is_gt() || (half.is_eq() && q & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    (q + u64::from(inc), inexact)
}
pub fn divide(a: u64, b: u64, mxcsr: u32) -> Result<FpResult, Error> {
    validate(a, b, mxcsr)?;
    let negative = (a ^ b) & SIGN != 0;
    let (a, ae, af) = unpack(a);
    let (b, be, bf) = unpack(b);
    if b == 0 {
        return Err(Error::ZeroDivisor);
    }
    divide_parts(a, b, ae - be, af | bf, mxcsr, negative)
}
fn divide_parts(
    a: u64,
    b: u64,
    scale: i32,
    flags: u32,
    mxcsr: u32,
    negative: bool,
) -> Result<FpResult, Error> {
    let sign = if negative { SIGN } else { 0 };
    if a == 0 {
        return Ok(FpResult {
            bits: sign,
            mxcsr: mxcsr | flags,
        });
    }
    let guess = (64 - a.leading_zeros()) as i32 - (64 - b.leading_zeros()) as i32;
    let less = if guess >= 0 {
        (a as u128) < ((b as u128) << guess)
    } else {
        ((a as u128) << -guess) < b as u128
    };
    let exponent = guess - i32::from(less) + scale;
    let mode = (mxcsr >> 13) & 3;
    let unit = (exponent - 52).max(-1074);
    let (mut q, inexact) = quantize(a, b, scale - unit, negative, mode);
    let (mut e, mut raised) = (exponent, flags | if inexact { 32 } else { 0 });
    let magnitude = if exponent < -1022 {
        let (unbounded, _) = quantize(a, b, scale - (exponent - 52), negative, mode);
        let tiny = exponent + i32::from(unbounded >= 1 << 53) < -1022;
        if tiny && inexact {
            raised |= 16;
        }
        q
    } else {
        if q >= 1 << 53 {
            q >>= 1;
            e += 1;
        }
        if e > 1023 {
            raised |= 40;
            if mode == 0 || (mode == 1 && negative) || (mode == 2 && !negative) {
                INF
            } else {
                INF - 1
            }
        } else {
            ((e + 1023) as u64) << 52 | (q & FRAC)
        }
    };
    Ok(FpResult {
        bits: sign | magnitude,
        mxcsr: mxcsr | raised,
    })
}
