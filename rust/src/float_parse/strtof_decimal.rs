//! Source-ordered 24-bit strtodg decimal conversion.
//! Derived from David M. Gay's FreeBSD 11.4 gdtoa strtodg.c; see GDTOA-LICENSE.
use super::decimal_arithmetic::{self as fp, Error as FpError};
use super::decimal_big::Integer;
use super::{parse, Rounding};
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
pub struct Result32 {
    pub bits: u32,
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
    m: u64,
    e: i32,
    denorm: bool,
    negative: bool,
    errno: Option<u32>,
    mxcsr: u32,
) -> Result32 {
    let bits = if m == 0 {
        0
    } else if e > 104 {
        0x7f800000
    } else if denorm {
        m as u32
    } else {
        ((m as u32) & 0x7fffff) | (((e + 150) as u32) << 23)
    };
    Result32 {
        bits: bits | if negative { 1 << 31 } else { 0 },
        errno,
        mxcsr,
    }
}
/// rvOK executes only integer operations. Preserve its source overflow/underflow
/// policy instead of replacing it with mathematical directed overflow handling.
fn rv_ok(v: u64, exact: bool, rd: u32, negative: bool, mxcsr: u32) -> Option<Result32> {
    let (mut b, mut e, nb) = unpack(v);
    let dif = nb - 24;
    e += dif;
    let mut carry = false;
    if dif <= 0 {
        if !exact {
            return None;
        }
    } else {
        match rd {
            1 => {}
            2 => carry = true,
            _ => {
                let k = dif - 1;
                if k == 0 {
                    if !exact {
                        return None;
                    }
                    carry = b & 2 != 0
                } else if k > 0 {
                    carry = b & (1 << k) != 0
                }
            }
        }
    }
    let mut lost = false;
    if dif > 0 {
        lost = b & ((1 << dif) - 1) != 0;
        b >>= dif;
        if carry {
            b += 1;
            if b >> 24 != 0 {
                if !lost {
                    lost = b & 1 != 0
                }
                b >>= 1;
                e += 1
            }
        }
    } else if dif < 0 {
        b <<= -dif
    }
    let mut denorm = false;
    if e < -149 {
        let k = -149 - e;
        e = -149;
        if k > 24 {
            b = 0
        } else {
            let k1 = k - 1;
            if k1 > 0 && !lost {
                lost = b & ((1 << k1) - 1) != 0
            }
            if !lost && !exact {
                return None;
            }
            carry = b & (1 << k1) != 0;
            b >>= k;
            denorm = true;
            if carry {
                b += 1
            }
        }
    }
    Some(finish(
        b,
        e,
        denorm,
        negative,
        if e > 104 { Some(34) } else { None },
        mxcsr,
    ))
}
/// `digits` is gdtoa's trimmed nonzero coefficient, exact value digits*10^e.
/// Limits match the existing lexical model; arithmetic iteration is bounded.
pub fn decimal(digits: &[u8], e: i32, negative: bool, mut control: u32) -> Result<Result32, Error> {
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
    if digits.len() <= 15 {
        if e == 0 {
            if let Some(r) = rv_ok(rv, true, rd, negative, control) {
                return Ok(r);
            }
        } else if e > 0 {
            if e <= 22 {
                let five = [
                    0, 3, 5, 7, 10, 12, 14, 17, 19, 21, 24, 26, 28, 31, 33, 35, 38, 40, 42, 45, 47,
                    49, 52,
                ];
                let exact = five[e as usize] + unpack(rv).2 <= 53;
                rv = op!(multiply, rv, ten(e));
                if let Some(r) = rv_ok(rv, exact, rd, negative, control) {
                    return Ok(r);
                }
                e1 -= e;
            } else {
                let i = 15 - digits.len() as i32;
                if e <= 22 + i {
                    e1 -= i;
                    rv = op!(multiply, rv, ten(i));
                    rv = op!(multiply, rv, ten(e - i));
                    if let Some(r) = rv_ok(rv, false, rd, negative, control) {
                        return Ok(r);
                    }
                    e1 -= e - i;
                }
            }
        } else if e >= -22 {
            rv = op!(divide, rv, ten(-e));
            if let Some(r) = rv_ok(rv, false, rd, negative, control) {
                return Ok(r);
            }
            e1 -= e;
        }
    }
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
    let (mut m, mut rve, mut rvbits) = unpack(rv);
    rve += e2;
    if rvbits > 24 {
        let j = rvbits - 24;
        m >>= j;
        rvbits = 24;
        rve += j
    }
    let mut bb0 = 0;
    let mut rve1 = rve + rvbits - 24;
    let mut denorm = false;
    if rve1 > 105 {
        return Ok(finish(1, 105, false, negative, Some(34), control));
    }
    if rve1 < -149 {
        denorm = true;
        let j = rve + 149;
        if j > 0 {
            m <<= j;
            rvbits += j
        } else if j < 0 {
            rvbits += j;
            if rvbits <= 0 {
                if rvbits < -1 {
                    return Ok(finish(0, -149, true, negative, Some(34), control));
                }
                m = 1;
                rvbits = 1
            } else {
                m >>= -j
            }
        }
        rve = -149;
        rve1 = -149;
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
        let mut j = 25 - bbbits;
        let i = bbe + bbbits - 24;
        if i < -149 {
            j += i + 149
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
        let mut bb = Integer::from_u64(m >> bb0).pow5(bb5 as usize);
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
            if !below && rve1 != -149 {
                // Source lo0bits(rvb->x) updates its word in place, including
                // the path that subsequently jumps to adj1. Do not replace
                // this with a read-only power-of-two predicate.
                let trailing = m.trailing_zeros();
                m >>= trailing;
                if trailing >= 23 {
                    rve = rve1 - 1;
                    m = (1 << 24) - 1;
                    rvbits = 24;
                    break;
                }
            }
            force_one = true;
        }
        if !force_one && comparison.is_lt() {
            inexact = true;
            if below || bbbits > 1 || denorm || rve1 == -149 {
                break;
            }
            if delta.shift(1).cmp(&bs).is_gt() {
                rve -= 24;
                m = (1 << 24) - 1;
                rvbits = 24;
            }
            break;
        }
        if !force_one && comparison.is_eq() {
            inexact = true;
            if below && denorm && m == (1u64 << rvbits) - 1 {
                m = 1;
                rvbits = 1;
                rve = -149 + 23;
                denorm = false;
                break;
            }
            if !below && bbbits == 1 {
                if rve1 == -149 {
                    if m == 1 {
                        sudden = true
                    }
                    break;
                }
                rve -= 24;
                m = (1 << 24) - 1;
                rvbits = 24;
                break;
            }
            if (bbbits < 24 && !denorm) || m & 1 == 0 {
                break;
            }
            if below {
                m += 1;
                if 64 - m.leading_zeros() as i32 > rvbits {
                    rvbits += 1
                }
            } else {
                if bbbits == 1 {
                    m = 0;
                    rve = -149;
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
                rve = -149;
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
                let inc = match rd {
                    0 => adj0 >= HALF,
                    1 => asub && adj0 != 0,
                    _ => !asub && adj0 != 0,
                };
                if inc {
                    l += 1
                }
                adj = integer(l);
            }
        }
        let y = rve + rvbits;
        if !denorm && rvbits < 24 {
            let j = 24 - rvbits;
            m <<= j;
            rve -= j;
            rvbits = 24
        }
        let (a, ae, _) = unpack(adj);
        let adjustment = if ae < 0 {
            a.checked_shr((-ae) as u32).unwrap_or(0)
        } else {
            a.checked_shl(ae as u32).ok_or(Error::Input)?
        };
        let oldbits = 64 - m.leading_zeros();
        if asub {
            m = m.checked_sub(adjustment).ok_or(Error::Input)?;
            if !denorm && 64 - m.leading_zeros() < oldbits {
                if rve1 == -149 {
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
            if 64 - m.leading_zeros() > oldbits {
                if denorm {
                    rvbits += 1;
                    if rvbits == 24 {
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
                adj0 > tol
            } else if a >> 63 == 0 && a > tol {
                adj0 < op!(subtract, ONE, tol)
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
        let j = 24 - rvbits;
        if j > 0 {
            m <<= j
        } else if j < 0 {
            m >>= -j
        }
        rve -= j;
    }
    if rve > 104 {
        if mode == 0 || (mode == 2 && !negative) || (mode == 1 && negative) {
            return Ok(finish(1, 105, false, negative, Some(34), control));
        }
        return Ok(finish((1 << 24) - 1, 104, false, negative, None, control));
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
    #[test]
    fn source_directed_mutation_and_overflow_paths() {
        let got = decimal(b"1", -35, false, 0x3f80).unwrap();
        assert_eq!((got.bits, got.errno, got.mxcsr), (0x056a5696, None, 0x3fa0));
        let early = decimal(b"1", 40, false, 0x3f80).unwrap();
        let refined =
            decimal(b"340282400000000000000000000000000000000", 0, false, 0x3f80).unwrap();
        assert_eq!((early.bits, early.errno), (0x7f800000, Some(34)));
        assert_eq!((refined.bits, refined.errno), (0x7f7fffff, None));
    }
    #[test]
    fn exact_subnormal_has_intermediate_precision_without_erange() {
        let got = decimal(b"140129846432481707092372958328991613128026194187651577175706828388979108268586060148663818836212158203125", -149, false, 0x1f80).unwrap();
        assert_eq!((got.bits, got.errno, got.mxcsr), (1, None, 0x1fa0));
        assert_eq!(decimal(b"0", 0, false, 0x1f80), Err(Error::Input));
        assert_eq!(
            decimal(b"1", 0, false, 0x1fc0),
            Err(Error::Arithmetic(FpError::Control))
        );
    }
}
