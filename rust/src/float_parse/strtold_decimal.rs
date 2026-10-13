//! Source-ordered 64-bit strtodg decimal conversion.
//! Derived from David M. Gay's FreeBSD 11.4 gdtoa strtodg.c; see GDTOA-LICENSE.
use super::decimal_arithmetic::{self as fp, Error as FpError};
use super::decimal_big::Integer;
use super::{Rounding, parse};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Arithmetic(FpError),
    Input,
    IterationBudget,
}
impl From<FpError> for Error {
    fn from(e: FpError) -> Self {
        Self::Arithmetic(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Result80 {
    pub bytes: [u8; 10],
    pub errno: Option<u32>,
    pub mxcsr: u32,
}
const ONE: u64 = 0x3ff0000000000000;
const HALF: u64 = 0x3fe0000000000000;
fn integer(v: u32) -> u64 {
    if v == 0 {
        return 0;
    }
    let n = 31 - v.leading_zeros();
    ((u64::from(n) + 1023) << 52) | ((u64::from(v) << (52 - n)) & ((1 << 52) - 1))
}
fn ten(n: i32) -> u64 {
    parse(format!("1e{n}").as_bytes(), Rounding::NearestEven)
        .unwrap()
        .bits
}
fn unpack(v: u64) -> (u64, i32, i32) {
    let field = ((v >> 52) & 2047) as i32;
    let m = (v & ((1 << 52) - 1)) | if field == 0 { 0 } else { 1 << 52 };
    if m == 0 {
        return (0, 0, 0);
    }
    let z = m.trailing_zeros();
    (
        m >> z,
        if field == 0 { -1074 } else { field - 1075 } + z as i32,
        64 - m.leading_zeros() as i32 - z as i32,
    )
}
fn finish(
    m: u128,
    e: i32,
    denorm: bool,
    negative: bool,
    errno: Option<u32>,
    mxcsr: u32,
) -> Result80 {
    let (significand, exponent) = if m == 0 {
        (0, 0)
    } else if e > 16320 {
        (1u64 << 63, 0x7fff)
    } else {
        (m as u64, if denorm { 0 } else { (e + 16446) as u16 })
    };
    let mut bytes = [0; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&(exponent | if negative { 0x8000 } else { 0 }).to_le_bytes());
    Result80 {
        bytes,
        errno,
        mxcsr,
    }
}
/// `digits` is gdtoa's trimmed nonzero coefficient, exact value digits*10^e.
/// Limits match the existing lexical model; arithmetic iteration is bounded.
pub fn decimal(digits: &[u8], e: i32, negative: bool, mut control: u32) -> Result<Result80, Error> {
    if digits.is_empty()
        || digits.len() > 4096
        || digits[0] == b'0'
        || !digits.iter().all(u8::is_ascii_digit)
        || !(-85535..=19999).contains(&e)
    {
        return Err(Error::Input);
    }
    // Validate controls even when the source fast path uses no FP operation.
    fp::add(0, 0, control)?;
    let mode = (control >> 13) & 3;
    let rd = match mode {
        1 => 1 + u32::from(negative),
        2 => 2 - u32::from(negative),
        3 => 1,
        _ => 0,
    };
    macro_rules! op {
        ($name:ident,$a:expr,$b:expr) => {{
            let r = fp::$name($a, $b, control)?;
            control = r.mxcsr;
            r.bits
        }};
    }
    let k = digits.len().min(16);
    let (mut y, mut z) = (0, 0);
    for (i, d) in digits[..k].iter().enumerate() {
        if i < 9 {
            y = y * 10 + u32::from(d - b'0')
        } else {
            z = z * 10 + u32::from(d - b'0')
        }
    }
    let mut rv = integer(y);
    if k > 9 {
        rv = op!(multiply, ten((k - 9) as i32), rv);
        rv = op!(add, rv, integer(z))
    }
    let mut e1 = e;
    // FPI64 exceeds native binary64 precision53: source skips rvOK entirely.
    e1 += digits.len() as i32 - k as i32;
    let mut e2 = 0;
    if e1 != 0 {
        let positive = e1 > 0;
        e1 = e1.abs();
        let i = e1 & 15;
        if i != 0 {
            rv = if positive {
                op!(multiply, rv, ten(i))
            } else {
                op!(divide, rv, ten(i))
            }
        }
        e1 >>= 4;
        if e1 != 0 {
            while e1 >= 16 {
                e2 += ((rv >> 52) & 2047) as i32 - 1023;
                rv = (rv & ((1 << 52) - 1)) | ONE;
                rv = op!(multiply, rv, ten(if positive { 256 } else { -256 }));
                e1 -= 16;
            }
            e2 += ((rv >> 52) & 2047) as i32 - 1023;
            rv = (rv & ((1 << 52) - 1)) | ONE;
            let mut j = 0;
            while e1 > 0 {
                if e1 & 1 != 0 {
                    rv = op!(
                        multiply,
                        rv,
                        ten(if positive { 16 << j } else { -(16 << j) })
                    );
                }
                j += 1;
                e1 >>= 1;
            }
        }
    }
    let (m, mut rve, mut rvbits) = unpack(rv);
    let mut m = u128::from(m);
    rve += e2;
    if rvbits > 64 {
        let j = rvbits - 64;
        m >>= j;
        rvbits = 64;
        rve += j
    }
    let mut bb0 = 0;
    let mut rve1 = rve + rvbits - 64;
    let mut denorm = false;
    if rve1 > 16321 {
        return Ok(finish(1, 16321, false, negative, Some(34), control));
    }
    if rve1 < -16445 {
        denorm = true;
        let j = rve + 16445;
        if j > 0 {
            m <<= j;
            rvbits += j
        } else if j < 0 {
            rvbits += j;
            if rvbits <= 0 {
                if rvbits < -1 {
                    return Ok(finish(0, -16445, true, negative, Some(34), control));
                }
                m = 1;
                rvbits = 1
            } else {
                m >>= -j
            }
        }
        rve = -16445;
        rve1 = -16445;
    }
    let coefficient = Integer::decimal(digits);
    let mut inexact = false;
    let mut sudden = false;
    for iteration in 0..128 {
        let bbbits = rvbits - bb0;
        let bbe = rve + bb0;
        let (mut bb2, bb5, mut bd2, bd5) = if e >= 0 { (0, 0, e, e) } else { (-e, -e, 0, 0) };
        if bbe >= 0 {
            bb2 += bbe
        } else {
            bd2 -= bbe
        }
        let mut bs2 = bb2;
        let mut j = 65 - bbbits;
        let i = bbe + bbbits - 64;
        if i < -16445 {
            j += i + 16445
        }
        bb2 += j;
        bd2 += j;
        let common = bb2.min(bd2).min(bs2);
        if common > 0 {
            bb2 -= common;
            bd2 -= common;
            bs2 -= common
        }
        // Remove bb0 before constructing bb, equivalent to source post-pow5 shift.
        let mut bb = Integer::from_u64(u64::try_from(m >> bb0).map_err(|_| Error::Input)?)
            .pow5(bb5 as usize);
        let shift = bb2;
        if shift < 0 {
            return Err(Error::Input);
        }
        bb = bb.shift(shift as usize);
        let bd = coefficient.pow5(bd5 as usize).shift(bd2 as usize);
        let bs = Integer::one().pow5(bb5 as usize).shift(bs2 as usize);
        let (delta, below) = bb.difference(&bd);
        if delta.bits() == 0 {
            break;
        }
        let comparison = delta.cmp(&bs);
        let mut finished = false;
        let mut force_one = false;
        if rd != 0 && !comparison.is_gt() {
            finished = below ^ (rd & 1 != 0);
            inexact = true;
            if !finished {
                break;
            }
            // For nbits64, source checks two complete32-bit words before
            // its partial-word lo0bits mutation; a nonzero significand
            // always jumps directly to adj1 without modifying either word.
            force_one = true;
        }
        if !force_one && comparison.is_lt() {
            inexact = true;
            if below || bbbits > 1 || denorm || rve1 == -16445 {
                break;
            }
            if delta.shift(1).cmp(&bs).is_gt() {
                rve -= 64;
                m = (1 << 64) - 1;
                rvbits = 64;
            }
            break;
        }
        if !force_one && comparison.is_eq() {
            inexact = true;
            if below && denorm && m == (1u128 << rvbits) - 1 {
                m = 1;
                rvbits = 1;
                rve = -16445 + 63;
                denorm = false;
                break;
            }
            if !below && bbbits == 1 {
                if rve1 == -16445 {
                    if m == 1 {
                        sudden = true
                    }
                    break;
                }
                rve -= 64;
                m = (1 << 64) - 1;
                rvbits = 64;
                break;
            }
            if (bbbits < 64 && !denorm) || m & 1 == 0 {
                break;
            }
            if below {
                m += 1;
                if 128 - m.leading_zeros() as i32 > rvbits {
                    rvbits += 1
                }
            } else {
                if bbbits == 1 {
                    m = 0;
                    rve = -16445;
                    break;
                }
                m -= 1
            }
            break;
        }
        let mut adj = if force_one {
            ONE
        } else {
            let (mut a, mut b) = (delta.leading_binary64(), bs.leading_binary64());
            let k = delta.bits() as i32 - bs.bits() as i32;
            if k > 0 {
                a += (k as u64) << 52
            } else {
                b += ((-k) as u64) << 52
            }
            op!(divide, a, b)
        };
        let asub = !below;
        let mut l = 0u32;
        let mut adj0;
        if force_one || adj <= 0x4000000000000000 {
            if !below && denorm && bbbits <= 1 {
                m = 0;
                rve = -16445;
                inexact = true;
                break;
            }
            adj = ONE;
            adj0 = ONE;
        } else {
            adj = op!(multiply, adj, HALF);
            adj0 = adj;
            if adj < integer(2147483647) {
                let t = crate::scalar_fp::truncate_binary64_i32(adj, control)
                    .map_err(|_| Error::Arithmetic(FpError::Control))?;
                control = t.mxcsr;
                l = t.bits;
                adj0 = op!(subtract, adj0, integer(l));
                // This is a nonnegative fractional remainder; exact subtraction
                // under downward rounding can encode -0. C numeric comparisons
                // treat both zeros equally, unlike raw-bit !=0 or ordering.
                let inc = match rd {
                    0 => adj0 >= HALF,
                    1 => asub && adj0 & !(1 << 63) != 0,
                    _ => !asub && adj0 & !(1 << 63) != 0,
                };
                if inc {
                    l += 1
                }
                adj = integer(l);
            }
        }
        let y = rve + rvbits;
        if !denorm && rvbits < 64 {
            let j = 64 - rvbits;
            m <<= j;
            rve -= j;
            rvbits = 64
        }
        let (a, ae, _) = unpack(adj);
        let adjustment = u128::from(if ae < 0 {
            a.checked_shr((-ae) as u32).unwrap_or(0)
        } else {
            a.checked_shl(ae as u32).ok_or(Error::Input)?
        });
        let oldbits = 128 - m.leading_zeros();
        if asub {
            m = m.checked_sub(adjustment).ok_or(Error::Input)?;
            if !denorm && 128 - m.leading_zeros() < oldbits {
                if rve1 == -16445 {
                    rvbits -= 1;
                    denorm = true
                } else {
                    m <<= 1;
                    rve -= 1;
                    rve1 -= 1;
                    l = 0;
                    finished = false
                }
            }
        } else {
            m = m.checked_add(adjustment).ok_or(Error::Input)?;
            if 128 - m.leading_zeros() > oldbits {
                if denorm {
                    rvbits += 1;
                    if rvbits == 64 {
                        denorm = false
                    }
                } else {
                    m >>= 1;
                    rve += 1;
                    rve1 += 1;
                    l = 0
                }
            }
        }
        if finished {
            break;
        }
        if y == rve + rvbits && l != 0 {
            let tol = op!(multiply, adj, 0x3cc203af9ee75616);
            let a = op!(subtract, adj0, HALF); // 5e-16
            let stop = if a >> 63 != 0 && (a & !(1 << 63)) > tol {
                adj0 & !(1 << 63) > tol
            } else if a >> 63 == 0 && a > tol {
                (adj0 & !(1 << 63)) < op!(subtract, ONE, tol)
            } else {
                false
            };
            if stop {
                inexact = true;
                break;
            }
        }
        bb0 = if denorm { 0 } else { m.trailing_zeros() as i32 };
        if iteration == 127 {
            return Err(Error::IterationBudget);
        }
    }
    if !denorm {
        let j = 64 - rvbits;
        if j > 0 {
            m <<= j
        } else if j < 0 {
            m >>= -j
        }
        rve -= j;
    }
    if rve > 16320 {
        if mode == 0 || (mode == 2 && !negative) || (mode == 1 && negative) {
            return Ok(finish(1, 16321, false, negative, Some(34), control));
        }
        return Ok(finish((1 << 64) - 1, 16320, false, negative, None, control));
    }
    let errno = if denorm && (inexact || sudden) {
        Some(34)
    } else {
        None
    };
    if sudden {
        m = 0
    }
    Ok(finish(m, rve, denorm, negative, errno, control))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fields(v: Result80) -> (u64, u16, Option<u32>, u32) {
        (
            u64::from_le_bytes(v.bytes[..8].try_into().unwrap()),
            u16::from_le_bytes(v.bytes[8..].try_into().unwrap()),
            v.errno,
            v.mxcsr,
        )
    }
    #[test]
    fn source_exact_integer_directed_quirk_and_intermediate_precision() {
        for (control, sig, exp) in [
            (0x1f80, u64::MAX, 0x403e),
            (0x3f80, u64::MAX, 0x403e),
            (0x5f80, 1 << 63, 0x403f),
            (0x7f80, u64::MAX, 0x403e),
        ] {
            assert_eq!(
                fields(decimal(b"18446744073709551615", 0, false, control).unwrap()),
                (sig, exp, None, control | 32)
            );
        }
    }
    #[test]
    fn input_and_control_guards() {
        assert_eq!(decimal(b"0", 0, false, 0x1f80), Err(Error::Input));
        assert_eq!(
            decimal(b"1", 0, false, 0x1fc0),
            Err(Error::Arithmetic(FpError::Control))
        );
    }
    #[test]
    fn negative_zero_fraction_is_not_a_positive_adjustment() {
        assert_eq!(
            fields(decimal(b"18446744073709551615", 0, true, 0x3f80).unwrap()),
            (u64::MAX, 0xc03e, None, 0x3fa0)
        );
        assert_eq!(
            fields(decimal(b"1", 23, true, 0x3f80).unwrap()),
            (0xa968163f0a57b400, 0xc04b, None, 0x3fa0)
        );
    }
}
