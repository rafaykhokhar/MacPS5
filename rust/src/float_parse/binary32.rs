//! Direct integer-rational binary32 conversion, avoiding binary64 double rounding.
//! Pure mathematical model; strtof errno/intermediate effects are separate.
use super::{parse, Big, Error, Range, Rounding};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed32 {
    pub bits: u32,
    pub end: usize,
    pub range: Option<Range>,
    pub inexact: bool,
}
fn outward(r: Rounding, negative: bool) -> bool {
    matches!(
        (r, negative),
        (Rounding::Up, false) | (Rounding::Down, true)
    )
}
fn extreme(negative: bool, end: usize, r: Rounding, overflow: bool) -> Parsed32 {
    let bits = if overflow {
        if r == Rounding::NearestEven || outward(r, negative) {
            0x7f800000
        } else {
            0x7f7fffff
        }
    } else {
        u32::from(outward(r, negative))
    };
    Parsed32 {
        bits: bits | if negative { 1 << 31 } else { 0 },
        end,
        range: Some(if overflow {
            Range::Overflow
        } else {
            Range::Underflow
        }),
        inexact: true,
    }
}
fn round(n: Big, d: Big, scale: i32, negative: bool, end: usize, r: Rounding) -> Parsed32 {
    let sign = if negative { 1 << 31 } else { 0 };
    if n.bits() == 0 {
        return Parsed32 {
            bits: sign,
            end,
            range: None,
            inexact: false,
        };
    }
    let guess = n.bits() as i32 - d.bits() as i32;
    let less = if guess >= 0 {
        n.cmp(&d.shl(guess as usize)).is_lt()
    } else {
        n.shl((-guess) as usize).cmp(&d).is_lt()
    };
    let exponent = guess - i32::from(less) + scale;
    if exponent > 127 {
        return extreme(negative, end, r, true);
    }
    if exponent < -150 {
        return extreme(negative, end, r, false);
    }
    let unit = (exponent - 23).max(-149);
    let shift = scale - unit;
    let (n, d) = if shift >= 0 {
        (n.shl(shift as usize), d)
    } else {
        (n, d.shl((-shift) as usize))
    };
    let (mut q, rem) = n.divide_small_quotient(&d);
    let inexact = rem.bits() != 0;
    let inc = inexact
        && if r == Rounding::NearestEven {
            let cmp = rem.shl(1).cmp(&d);
            cmp.is_gt() || (cmp.is_eq() && q & 1 != 0)
        } else {
            outward(r, negative)
        };
    q += u64::from(inc);
    let mut e = exponent;
    if q == 1 << 24 {
        q >>= 1;
        e += 1;
    }
    if e > 127 {
        return extreme(negative, end, r, true);
    }
    let bits = if exponent < -126 {
        q as u32
    } else {
        ((e + 127) as u32) << 23 | ((q as u32) & 0x7fffff)
    };
    Parsed32 {
        bits: bits | sign,
        end,
        range: if bits < 1 << 23 && inexact {
            Some(Range::Underflow)
        } else {
            None
        },
        inexact,
    }
}
/// Shares the bounded lexical contract of binary64 parsing, then rounds the
/// exact coefficient directly to24 bits. Binary64 result bits are used only
/// for spelling-independent infinity/NaN payload construction, never numerics.
pub fn parse_binary32(input: &[u8], rounding: Rounding) -> Result<Parsed32, Error> {
    let syntax = parse(input, rounding)?;
    if syntax.end == 0 {
        return Ok(Parsed32 {
            bits: 0,
            end: 0,
            range: None,
            inexact: false,
        });
    }
    let mut s = &input[..syntax.end];
    while s.first().is_some_and(|b| matches!(*b, 9..=13 | 32)) {
        s = &s[1..];
    }
    let negative = s.first() == Some(&b'-');
    if matches!(s.first(), Some(b'+' | b'-')) {
        s = &s[1..];
    }
    let sign = if negative { 1 << 31 } else { 0 };
    if s[0].eq_ignore_ascii_case(&b'i') {
        return Ok(Parsed32 {
            bits: sign | 0x7f800000,
            end: syntax.end,
            range: None,
            inexact: false,
        });
    }
    if s[0].eq_ignore_ascii_case(&b'n') {
        return Ok(Parsed32 {
            bits: sign | 0x7fc00000 | (syntax.bits as u32 & 0x7fffff),
            end: syntax.end,
            range: None,
            inexact: false,
        });
    }
    let hex = s.first() == Some(&b'0') && matches!(s.get(1), Some(b'x' | b'X'));
    let radix = if hex { 16 } else { 10 };
    let mut at = if hex { 2 } else { 0 };
    let mut n = Big::new(0);
    let (mut fraction, mut point, mut significant) = (0i32, false, 0i32);
    while at < s.len() {
        let c = s[at];
        if c == b'.' && !point {
            point = true;
            at += 1;
            continue;
        }
        let d = match c {
            b'0'..=b'9' => (c - b'0') as u32,
            b'a'..=b'f' if hex => (c - b'a' + 10) as u32,
            b'A'..=b'F' if hex => (c - b'A' + 10) as u32,
            _ => break,
        };
        if point {
            fraction += 1;
        }
        n.small(radix, d);
        if n.bits() != 0 {
            significant += 1;
        }
        at += 1;
    }
    let mut exponent = 0i32;
    if at < s.len() {
        at += 1;
        let minus = s.get(at) == Some(&b'-');
        if matches!(s.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while let Some(c) = s.get(at).filter(|b| b.is_ascii_digit()) {
            exponent = (exponent.saturating_mul(10) + i32::from(c - b'0')).min(1_000_000);
            at += 1;
        }
        if minus {
            exponent = -exponent;
        }
    }
    if n.bits() == 0 {
        return Ok(Parsed32 {
            bits: sign,
            end: syntax.end,
            range: None,
            inexact: false,
        });
    }
    if hex {
        return Ok(round(
            n,
            Big::new(1),
            exponent - fraction * 4,
            negative,
            syntax.end,
            rounding,
        ));
    }
    let power = exponent - fraction;
    let decimal = significant + power - 1;
    if decimal > 39 {
        return Ok(extreme(negative, syntax.end, rounding, true));
    }
    if decimal < -47 {
        return Ok(extreme(negative, syntax.end, rounding, false));
    }
    let mut d = Big::new(1);
    if power >= 0 {
        for _ in 0..power {
            n.small(5, 0);
        }
    } else {
        for _ in 0..-power {
            d.small(5, 0);
        }
    }
    Ok(round(n, d, power, negative, syntax.end, rounding))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_halfway_and_double_rounding_counterexample() {
        let p = parse_binary32(
            b"1.0000000596046447753906250000000000000001",
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(p.bits, 0x3f800001);
        assert_eq!(
            parse_binary32(b"1.000000059604644775390625", Rounding::NearestEven)
                .unwrap()
                .bits,
            0x3f800000
        );
        assert_eq!(
            parse_binary32(b"0x1p-149", Rounding::NearestEven)
                .unwrap()
                .bits,
            1
        );
        assert_eq!(parse_binary32(b"0x1p-150", Rounding::Up).unwrap().bits, 1);
    }
}
