//! LP64 integer conversion policy follows FreeBSD releng/11.4 strtol/strtoull.
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/stdlib/strtol.c
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/stdlib/strtoull.c
//! Guest locale currently supports C/POSIX only. No host parsing or locale state.
use crate::{Result, guest_memory::Memory};
pub const BYTE_LIMIT: u64 = 65_536;
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedInteger {
    pub bits: u64,
    pub end_offset: u64,
    pub errno: Option<u32>,
}
fn byte(memory: &Memory<'_>, address: u64, offset: u64) -> Result<u8> {
    if offset >= BYTE_LIMIT {
        return Err("Integer conversion exceeds prototype byte limit".into());
    }
    let pointer = address
        .checked_add(offset)
        .ok_or("Integer string address overflow")?;
    Ok(memory.read(pointer, 1)?[0])
}
fn digit(c: u8) -> Option<u32> {
    match c {
        b'0'..=b'9' => Some(u32::from(c - b'0')),
        b'A'..=b'Z' => Some(u32::from(c - b'A') + 10),
        b'a'..=b'z' => Some(u32::from(c - b'a') + 10),
        _ => None,
    }
}
/// Returns a plan only: the caller validates all output/errno locations before
/// committing errno followed by endptr. A successful parse preserves errno.
pub fn parse(
    memory: &Memory<'_>,
    address: u64,
    mut base: i32,
    signed: bool,
) -> Result<ParsedInteger> {
    let mut offset = 0;
    let mut c = byte(memory, address, offset)?;
    while matches!(c, 9..=13 | 32) {
        offset += 1;
        c = byte(memory, address, offset)?;
    }
    let negative = c == b'-';
    if negative || c == b'+' {
        offset += 1;
        c = byte(memory, address, offset)?;
    }
    if (base == 0 || base == 16)
        && c == b'0'
        && matches!(byte(memory, address, offset + 1)?, b'x' | b'X')
        && digit(byte(memory, address, offset + 2)?).is_some_and(|d| d < 16)
    {
        offset += 2;
        c = byte(memory, address, offset)?;
        base = 16;
    }
    if base == 0 {
        base = if c == b'0' { 8 } else { 10 };
    }
    if !(2..=36).contains(&base) {
        return Ok(ParsedInteger {
            bits: 0,
            end_offset: 0,
            errno: Some(22),
        });
    }
    let limit = if signed {
        (i64::MAX as u64) + u64::from(negative)
    } else {
        u64::MAX
    };
    let radix = base as u64;
    let mut magnitude = 0u64;
    let mut any = false;
    let mut overflow = false;
    while let Some(d) = digit(c).filter(|d| *d < base as u32) {
        any = true;
        let d = u64::from(d);
        if magnitude > limit / radix || (magnitude == limit / radix && d > limit % radix) {
            overflow = true;
        } else if !overflow {
            magnitude = magnitude * radix + d;
        }
        offset += 1;
        c = byte(memory, address, offset)?;
    }
    if !any {
        return Ok(ParsedInteger {
            bits: 0,
            end_offset: 0,
            errno: Some(22),
        });
    }
    let bits = if overflow {
        if signed && negative {
            i64::MIN as u64
        } else {
            limit
        }
    } else if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    };
    Ok(ParsedInteger {
        bits,
        end_offset: offset,
        errno: overflow.then_some(34),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn check(s: &[u8], base: i32, signed: bool, bits: u64, end: u64, errno: Option<u32>) {
        let mut b = s.to_vec();
        let mut m = Memory::default();
        m.add(0x1_0000_0001, &mut b, false, false).unwrap();
        assert_eq!(
            parse(&m, 0x1_0000_0001, base, signed).unwrap(),
            ParsedInteger {
                bits,
                end_offset: end,
                errno
            },
            "{s:?} base {base}"
        );
        assert_eq!(m.read(0x1_0000_0001, s.len()).unwrap(), s);
    }
    #[test]
    fn syntax_and_endpoints() {
        for (s, base, bits, end) in [
            (&b" \t\n\r\x0b\x0c-12!"[..], 10, (-12i64) as u64, 9),
            (&b"0x\0"[..], 0, 0, 1),
            (&b"0xg"[..], 16, 0, 1),
            (&b"0Xf!"[..], 0, 15, 3),
            (&b"09"[..], 0, 0, 1),
            (&b"0b11"[..], 0, 0, 1),
            (&b"1012"[..], 2, 5, 3),
            (&b"Zz!"[..], 36, 1295, 2),
            (&b"077!"[..], 0, 63, 3),
        ] {
            check(s, base, true, bits, end, None);
        }
        for s in [&b"\0"[..], &b" -!"[..], &b"\xff"[..]] {
            check(s, 10, true, 0, 0, Some(22));
        }
        for base in [-1, 1, 37, i32::MAX] {
            check(b"1", base, true, 0, 0, Some(22));
        }
    }
    #[test]
    fn exact_limits_and_overflow_scanning() {
        check(b"9223372036854775807!", 10, true, i64::MAX as u64, 19, None);
        check(
            b"-9223372036854775808!",
            10,
            true,
            i64::MIN as u64,
            20,
            None,
        );
        check(
            b"9223372036854775808999!",
            10,
            true,
            i64::MAX as u64,
            22,
            Some(34),
        );
        check(
            b"-9223372036854775809!",
            10,
            true,
            i64::MIN as u64,
            20,
            Some(34),
        );
        check(b"18446744073709551615!", 10, false, u64::MAX, 20, None);
        check(b"18446744073709551616!", 10, false, u64::MAX, 20, Some(34));
        check(b"-18446744073709551615!", 10, false, 1, 21, None);
        check(b"-18446744073709551616!", 10, false, u64::MAX, 21, Some(34));
        check(b"-1!", 10, false, u64::MAX, 2, None);
        check(b"ffffffffffffffff!", 16, false, u64::MAX, 16, None);
        check(b"2147483648!", 10, true, 2147483648, 10, None);
    }
    #[test]
    fn guarded_and_bounded() {
        let mut b = vec![b'9'; BYTE_LIMIT as usize];
        let mut m = Memory::default();
        m.add(0x1000, &mut b, false, false).unwrap();
        assert!(parse(&m, 0x1000, 10, true).is_err());
        assert!(parse(&m, 0x1000 + BYTE_LIMIT - 1, 10, true).is_err());
        assert!(parse(&m, u64::MAX, 10, true).is_err());
    }
}
