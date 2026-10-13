//! Pure integer-rational conversion to canonical x87 extended80 (64-bit
//! significand, explicit integer bit, 15-bit exponent). This is NOT strtold:
//! mathematical range/inexact are not libc errno or intermediate FP effects.
//! NaN payload syntax is deliberately unsupported pending target-source audit.
//! Payload is10 little-endian bytes; ABI object padding is not part of it.
use super::{Big, Error as ParseError, Range, Rounding, parse};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed80 {
    pub bytes: [u8; 10],
    pub end: usize,
    pub range: Option<Range>,
    pub inexact: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
    NanPayload,
    QuotientBounds,
}
fn pack(
    significand: u64,
    exponent: u16,
    negative: bool,
    end: usize,
    range: Option<Range>,
    inexact: bool,
) -> Parsed80 {
    let mut bytes = [0; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&(exponent | if negative { 0x8000 } else { 0 }).to_le_bytes());
    Parsed80 {
        bytes,
        end,
        range,
        inexact,
    }
}
fn outward(r: Rounding, negative: bool) -> bool {
    matches!(
        (r, negative),
        (Rounding::Up, false) | (Rounding::Down, true)
    )
}
fn extreme(negative: bool, end: usize, r: Rounding, overflow: bool) -> Parsed80 {
    if overflow {
        let infinity = r == Rounding::NearestEven || outward(r, negative);
        pack(
            if infinity { 1 << 63 } else { u64::MAX },
            if infinity { 0x7fff } else { 0x7ffe },
            negative,
            end,
            Some(Range::Overflow),
            true,
        )
    } else {
        pack(
            u64::from(outward(r, negative)),
            0,
            negative,
            end,
            Some(Range::Underflow),
            true,
        )
    }
}
// A64-bit quotient can have numerator/denominator bit-length difference64
// while still fitting u64. Use a u128 accumulator instead of Big's53-bit-use
// divide_small_quotient precondition, and explicitly reject invalid bounds.
fn quotient(mut n: Big, d: &Big) -> Result<(u64, Big), Error> {
    if n.cmp(d).is_lt() {
        return Ok((0, n));
    }
    let shift = n.bits() - d.bits();
    if shift > 64 {
        return Err(Error::QuotientBounds);
    }
    let mut q = 0u128;
    for i in (0..=shift).rev() {
        let part = d.shl(i);
        if !n.cmp(&part).is_lt() {
            n.sub(&part);
            q |= 1u128 << i;
        }
    }
    Ok((u64::try_from(q).map_err(|_| Error::QuotientBounds)?, n))
}
fn round(
    n: Big,
    d: Big,
    scale: i32,
    negative: bool,
    end: usize,
    r: Rounding,
) -> Result<Parsed80, Error> {
    if n.bits() == 0 {
        return Ok(pack(0, 0, negative, end, None, false));
    }
    let guess = n.bits() as i32 - d.bits() as i32;
    let less = if guess >= 0 {
        n.cmp(&d.shl(guess as usize)).is_lt()
    } else {
        n.shl((-guess) as usize).cmp(&d).is_lt()
    };
    let mut exponent = guess - i32::from(less) + scale;
    if exponent > 16383 {
        return Ok(extreme(negative, end, r, true));
    }
    if exponent < -16446 {
        return Ok(extreme(negative, end, r, false));
    }
    let unit = (exponent - 63).max(-16445);
    let shift = scale - unit;
    let (n, d) = if shift >= 0 {
        (n.shl(shift as usize), d)
    } else {
        (n, d.shl((-shift) as usize))
    };
    let (mut q, remainder) = quotient(n, &d)?;
    let inexact = remainder.bits() != 0;
    let increment = if !inexact {
        false
    } else if r == Rounding::NearestEven {
        let cmp = remainder.shl(1).cmp(&d);
        cmp.is_gt() || (cmp.is_eq() && q & 1 != 0)
    } else {
        outward(r, negative)
    };
    let carry = increment && q == u64::MAX;
    if carry {
        q = 1 << 63;
        exponent += 1;
    } else {
        q += u64::from(increment);
    }
    if exponent > 16383 {
        return Ok(extreme(negative, end, r, true));
    }
    let field = if q >= 1 << 63 {
        (exponent.max(-16382) + 16383) as u16
    } else {
        0
    };
    // Tininess-after-rounding mathematical classification only, not source errno.
    let range = if field == 0 && inexact {
        Some(Range::Underflow)
    } else {
        None
    };
    Ok(pack(q, field, negative, end, range, inexact))
}
pub fn parse_extended80(input: &[u8], r: Rounding) -> Result<Parsed80, Error> {
    // Reuse only the bounded lexical end; never cast its binary64 numeric value.
    let syntax = parse(input, r).map_err(Error::Parse)?;
    if syntax.end == 0 {
        return Ok(pack(0, 0, false, 0, None, false));
    }
    let mut s = &input[..syntax.end];
    while s.first().is_some_and(|b| matches!(*b, 9..=13 | 32)) {
        s = &s[1..];
    }
    let negative = s.first() == Some(&b'-');
    if matches!(s.first(), Some(b'+' | b'-')) {
        s = &s[1..];
    }
    if s[0].eq_ignore_ascii_case(&b'i') {
        return Ok(pack(1 << 63, 0x7fff, negative, syntax.end, None, false));
    }
    if s[0].eq_ignore_ascii_case(&b'n') {
        if s.len() > 3 {
            return Err(Error::NanPayload);
        }
        return Ok(pack(3 << 62, 0x7fff, negative, syntax.end, None, false));
    }
    let hex = s.first() == Some(&b'0') && matches!(s.get(1), Some(b'x' | b'X'));
    let radix = if hex { 16 } else { 10 };
    let mut at = if hex { 2 } else { 0 };
    let mut n = Big::new(0);
    let (mut fraction, mut significant, mut point) = (0i32, 0i32, false);
    while at < s.len() {
        let c = s[at];
        if c == b'.' && !point {
            point = true;
            at += 1;
            continue;
        }
        let digit = match c {
            b'0'..=b'9' => u32::from(c - b'0'),
            b'a'..=b'f' if hex => u32::from(c - b'a' + 10),
            b'A'..=b'F' if hex => u32::from(c - b'A' + 10),
            _ => break,
        };
        if point {
            fraction += 1;
        }
        n.small(radix, digit);
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
            exponent = (exponent.saturating_mul(10) + i32::from(*c - b'0')).min(1_000_000);
            at += 1;
        }
        if minus {
            exponent = -exponent;
        }
    }
    if n.bits() == 0 {
        return Ok(pack(0, 0, negative, syntax.end, None, false));
    }
    if hex {
        return round(
            n,
            Big::new(1),
            exponent - 4 * fraction,
            negative,
            syntax.end,
            r,
        );
    }
    let power = exponent - fraction;
    let decimal = significant + power - 1;
    if decimal > 4932 {
        return Ok(extreme(negative, syntax.end, r, true));
    }
    if decimal < -4952 {
        return Ok(extreme(negative, syntax.end, r, false));
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
    round(n, d, power, negative, syntax.end, r)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fields(p: Parsed80) -> (u64, u16) {
        (
            u64::from_le_bytes(p.bytes[..8].try_into().unwrap()),
            u16::from_le_bytes(p.bytes[8..].try_into().unwrap()),
        )
    }
    #[test]
    fn exact_range_and_precision_beyond_binary64() {
        for (text, sig, exp) in [
            ("0", 0, 0),
            ("-0", 0, 0x8000),
            ("1", 1 << 63, 0x3fff),
            ("1.5", 3 << 62, 0x3fff),
            ("18446744073709551615", u64::MAX, 0x403e),
            ("0x1p-16445", 1, 0),
            ("0x1p-16382", 1 << 63, 1),
            ("0x1.fffffffffffffffep16383", u64::MAX, 0x7ffe),
        ] {
            let p = parse_extended80(text.as_bytes(), Rounding::NearestEven).unwrap();
            assert_eq!(fields(p), (sig, exp), "{text}");
            assert!(!p.inexact);
            assert_eq!(p.range, None);
        }
    }
    #[test]
    fn ties_carry_tiny_and_directed() {
        for (r, up) in [
            (Rounding::NearestEven, false),
            (Rounding::TowardZero, false),
            (Rounding::Down, false),
            (Rounding::Up, true),
        ] {
            let p = parse_extended80(b"18446744073709551617", r).unwrap();
            assert_eq!(fields(p), ((1 << 63) + u64::from(up), 0x403f));
            assert!(p.inexact);
            let tiny = parse_extended80(b"0x1p-16446", r).unwrap();
            assert_eq!(fields(tiny), (u64::from(up), 0));
            assert_eq!(tiny.range, Some(Range::Underflow));
        }
        assert_eq!(
            fields(parse_extended80(b"18446744073709551619", Rounding::NearestEven).unwrap()),
            ((1 << 63) + 2, 0x403f)
        );
        assert_eq!(
            fields(parse_extended80(b"0x1.ffffffffffffffffp0", Rounding::NearestEven).unwrap()),
            (1 << 63, 0x4000)
        );
        assert_eq!(
            fields(parse_extended80(b"-0x1p-16446", Rounding::Down).unwrap()),
            (1, 0x8000)
        );
    }
    #[test]
    fn end_bounds_and_explicit_nan_payload_gap() {
        let p = parse_extended80(b" \t-1.5e+tail", Rounding::NearestEven).unwrap();
        assert_eq!(p.end, 6);
        assert_eq!(fields(p), (3 << 62, 0xbfff));
        assert_eq!(
            parse_extended80(b"nan(123)", Rounding::NearestEven),
            Err(Error::NanPayload)
        );
        assert_eq!(
            fields(parse_extended80(b"inf!", Rounding::NearestEven).unwrap()),
            (1 << 63, 0x7fff)
        );
        assert!(parse_extended80(&vec![b'1'; 4097], Rounding::NearestEven).is_err());
    }
}
