//! Bounded locale-independent binary64 parser. No host floating-point operation.
//! Pure conversion engine; guest fenv/errno service integration remains separate.
mod big;
mod decimal_arithmetic;
mod decimal_big;
mod decimal_effects;
mod decimal_refinement;
pub mod effects;
use big::Big;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    NearestEven,
    TowardZero,
    Up,
    Down,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Underflow,
    Overflow,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub bits: u64,
    pub end: usize,
    pub range: Option<Range>,
    pub inexact: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InputLimit,
    SignificandLimit,
}
const SIGN: u64 = 1 << 63;
const INF: u64 = 0x7ff0000000000000;
fn digit(b: u8, radix: u32) -> Option<u32> {
    let d = match b {
        b'0'..=b'9' => (b - b'0') as u32,
        b'a'..=b'f' => (b - b'a' + 10) as u32,
        b'A'..=b'F' => (b - b'A' + 10) as u32,
        _ => return None,
    };
    (d < radix).then_some(d)
}
fn outward(r: Rounding, negative: bool) -> bool {
    matches!(
        (r, negative),
        (Rounding::Up, false) | (Rounding::Down, true)
    )
}
fn extreme(negative: bool, end: usize, r: Rounding, overflow: bool) -> Parsed {
    let magnitude = if overflow {
        if matches!(r, Rounding::NearestEven) || outward(r, negative) {
            INF
        } else {
            INF - 1
        }
    } else {
        u64::from(outward(r, negative))
    };
    Parsed {
        bits: magnitude | if negative { SIGN } else { 0 },
        end,
        range: Some(if overflow {
            Range::Overflow
        } else {
            Range::Underflow
        }),
        inexact: true,
    }
}
fn rounded(n: Big, d: Big, scale: i32, negative: bool, end: usize, r: Rounding) -> Parsed {
    if n.bits() == 0 {
        return Parsed {
            bits: if negative { SIGN } else { 0 },
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
    if exponent > 1023 {
        return extreme(negative, end, r, true);
    }
    if exponent < -1075 {
        return extreme(negative, end, r, false);
    }
    let unit = (exponent - 52).max(-1074);
    let shift = scale - unit;
    let (n, d) = if shift >= 0 {
        (n.shl(shift as usize), d)
    } else {
        (n, d.shl((-shift) as usize))
    };
    let (mut q, remainder) = n.divide_small_quotient(&d);
    let inexact = remainder.bits() != 0;
    let increment = if !inexact {
        false
    } else {
        match r {
            Rounding::NearestEven => {
                let cmp = remainder.shl(1).cmp(&d);
                cmp.is_gt() || (cmp.is_eq() && q & 1 != 0)
            }
            _ => outward(r, negative),
        }
    };
    q += u64::from(increment);
    let mut e = exponent;
    if q == 1 << 53 {
        q >>= 1;
        e += 1;
    }
    if e > 1023 {
        return extreme(negative, end, r, true);
    }
    let magnitude = if exponent < -1022 {
        q
    } else {
        ((e + 1023) as u64) << 52 | (q & ((1 << 52) - 1))
    };
    Parsed {
        bits: magnitude | if negative { SIGN } else { 0 },
        end,
        range: if magnitude < 1 << 52 && inexact {
            Some(Range::Underflow)
        } else {
            None
        },
        inexact,
    }
}
/// Up to64KiB input and4096 significant digits. Exponents are scanned fully but
/// saturated; clear overflow/underflow returns without allocating giant powers.
/// Range reports mathematical inexact underflow; exact-subnormal libc errno
/// policy is deliberately left to the pinned ABI adapter.
pub fn parse(input: &[u8], rounding: Rounding) -> Result<Parsed, Error> {
    if input.len() > 65536 {
        return Err(Error::InputLimit);
    }
    let endnul = input.iter().position(|b| *b == 0).unwrap_or(input.len());
    let s = &input[..endnul];
    let mut at = 0;
    while at < s.len() && matches!(s[at], b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c') {
        at += 1;
    }
    let negative = s.get(at) == Some(&b'-');
    if matches!(s.get(at), Some(b'+' | b'-')) {
        at += 1;
    }
    let starts = |p: usize, v: &[u8]| {
        s.get(p..p + v.len())
            .is_some_and(|x| x.eq_ignore_ascii_case(v))
    };
    if starts(at, b"inf") {
        at += 3;
        if starts(at, b"inity") {
            at += 5;
        }
        return Ok(Parsed {
            bits: INF | if negative { SIGN } else { 0 },
            end: at,
            range: None,
            inexact: false,
        });
    }
    if starts(at, b"nan") {
        at += 3;
        let payload = if s.get(at) == Some(&b'(') {
            let (end, payload) = nan_payload(s, at);
            at = end;
            payload
        } else {
            0
        };
        return Ok(Parsed {
            bits: 0x7ff8000000000000 | payload | if negative { SIGN } else { 0 },
            end: at,
            range: None,
            inexact: false,
        });
    }
    let start = at;
    let mut radix = 10;
    if s.get(at) == Some(&b'0') && matches!(s.get(at + 1), Some(b'x' | b'X')) {
        let p = at + 2;
        let valid = s.get(p).and_then(|b| digit(*b, 16)).is_some()
            || (s.get(p) == Some(&b'.') && s.get(p + 1).and_then(|b| digit(*b, 16)).is_some());
        if valid {
            radix = 16;
            at += 2;
        }
    }
    let mut digits = Vec::new();
    let mut point = false;
    let mut fractional = 0i32;
    let mut any = false;
    let mut nonzero = false;
    while at < s.len() {
        if s[at] == b'.' && !point {
            point = true;
            at += 1;
            continue;
        }
        let Some(d) = digit(s[at], radix) else { break };
        any = true;
        nonzero |= d != 0;
        if nonzero {
            if digits.len() == 4096 {
                return Err(Error::SignificandLimit);
            }
            digits.push(d);
        }
        if point {
            fractional += 1;
        }
        at += 1;
    }
    if !any {
        return Ok(Parsed {
            bits: 0,
            end: 0,
            range: None,
            inexact: false,
        });
    }
    let mut exponent = 0i32;
    let marker = if radix == 16 { b'p' } else { b'e' };
    if s.get(at).is_some_and(|b| b.to_ascii_lowercase() == marker) {
        let saved = at;
        at += 1;
        let neg = s.get(at) == Some(&b'-');
        if matches!(s.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        let begin = at;
        while let Some(d) = s.get(at).and_then(|b| digit(*b, 10)) {
            exponent = (exponent.saturating_mul(10) + d as i32).min(1_000_000);
            at += 1;
        }
        if at == begin {
            at = saved
        } else if neg {
            exponent = -exponent;
        }
    }
    debug_assert!(at > start);
    let mut n = Big::new(0);
    for d in &digits {
        n.small(radix, *d);
    }
    let mut d = Big::new(1);
    let scale = if radix == 16 {
        exponent - fractional * 4
    } else {
        let decimal = exponent - fractional;
        let magnitude = digits.len() as i32 + decimal;
        if nonzero && magnitude > 310 {
            return Ok(extreme(negative, at, rounding, true));
        }
        if nonzero && magnitude < -325 {
            return Ok(extreme(negative, at, rounding, false));
        }
        if !nonzero {
            return Ok(Parsed {
                bits: if negative { SIGN } else { 0 },
                end: at,
                range: None,
                inexact: false,
            });
        }
        if decimal >= 0 {
            for _ in 0..decimal {
                n.small(5, 0)
            }
        } else {
            for _ in 0..-decimal {
                d.small(5, 0)
            }
        }
        decimal
    };
    Ok(rounded(n, d, scale, negative, at, rounding))
}

/// gdtoa's 52-bit hexnan grammar accepts hexadecimal word groups separated by
/// bytes <=space. Malformed closed payload is consumed but discarded; an open
/// valid payload can influence bits without advancing past `nan`.
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
    } else {
        words[1] &= 0xfffff;
    }
    let payload = (words[1] as u64) << 32 | words[0] as u64;
    (end, payload.max(1))
}

/// FreeBSD11.4 gdtoa policy is distinct from mathematical IEEE rounding.
/// In particular, hexadecimal overflow returns infinity for every mode;
/// decimal subnormals set ERANGE while ordinary hexadecimal subnormals do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeBsdParsed {
    pub bits: u64,
    pub end: usize,
    pub errno: Option<i32>,
    pub mathematical_range: Option<Range>,
    pub inexact: bool,
}
pub fn parse_freebsd(input: &[u8], rounding: Rounding) -> Result<FreeBsdParsed, Error> {
    let result = parse(input, rounding)?;
    let mut bits = result.bits;
    let magnitude = bits & !SIGN;
    let token = &input[..result.end];
    let start = token
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c'))
        .unwrap_or(token.len());
    let mut token = &token[start..];
    if matches!(token.first(), Some(b'+' | b'-')) {
        token = &token[1..];
    }
    let hex = token.starts_with(b"0x") || token.starts_with(b"0X");
    let finite = token
        .first()
        .is_some_and(|b| b.is_ascii_digit() || *b == b'.');
    let nonzero = magnitude != 0 || result.inexact;
    let range = if !finite || !nonzero {
        false
    } else if hex {
        if result.range == Some(Range::Overflow) {
            bits = (bits & SIGN) | INF;
            true
        } else {
            // gethex n>=nbits: the exact magnitude is below the minimum
            // subnormal. Other inexact hexadecimal subnormals preserve errno.
            parse(input, Rounding::TowardZero)?.bits & !SIGN == 0
        }
    } else {
        if magnitude < (1 << 52) && decimal_approximation_exponent(token) <= -512 {
            // strtod.c e1>>4 >= 1<<n_bigtens reaches undfl before refinement.
            bits &= SIGN;
        } else if magnitude == 0 && rounding == Rounding::Down && bits & SIGN == 0 {
            // The downward decimal refinement reaches negative zero through
            // exact cancellation; the early undfl shortcut above does not.
            bits = SIGN;
        }
        result.range == Some(Range::Overflow) || magnitude < (1 << 52)
    };
    Ok(FreeBsdParsed {
        bits,
        end: result.end,
        errno: range.then_some(34),
        mathematical_range: result.range,
        inexact: result.inexact,
    })
}
fn decimal_approximation_exponent(token: &[u8]) -> i32 {
    let mut point = false;
    let mut fractional = 0i32;
    let mut digits = 0i32;
    let mut nonzero = false;
    let mut trailing = 0i32;
    let mut at = 0;
    while let Some(&b) = token.get(at) {
        if b == b'.' {
            point = true;
            at += 1;
            continue;
        }
        if !b.is_ascii_digit() {
            break;
        }
        nonzero |= b != b'0';
        if nonzero {
            digits += 1;
        }
        if point {
            fractional += 1;
            trailing = if b == b'0' { trailing + 1 } else { 0 };
        }
        at += 1;
    }
    fractional -= trailing;
    digits -= trailing.min(digits);
    let mut exponent = 0i32;
    if matches!(token.get(at), Some(b'e' | b'E')) {
        at += 1;
        let negative = token.get(at) == Some(&b'-');
        if matches!(token.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while let Some(d) = token.get(at).and_then(|b| digit(*b, 10)) {
            exponent = (exponent.saturating_mul(10) + d as i32).min(1_000_000);
            at += 1;
        }
        if negative {
            exponent = -exponent;
        }
    }
    exponent - fractional + (digits - 16).max(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn freebsd_range_policy_is_separate_from_ieee_rounding() {
        for (text, rounding, bits, errno) in [
            ("0x1p-1074", Rounding::NearestEven, 1, None),
            ("0x1.1p-1074", Rounding::NearestEven, 1, None),
            ("0x1p-1075", Rounding::Up, 1, Some(34)),
            ("5e-324", Rounding::NearestEven, 1, Some(34)),
            (
                "2.2250738585072012e-308",
                Rounding::NearestEven,
                1 << 52,
                None,
            ),
            ("0x1p1024", Rounding::TowardZero, INF, Some(34)),
            ("1e309", Rounding::TowardZero, INF - 1, Some(34)),
            ("1e-400", Rounding::Down, SIGN, Some(34)),
            ("1e-5000", Rounding::Up, 0, Some(34)),
        ] {
            let parsed = parse_freebsd(text.as_bytes(), rounding).unwrap();
            assert_eq!((parsed.bits, parsed.errno), (bits, errno), "{text}");
            assert_eq!(parsed.end, text.len());
        }
        assert_eq!(
            parse(b"0x1p1024", Rounding::TowardZero).unwrap().bits,
            INF - 1
        );
    }
    #[test]
    fn syntax_and_extremes() {
        for (s, bits, end) in [
            (" -0tail", SIGN, 3),
            ("1e+tail", 0x3ff0000000000000, 1),
            ("0x1.8p+1!", 0x4008000000000000, 8),
            ("0xQ", 0, 1),
            ("+.", 0, 0),
            ("-INFINITY!", SIGN | INF, 9),
            ("NaN", 0x7ff8000000000000, 3),
            ("0x1p-1074", 1, 9),
        ] {
            let p = parse(s.as_bytes(), Rounding::NearestEven).unwrap();
            assert_eq!((p.bits, p.end), (bits, end), "{s}");
        }
        assert_eq!(
            parse(b"1e100000", Rounding::TowardZero).unwrap().bits,
            INF - 1
        );
        assert_eq!(parse(b"-1e-100000", Rounding::Down).unwrap().bits, SIGN | 1);
    }
    #[test]
    fn halfway_and_boundaries() {
        let s = b"1.00000000000000011102230246251565404236316680908203125";
        assert_eq!(
            parse(s, Rounding::NearestEven).unwrap().bits,
            0x3ff0000000000000
        );
        assert_eq!(parse(s, Rounding::Up).unwrap().bits, 0x3ff0000000000001);
        assert_eq!(parse(b"0x1p-1075", Rounding::NearestEven).unwrap().bits, 0);
        assert_eq!(parse(b"0x1p-1075", Rounding::Up).unwrap().bits, 1);
        assert_eq!(
            parse(b"0x1.fffffffffffffp1023", Rounding::NearestEven)
                .unwrap()
                .bits,
            INF - 1
        );
        assert_eq!(
            parse(b"0x1.fffffffffffff8p1023", Rounding::NearestEven)
                .unwrap()
                .bits,
            INF
        );
        assert_eq!(
            parse(b"0x0.fffffffffffff8p-1022", Rounding::NearestEven)
                .unwrap()
                .bits,
            1 << 52
        );
    }
    #[test]
    fn independent_rust_decimal_reference() {
        let mut state = 0x4d595df4d0f33173u64;
        for _ in 0..10000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let e = (state % 700) as i32 - 350;
            let text = format!("{}e{}", state, e);
            let expected = text.parse::<f64>().unwrap().to_bits();
            let actual = parse(text.as_bytes(), Rounding::NearestEven).unwrap();
            assert_eq!(actual.bits, expected, "{text}");
            assert_eq!(actual.end, text.len());
        }
    }
}
