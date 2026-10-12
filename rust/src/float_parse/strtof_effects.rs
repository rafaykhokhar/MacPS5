//! Target strtof lexer, integer hex conversion and source-ordered decimal effects.
//! Derived from FreeBSD 11.4 gdtoa strtodg.c/gethex.c; see GDTOA-LICENSE.
//! Binary32 assignments preserve target libc policy, including source quirks.
use super::{binary32, strtof_decimal, Big, Error as ParseError, Rounding};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
    Decimal(strtof_decimal::Error),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effects32 {
    pub bits: u32,
    pub end: usize,
    pub errno: Option<u32>,
    pub mxcsr_flags: u32,
}
fn outward(r: Rounding, negative: bool) -> bool {
    matches!(
        (r, negative),
        (Rounding::Up, false) | (Rounding::Down, true)
    )
}
// A 24-bit FPI allocates one 32-bit word: hexnan keeps the first eight
// hexadecimal digits and truncates that word to 24 bits (not the final digits).
fn nan_payload(s: &[u8]) -> u32 {
    if s.get(3) != Some(&b'(') {
        return 0;
    }
    let mut at = 4;
    while s.get(at).is_some_and(|c| *c != 0 && *c <= b' ') {
        at += 1;
    }
    if s.get(at) == Some(&b'0')
        && matches!(s.get(at + 1), Some(b'x' | b'X'))
        && s.get(at + 2).is_some_and(|c| *c > b' ')
    {
        at += 2;
    }
    let (mut word, mut digits, mut total) = (0u32, 0usize, 0usize);
    while let Some(c) = s.get(at).filter(|c| **c != 0) {
        at += 1;
        let d = match *c {
            b'0'..=b'9' => *c - b'0',
            b'a'..=b'f' => *c - b'a' + 10,
            b'A'..=b'F' => *c - b'A' + 10,
            b')' if total != 0 => break,
            1..=32 => {
                if total != 0 {
                    digits = 8;
                }
                continue;
            }
            _ => return 0,
        };
        total += 1;
        digits += 1;
        if digits <= 8 {
            word = (word << 4) | u32::from(d);
        }
    }
    if total == 0 {
        0
    } else {
        (word & 0xffffff).max(1)
    }
}
fn any(b: &Big, n: usize) -> bool {
    (0..n.min(b.bits())).any(|i| b.0[i / 32] & (1 << (i % 32)) != 0)
}
fn bit(b: &Big, n: usize) -> bool {
    b.0.get(n / 32).is_some_and(|v| v & (1 << (n % 32)) != 0)
}
fn top(b: &Big, shift: usize) -> u32 {
    let mut v = 0;
    for i in (shift..b.bits()).rev() {
        v = (v << 1) | u32::from(bit(b, i))
    }
    v
}
fn exponent(s: &[u8], at: &mut usize, cap: i32) -> i32 {
    let mut e: i32 = 0;
    if *at < s.len() {
        *at += 1;
        let minus = s.get(*at) == Some(&b'-');
        if matches!(s.get(*at), Some(b'+' | b'-')) {
            *at += 1
        }
        while let Some(c) = s.get(*at).filter(|c| c.is_ascii_digit()) {
            e = (e.saturating_mul(10) + i32::from(*c - b'0')).min(cap);
            *at += 1
        }
        if minus {
            e = -e
        }
    }
    e
}
fn hex(s: &[u8], negative: bool, r: Rounding) -> (u32, Option<u32>) {
    let mut b = Big::new(0);
    let mut at = 2;
    let mut frac = 0;
    let mut point = false;
    while let Some(c) = s.get(at) {
        if *c == b'.' {
            point = true;
            at += 1;
            continue;
        }
        let d = match *c {
            b'0'..=b'9' => *c - b'0',
            b'a'..=b'f' => *c - b'a' + 10,
            b'A'..=b'F' => *c - b'A' + 10,
            _ => break,
        };
        b.small(16, u32::from(d));
        if point {
            frac += 1
        }
        at += 1;
    }
    let exp = exponent(s, &mut at, 1_000_000);
    let n = b.bits();
    if n == 0 {
        return (0, None);
    }
    let mut e = exp - frac * 4 + n as i32 - 24;
    let (mut m, mut lost) = if n > 24 {
        let shift = n - 24;
        let lost = if any(&b, shift) {
            if bit(&b, shift - 1) {
                2 | u32::from(shift > 1 && any(&b, shift - 1))
            } else {
                1
            }
        } else {
            0
        };
        (top(&b, shift), lost)
    } else {
        (top(&b, 0) << (24 - n), 0)
    };
    if e > 104 {
        return (0x7f800000, Some(34));
    }
    let mut denorm = false;
    let mut nbits = 24;
    if e < -149 {
        denorm = true;
        let n = (-149 - e) as u32;
        if n >= 24 {
            let up = if r == Rounding::NearestEven {
                n == 24 && m & 0x7fffff != 0
            } else {
                outward(r, negative)
            };
            return (u32::from(up), Some(34));
        }
        let k = n - 1;
        lost = if lost != 0 {
            1
        } else {
            u32::from(k > 0 && m & ((1 << k) - 1) != 0)
        };
        if m & (1 << k) != 0 {
            lost |= 2
        }
        nbits -= n;
        m >>= n;
        e = -149;
    }
    if lost != 0 {
        let up = if r == Rounding::NearestEven {
            lost & 2 != 0 && (lost | m) & 1 != 0
        } else {
            outward(r, negative)
        };
        if up {
            m += 1;
            if denorm {
                if nbits == 23 && m & (1 << 23) != 0 {
                    denorm = false
                }
            } else if m & (1 << 24) != 0 {
                m >>= 1;
                e += 1;
                if e > 104 {
                    return (0x7f800000, Some(34));
                }
            }
        }
    }
    (
        if denorm {
            m
        } else {
            m & 0x7fffff | ((e + 150) as u32) << 23
        },
        None,
    )
}
pub fn parse_effects(input: &[u8], r: Rounding) -> Result<Effects32, Error> {
    let parsed = binary32::parse_binary32(input, r).map_err(Error::Parse)?;
    let mut result = Effects32 {
        bits: parsed.bits,
        end: parsed.end,
        errno: None,
        mxcsr_flags: 0,
    };
    let mut text = &input[..parsed.end];
    while text.first().is_some_and(|b| matches!(*b, 9..=13 | 32)) {
        text = &text[1..]
    }
    let negative = text.first() == Some(&b'-');
    if matches!(text.first(), Some(b'+' | b'-')) {
        text = &text[1..]
    }
    if text.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'n')) {
        let offset = parsed.end - text.len();
        result.bits =
            0x7fc00000 | nan_payload(&input[offset..]) | if negative { 1 << 31 } else { 0 };
        return Ok(result);
    }
    if text.is_empty()
        || text
            .first()
            .is_some_and(|b| b.eq_ignore_ascii_case(&b'i') || b.eq_ignore_ascii_case(&b'n'))
    {
        return Ok(result);
    }
    if text.first() == Some(&b'0') && matches!(text.get(1), Some(b'x' | b'X')) {
        let (bits, errno) = hex(text, negative, r);
        result.bits = bits | if negative { 1 << 31 } else { 0 };
        result.errno = errno;
        return Ok(result);
    }
    let mut at = 0;
    let mut digits = Vec::new();
    let mut frac = 0;
    while text.get(at).is_some_and(u8::is_ascii_digit) {
        digits.push(text[at]);
        at += 1
    }
    if text.get(at) == Some(&b'.') {
        at += 1;
        while text.get(at).is_some_and(u8::is_ascii_digit) {
            digits.push(text[at]);
            frac += 1;
            at += 1
        }
        while frac > 0 && digits.last() == Some(&b'0') {
            digits.pop();
            frac -= 1
        }
    }
    let Some(skip) = digits.iter().position(|b| *b != b'0') else {
        return Ok(result);
    };
    let e = exponent(text, &mut at, 19999) - frac;
    let mode = match r {
        Rounding::NearestEven => 0,
        Rounding::Down => 1,
        Rounding::Up => 2,
        Rounding::TowardZero => 3,
    };
    let got = strtof_decimal::decimal(&digits[skip..], e, negative, 0x1f80 | (mode << 13))
        .map_err(Error::Decimal)?;
    result.bits = got.bits;
    result.errno = got.errno;
    result.mxcsr_flags = got.mxcsr & 63;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_payload_and_hex_boundary_policy() {
        let got = parse_effects(b"-nan(0x123456789)", Rounding::NearestEven).unwrap();
        assert_eq!(
            (got.bits, got.end, got.errno, got.mxcsr_flags),
            (0xfff45678, 17, None, 0)
        );
        // gethex tests retained leading bits at this tiny boundary, ignoring
        // digits already discarded below its 24-bit working significand.
        let got = parse_effects(
            b"0x1.00000000000000000000000000000000001p-150",
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!((got.bits, got.errno, got.mxcsr_flags), (0, Some(34), 0));
        let got = parse_effects(b"0x1p128", Rounding::TowardZero).unwrap();
        assert_eq!(
            (got.bits, got.errno, got.mxcsr_flags),
            (0x7f800000, Some(34), 0)
        );
        let got = parse_effects(b"0x1p-149", Rounding::NearestEven).unwrap();
        assert_eq!((got.bits, got.errno, got.mxcsr_flags), (1, None, 0));
    }
    #[test]
    fn decimal_rounds_directly_to_binary32_and_retains_end() {
        let text = b"1.0000000596046447753906250000000000000001";
        let got = parse_effects(text, Rounding::NearestEven).unwrap();
        assert_eq!(
            (got.bits, got.end, got.errno),
            (0x3f800001, text.len(), None)
        );
        let got = parse_effects(b"\x0b-0.1e+tail", Rounding::NearestEven).unwrap();
        assert_eq!((got.bits, got.end, got.mxcsr_flags), (0xbdcccccd, 5, 32));
    }
}
