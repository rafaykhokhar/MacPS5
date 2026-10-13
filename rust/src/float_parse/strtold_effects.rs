//! Target strtold lexer, integer hex conversion and source-ordered decimal effects.
//! Derived from FreeBSD 11.4 gdtoa strtodg.c/gethex.c/hexnan.c/strtorx.c;
//! see GDTOA-LICENSE and https://github.com/freebsd/freebsd-src/tree/releng/11.4/contrib/gdtoa.
//! The amd64 gd_qnan.h sets ldus_QNAN to canonical positive quiet extended NaN.
//! This is source policy, not evidence of the proprietary console libc build.
//! C locale, bounded 64KiB input/4096 significant digits; integer hex conversion
//! has no SSE effects, while decimal delegates source-ordered effects.
//! Extended80 assignments preserve target libc policy, including source quirks.
use super::{Big, Error as ParseError, Rounding, digit, parse, strtold_decimal};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Parse(ParseError),
    Decimal(strtold_decimal::Error),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Effects80 {
    pub bytes: [u8; 10],
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
// FPI64 hexnan uses two complete 32-bit words; ULtox forces integer/quiet bits.
fn nan_payload(s: &[u8], open: usize) -> (usize, u64) {
    let mut words = [0u32; 2];
    let mut word = 1usize;
    let mut group_top = 1usize;
    let mut digits = 0usize;
    let mut total = 0usize;
    let mut group_total = 0usize;
    let mut at = open + 1;
    let mut end = open;
    while s.get(at).is_some_and(|b| *b <= b' ') {
        at += 1;
    }
    if s.get(at) == Some(&b'0')
        && matches!(s.get(at + 1), Some(b'x' | b'X'))
        && s.get(at + 2).is_some_and(|b| *b > b' ')
    {
        at += 2;
    }
    while let Some(&b) = s.get(at) {
        if let Some(d) = digit(b, 16) {
            total += 1;
            digits += 1;
            if digits > 8 {
                if word == 0 {
                    at += 1;
                    continue;
                }
                word -= 1;
                words[word] = 0;
                digits = 1;
            }
            words[word] = words[word].wrapping_shl(4) | d;
            at += 1;
            continue;
        }
        if b <= b' ' {
            if group_total < total {
                if word < group_top && digits < 8 {
                    let shift = (8 - digits) * 4;
                    for i in word..group_top {
                        words[i] |= words[i + 1] << (32 - shift);
                        words[i + 1] >>= shift;
                    }
                }
                if word == 0 {
                    digits = 8;
                    at += 1;
                    continue;
                }
                group_total = total;
                word -= 1;
                group_top = word;
                words[word] = 0;
                digits = 0;
            }
            while s.get(at).is_some_and(|b| *b <= b' ') {
                at += 1;
            }
            if s.get(at) == Some(&b'0')
                && matches!(s.get(at + 1), Some(b'x' | b'X'))
                && s.get(at + 2).is_some_and(|b| *b > b' ')
            {
                at += 2;
            }
            continue;
        }
        if b == b')' && total != 0 {
            end = at + 1;
            break;
        }
        return (
            s[at..]
                .iter()
                .position(|b| *b == b')')
                .map_or(open, |i| at + i + 1),
            0,
        );
    }
    if total == 0 {
        return (end, 0);
    }
    if word < group_top && digits < 8 {
        let shift = (8 - digits) * 4;
        for i in word..group_top {
            words[i] |= words[i + 1] << (32 - shift);
            words[i + 1] >>= shift;
        }
    }
    if word == 1 {
        words[0] = words[1];
        words[1] = 0;
    }
    let payload = (words[1] as u64) << 32 | words[0] as u64;
    (end, payload.max(1))
}

fn any(b: &Big, n: usize) -> bool {
    (0..n.min(b.bits())).any(|i| b.0[i / 32] & (1 << (i % 32)) != 0)
}
fn bit(b: &Big, n: usize) -> bool {
    b.0.get(n / 32).is_some_and(|v| v & (1 << (n % 32)) != 0)
}
fn top(b: &Big, shift: usize) -> u64 {
    let mut v = 0;
    for i in (shift..b.bits()).rev() {
        v = (v << 1) | u64::from(bit(b, i))
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
fn hex(s: &[u8], negative: bool, r: Rounding) -> (u64, u16, Option<u32>) {
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
        return (0, 0, None);
    }
    let mut e = exp - frac * 4 + n as i32 - 64;
    let (mut m, mut lost) = if n > 64 {
        let shift = n - 64;
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
        (top(&b, 0) << (64 - n), 0)
    };
    if e > 16320 {
        return (1 << 63, 0x7fff, Some(34));
    }
    let mut denorm = false;
    let mut nbits = 64;
    if e < -16445 {
        denorm = true;
        let n = (-16445 - e) as u32;
        if n >= 64 {
            let up = if r == Rounding::NearestEven {
                n == 64 && m & 0x7fff_ffff_ffff_ffff != 0
            } else {
                outward(r, negative)
            };
            return (u64::from(up), 0, Some(34));
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
        e = -16445;
    }
    if lost != 0 {
        let up = if r == Rounding::NearestEven {
            lost & 2 != 0 && (u64::from(lost) | m) & 1 != 0
        } else {
            outward(r, negative)
        };
        if up {
            let (incremented, carry) = m.overflowing_add(1);
            m = incremented;
            if denorm {
                if nbits == 63 && m & (1 << 63) != 0 {
                    denorm = false
                }
            } else if carry {
                m = 1 << 63;
                e += 1;
                if e > 16320 {
                    return (1 << 63, 0x7fff, Some(34));
                }
            }
        }
    }
    (m, if denorm { 0 } else { (e + 16446) as u16 }, None)
}
pub fn parse_effects(input: &[u8], r: Rounding) -> Result<Effects80, Error> {
    let parsed = parse(input, r).map_err(Error::Parse)?;
    let mut result = Effects80 {
        bytes: [0; 10],
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
    result.bytes = pack(0, 0, negative && parsed.end != 0);
    if text.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'n')) {
        let offset = parsed.end - text.len();
        let full = &input[offset..];
        let full = &full[..full.iter().position(|b| *b == 0).unwrap_or(full.len())];
        let payload = if full.get(3) == Some(&b'(') {
            nan_payload(full, 3).1
        } else {
            0
        };
        result.bytes = pack(0xc000_0000_0000_0000 | payload, 0x7fff, negative);
        return Ok(result);
    }
    if text.first().is_some_and(|c| c.eq_ignore_ascii_case(&b'i')) {
        result.bytes = pack(1 << 63, 0x7fff, negative);
        return Ok(result);
    }
    if text.is_empty() {
        return Ok(result);
    }
    if text.first() == Some(&b'0') && matches!(text.get(1), Some(b'x' | b'X')) {
        let (significand, exponent, errno) = hex(text, negative, r);
        result.bytes = pack(significand, exponent, negative);
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
    let got = strtold_decimal::decimal(&digits[skip..], e, negative, 0x1f80 | (mode << 13))
        .map_err(Error::Decimal)?;
    result.bytes = got.bytes;
    result.errno = got.errno;
    result.mxcsr_flags = got.mxcsr & 63;
    Ok(result)
}

fn pack(significand: u64, exponent: u16, negative: bool) -> [u8; 10] {
    let mut bytes = [0; 10];
    bytes[..8].copy_from_slice(&significand.to_le_bytes());
    bytes[8..].copy_from_slice(&(exponent | if negative { 0x8000 } else { 0 }).to_le_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_hex_range_quirks_and_extended_payload() {
        for r in [
            Rounding::NearestEven,
            Rounding::Down,
            Rounding::Up,
            Rounding::TowardZero,
        ] {
            let got = parse_effects(b"-0x1p16384!", r).unwrap();
            assert_eq!(
                (got.bytes, got.end, got.errno, got.mxcsr_flags),
                (pack(1 << 63, 0x7fff, true), 10, Some(34), 0)
            );
        }
        let got = parse_effects(b"0x1p-16445", Rounding::NearestEven).unwrap();
        assert_eq!(
            (got.bytes, got.errno, got.mxcsr_flags),
            (pack(1, 0, false), None, 0)
        );
        // gethex loses prior low bits before its n >= nbits tiny branch.
        let got = parse_effects(
            b"0x1.000000000000000000000000000000000001p-16446",
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!((got.bytes, got.errno), (pack(0, 0, false), Some(34)));
        let got = parse_effects(b"0x1.0000000000000002", Rounding::NearestEven).unwrap();
        assert_eq!(got.bytes, pack(0x8000_0000_0000_0001, 0x3fff, false));
        let got = parse_effects(b"0x1.ffffffffffffffffp0", Rounding::NearestEven).unwrap();
        assert_eq!(got.bytes, pack(1 << 63, 0x4000, false));
    }
    #[test]
    fn two_word_nan_source_packing_and_consumption() {
        for (text, sig, end) in [
            ("nan(123456789abcdef01)", 0xd234_5678_9abc_def0, 22),
            ("nan(1 2)", 0xc000_0001_0000_0002, 8),
            ("nan(0)", 0xc000_0000_0000_0001, 6),
            ("nan(123", 0xc000_0000_0000_0123, 3),
            ("nan(1g)", 0xc000_0000_0000_0000, 7),
            ("nan()", 0xc000_0000_0000_0000, 5),
        ] {
            let got = parse_effects(text.as_bytes(), Rounding::NearestEven).unwrap();
            assert_eq!(
                (got.bytes, got.end, got.errno, got.mxcsr_flags),
                (pack(sig, 0x7fff, false), end, None, 0),
                "{text}"
            );
        }
    }
    #[test]
    fn decimal_delegation_zero_and_lexical_bounds() {
        let got = parse_effects(b"\x0b-0.1e+tail", Rounding::NearestEven).unwrap();
        assert_eq!(
            (got.bytes, got.end, got.errno, got.mxcsr_flags),
            (pack(0xcccc_cccc_cccc_cccd, 0x3ffb, true), 5, None, 32)
        );
        assert_eq!(
            parse_effects(b"-0.000e99999", Rounding::Up).unwrap().bytes,
            pack(0, 0, true)
        );
        assert_eq!(
            parse_effects(b" -tail", Rounding::Up).unwrap().bytes,
            pack(0, 0, false)
        );
        assert!(matches!(
            parse_effects(&vec![b'1'; 4097], Rounding::NearestEven),
            Err(Error::Parse(ParseError::SignificandLimit))
        ));
        assert!(matches!(
            parse_effects(&vec![b'0'; 65537], Rounding::NearestEven),
            Err(Error::Parse(ParseError::InputLimit))
        ));
    }
}
