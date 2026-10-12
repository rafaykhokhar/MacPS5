//! Source-ordered FreeBSD 11.4 gdtoa decimal approximation and refinement.
//! Derived from David M. Gay's gdtoa; see GDTOA-LICENSE.
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/contrib/gdtoa/strtod.c
use super::decimal_arithmetic::{self as fp, Error};
use super::{parse, Rounding};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Refinement,
    Overflow,
    Underflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Approximation {
    pub bits: u64,
    pub mxcsr: u32,
    pub scale: i32,
    pub boundary: Boundary,
}
fn integer(v: u32) -> u64 {
    if v == 0 {
        return 0;
    }
    let top = 31 - v.leading_zeros();
    ((u64::from(top) + 1023) << 52) | ((u64::from(v) << (52 - top)) & ((1 << 52) - 1))
}
fn ten(e: i32) -> u64 {
    // These are compile-time gdtoa table values, rounded nearest irrespective of
    // guest RC. Integer conversion here avoids any host floating-point operation.
    parse(format!("1e{e}").as_bytes(), Rounding::NearestEven)
        .expect("bounded constant")
        .bits
}
/// `digits` is the significant decimal coefficient after gdtoa's lexical zero
/// trimming; `exponent` satisfies exact value = coefficient * 10^exponent.
/// Caller has already excluded the nd<=15 fast paths.
pub fn approximate(digits: &[u8], exponent: i32, mxcsr: u32) -> Result<Approximation, Error> {
    assert!(
        !digits.is_empty()
            && digits.len() <= 4096
            && digits[0] != b'0'
            && digits.iter().all(u8::is_ascii_digit)
    );
    let k = digits.len().min(16);
    let mut y = 0;
    let mut z = 0;
    for (i, b) in digits[..k].iter().enumerate() {
        if i < 9 {
            y = y * 10 + u32::from(b - b'0')
        } else {
            z = z * 10 + u32::from(b - b'0')
        }
    }
    let mut rv = integer(y);
    let mut control = mxcsr;
    macro_rules! op {
        ($name:ident,$a:expr,$b:expr) => {{
            let r = fp::$name($a, $b, control)?;
            control = r.mxcsr;
            r.bits
        }};
    }
    if k > 9 {
        rv = op!(multiply, ten((k - 9) as i32), rv);
        rv = op!(add, rv, integer(z));
    }
    let mut e1 = exponent + (digits.len() - k) as i32;
    let mut scale = 0;
    macro_rules! finish {
        ($b:ident) => {
            return Ok(Approximation {
                bits: rv,
                mxcsr: control,
                scale,
                boundary: Boundary::$b,
            })
        };
    }
    if e1 > 0 {
        let i = e1 & 15;
        if i != 0 {
            rv = op!(multiply, rv, ten(i));
        }
        e1 &= !15;
        if e1 != 0 {
            if e1 > 308 {
                finish!(Overflow);
            }
            e1 >>= 4;
            let mut j = 0;
            while e1 > 1 {
                if e1 & 1 != 0 {
                    rv = op!(multiply, rv, ten(16 << j));
                }
                j += 1;
                e1 >>= 1;
            }
            // Exact exponent-field adjustment used to keep final multiplication finite.
            rv -= 53u64 << 52;
            rv = op!(multiply, rv, ten(16 << j));
            let z = rv & 0x7ff0_0000_0000_0000;
            if z > ((1024 + 1023 - 53) as u64) << 52 {
                finish!(Overflow);
            }
            if z > ((1024 + 1023 - 1 - 53) as u64) << 52 {
                rv = 0x7fef_ffff_ffff_ffff;
            } else {
                rv += 53u64 << 52;
            }
        }
    } else if e1 < 0 {
        e1 = -e1;
        let i = e1 & 15;
        if i != 0 {
            rv = op!(divide, rv, ten(i));
        }
        e1 >>= 4;
        if e1 != 0 {
            if e1 >= 32 {
                rv = 0;
                finish!(Underflow);
            }
            if e1 & 16 != 0 {
                scale = 106;
            }
            let mut j = 0;
            while e1 > 0 {
                if e1 & 1 != 0 {
                    let mut factor = ten(-(16 << j));
                    if j == 4 {
                        factor += 106u64 << 52;
                    }
                    rv = op!(multiply, rv, factor);
                }
                j += 1;
                e1 >>= 1;
            }
            let j = 107 - ((rv >> 52) & 2047) as i32;
            if scale != 0 && j > 0 {
                if j >= 32 {
                    rv &= 0xffff_ffff_0000_0000;
                    if j >= 53 {
                        rv = 55u64 << 52;
                    } else {
                        rv &= u64::from(u32::MAX << ((j - 32) as u32)) << 32;
                    }
                } else {
                    rv &= !(u64::from((1u32 << j) - 1));
                }
            }
            if rv == 0 {
                finish!(Underflow);
            }
        }
    }
    finish!(Refinement);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefinementError {
    Arithmetic(Error),
    NearestPending,
    IterationBudget,
}
impl From<Error> for RefinementError {
    fn from(e: Error) -> Self {
        Self::Arithmetic(e)
    }
}
fn ulp(rv: u64) -> u64 {
    let e = ((rv >> 52) & 2047) as i32 - 52;
    if e > 0 {
        (e as u64) << 52
    } else {
        1u64 << ((51 + e).max(0) as u32)
    }
}
/// Source-ordered directed-rounding adjustment loop; nearest-even uses its
/// separate adjustment branch below.
pub fn directed(
    digits: &[u8],
    exponent: i32,
    negative: bool,
    mxcsr: u32,
) -> Result<Approximation, RefinementError> {
    use super::decimal_big::{delta, Integer};
    let mode = (mxcsr >> 13) & 3;
    if mode == 0 {
        return Err(RefinementError::NearestPending);
    }
    let outward = (mode == 1 && negative) || (mode == 2 && !negative);
    let start = approximate(digits, exponent, mxcsr)?;
    let (mut rv, mut control, scale) = (start.bits, start.mxcsr, start.scale);
    let sign = if negative { 1u64 << 63 } else { 0 };
    macro_rules! overflow {
        () => {
            return Ok(Approximation {
                bits: sign
                    | if outward {
                        0x7ff0_0000_0000_0000
                    } else {
                        0x7fef_ffff_ffff_ffff
                    },
                mxcsr: control,
                scale,
                boundary: Boundary::Overflow,
            })
        };
    }
    macro_rules! op {
        ($name:ident,$a:expr,$b:expr) => {{
            let r = fp::$name($a, $b, control)?;
            control = r.mxcsr;
            r.bits
        }};
    }
    if start.boundary == Boundary::Overflow {
        overflow!();
    }
    if start.boundary == Boundary::Underflow {
        return Ok(Approximation {
            bits: sign,
            ..start
        });
    }
    let coefficient = Integer::decimal(digits);
    for _ in 0..128 {
        let d = delta(&coefficient, exponent, rv, scale, true);
        if d.delta.cmp(&d.unit).is_lt() {
            if d.delta.bits() != 0 && ((outward && d.below) || (!outward && !d.below)) {
                let mut adj = 0x3ff0_0000_0000_0000;
                if !outward {
                    adj |= 1 << 63;
                    if rv & ((1 << 52) - 1) == 0
                        && (scale == 0 || ((rv >> 52) & 2047) > 106)
                        && !d.delta.shift(1).cmp(&d.unit).is_gt()
                    {
                        adj = 0xbfe0_0000_0000_0000;
                    }
                }
                let enc = ((rv >> 52) & 2047) as i32;
                if scale != 0 && enc <= 106 {
                    adj += ((107 - enc) as u64) << 52;
                }
                let step = op!(multiply, adj, ulp(rv));
                rv = op!(add, rv, step);
            }
            if scale != 0 {
                rv = op!(multiply, rv, ((1023 - 106) as u64) << 52);
            }
            return Ok(Approximation {
                bits: rv ^ sign,
                mxcsr: control,
                scale,
                boundary: Boundary::Refinement,
            });
        }
        let (mut numerator, mut denominator) =
            (d.delta.leading_binary64(), d.unit.leading_binary64());
        let k = d.delta.bits() as i32 - d.unit.bits() as i32;
        if k > 0 {
            numerator += (k as u64) << 52;
        } else {
            denominator += ((-k) as u64) << 52;
        }
        let mut adj = op!(divide, numerator, denominator);
        if adj < 0x3ff0_0000_0000_0000 {
            adj = 0x3ff0_0000_0000_0000;
        }
        // 0x7ffffffe is exactly representable; source truncation sets precision.
        if adj <= integer(0x7ffffffe) {
            let trunc = crate::scalar_fp::truncate_binary64_i32(adj, control)
                .map_err(|_| RefinementError::Arithmetic(Error::Control))?;
            control = trunc.mxcsr;
            let mut y = trunc.bits;
            if integer(y) != adj {
                if outward == d.below {
                    y += 1;
                }
                adj = integer(y);
            }
        }
        let enc = ((rv >> 52) & 2047) as i32;
        if scale != 0 && enc <= 106 {
            adj += ((107 - enc) as u64) << 52;
        }
        adj = op!(multiply, adj, ulp(rv));
        if d.below {
            if rv == 0x7fef_ffff_ffff_ffff {
                overflow!();
            }
            rv = op!(add, rv, adj);
        } else {
            rv = op!(subtract, rv, adj);
        }
    }
    Err(RefinementError::IterationBudget)
}

/// Nearest-even gdtoa refinement, retaining every source arithmetic operation
/// that can change SSE status. Integer boundary manipulations set no flags.
pub fn nearest(
    digits: &[u8],
    exponent: i32,
    negative: bool,
    mxcsr: u32,
) -> Result<Approximation, RefinementError> {
    use super::decimal_big::{delta, Integer};
    if (mxcsr >> 13) & 3 != 0 {
        return Err(RefinementError::NearestPending);
    }
    let start = approximate(digits, exponent, mxcsr)?;
    let (mut rv, mut control, scale) = (start.bits, start.mxcsr, start.scale);
    let sign = if negative { 1u64 << 63 } else { 0 };
    macro_rules! op {
        ($name:ident,$a:expr,$b:expr) => {{
            let r = fp::$name($a, $b, control)?;
            control = r.mxcsr;
            r.bits
        }};
    }
    macro_rules! overflow {
        () => {
            return Ok(Approximation {
                bits: sign | 0x7ff0_0000_0000_0000,
                mxcsr: control,
                scale,
                boundary: Boundary::Overflow,
            })
        };
    }
    macro_rules! underflow {
        () => {
            return Ok(Approximation {
                bits: sign,
                mxcsr: control,
                scale,
                boundary: Boundary::Underflow,
            })
        };
    }
    if start.boundary == Boundary::Overflow {
        overflow!();
    }
    if start.boundary == Boundary::Underflow {
        underflow!();
    }
    let coefficient = Integer::decimal(digits);
    let mut done = false;
    const FRACTION: u64 = (1 << 52) - 1;
    for _ in 0..128 {
        let d = delta(&coefficient, exponent, rv, scale, false);
        let cmp = d.delta.cmp(&d.unit);
        let enc = ((rv >> 52) & 2047) as i32;
        if cmp.is_lt() {
            if d.below
                || rv & FRACTION != 0
                || enc <= 107
                || d.delta.bits() == 0
                || !d.delta.shift(1).cmp(&d.unit).is_gt()
            {
                done = true;
                break;
            }
            // drop_down for the narrow lower interval next to an exact power of two.
            if scale != 0 && enc <= 107 {
                if enc > 55 {
                    done = true;
                    break;
                }
                underflow!();
            }
            rv = (((enc - 1) as u64) << 52) | FRACTION;
            done = true;
            break;
        }
        if cmp.is_eq() {
            let boundary_low = if scale != 0 && enc <= 106 {
                u32::MAX.checked_shl((107 - enc) as u32).unwrap_or(0)
            } else {
                u32::MAX
            };
            if d.below && (rv >> 32) & 0xfffff == 0xfffff && rv as u32 == boundary_low {
                if rv == 0x7fef_ffff_ffff_ffff {
                    overflow!();
                }
                rv = ((enc + 1) as u64) << 52;
                done = true;
                break;
            }
            if !d.below && rv & FRACTION == 0 {
                if scale != 0 && enc <= 107 {
                    if enc > 55 {
                        done = true;
                        break;
                    }
                    underflow!();
                }
                rv = (((enc - 1) as u64) << 52) | FRACTION;
                done = true;
                break;
            }
            let odd = if d.lsb >> 32 != 0 {
                rv & (d.lsb & 0xffff_ffff_0000_0000) != 0
            } else {
                rv & d.lsb != 0
            };
            if !odd {
                done = true;
                break;
            }
            let mut step = ulp(rv);
            if scale != 0 && 107 - enc > 0 {
                step = op!(multiply, step, ((1023 + 107 - enc) as u64) << 52);
            }
            if d.below {
                rv = op!(add, rv, step);
            } else {
                rv = op!(subtract, rv, step);
                if rv & !(1 << 63) == 0 {
                    underflow!();
                }
            }
            done = true;
            break;
        }
        let (mut numerator, mut denominator) =
            (d.delta.leading_binary64(), d.unit.leading_binary64());
        let k = d.delta.bits() as i32 - d.unit.bits() as i32;
        if k > 0 {
            numerator += (k as u64) << 52;
        } else {
            denominator += ((-k) as u64) << 52;
        }
        let mut aadj = op!(divide, numerator, denominator);
        let mut adj;
        if aadj <= 0x4000_0000_0000_0000 {
            if d.below {
                aadj = 0x3ff0_0000_0000_0000;
                adj = aadj;
            } else if rv & FRACTION != 0 {
                if rv == 1 {
                    underflow!();
                }
                aadj = 0x3ff0_0000_0000_0000;
                adj = aadj | (1 << 63);
            } else {
                if aadj < 0x3ff0_0000_0000_0000 {
                    aadj = 0x3fe0_0000_0000_0000;
                } else {
                    aadj = op!(multiply, aadj, 0x3fe0_0000_0000_0000);
                }
                adj = aadj | (1 << 63);
            }
        } else {
            aadj = op!(multiply, aadj, 0x3fe0_0000_0000_0000);
            adj = aadj | if d.below { 0 } else { 1 << 63 };
        }
        if enc == 2046 {
            let old = rv;
            rv -= 53u64 << 52;
            adj = op!(multiply, adj, ulp(rv));
            rv = op!(add, rv, adj);
            if ((rv >> 52) & 2047) >= 1994 {
                if old == 0x7fef_ffff_ffff_ffff {
                    overflow!();
                }
                rv = 0x7fef_ffff_ffff_ffff;
                continue;
            }
            rv += 53u64 << 52;
        } else {
            if scale != 0 && enc <= 106 {
                if aadj <= integer(0x7fffffff) {
                    let t = crate::scalar_fp::truncate_binary64_i32(aadj, control)
                        .map_err(|_| RefinementError::Arithmetic(Error::Control))?;
                    control = t.mxcsr;
                    let z = t.bits.max(1);
                    aadj = integer(z);
                    adj = aadj | if d.below { 0 } else { 1 << 63 };
                }
                adj += ((107 - enc) as u64) << 52;
            }
            adj = op!(multiply, adj, ulp(rv));
            rv = op!(add, rv, adj);
        }
        if scale == 0 && enc == ((rv >> 52) & 2047) as i32 {
            let t = crate::scalar_fp::truncate_binary64_i32(aadj, control)
                .map_err(|_| RefinementError::Arithmetic(Error::Control))?;
            control = t.mxcsr;
            let fraction = op!(subtract, aadj, integer(t.bits));
            let low = parse(b"0.4999999", Rounding::NearestEven)
                .expect("constant")
                .bits;
            let high = parse(b"0.5000001", Rounding::NearestEven)
                .expect("constant")
                .bits;
            if d.below || rv & FRACTION != 0 {
                if fraction < low || fraction > high {
                    done = true;
                    break;
                }
            } else if fraction
                < parse(b"0.24999995", Rounding::NearestEven)
                    .expect("constant")
                    .bits
            {
                done = true;
                break;
            }
        }
    }
    if !done {
        return Err(RefinementError::IterationBudget);
    }
    if scale != 0 {
        rv = op!(multiply, rv, ((1023 - 106) as u64) << 52);
    }
    Ok(Approximation {
        bits: rv ^ sign,
        mxcsr: control,
        scale,
        boundary: Boundary::Refinement,
    })
}
