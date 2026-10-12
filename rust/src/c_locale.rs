//! C/POSIX byte classification only; GuestLocale rejects all other locales.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/ctype.h:
//! all functions accept/return int. FreeBSD11.4 include/_ctype.h normalizes
//! classification to0/1. locale/isalpha.3 and tolower.3 require unsigned char
//! or EOF; values outside that domain are an explicit prototype error here.
//! string/strcasecmp.c compares lowercased unsigned bytes by subtraction.
use crate::{Result, guest_memory::Memory};
pub const COMPARE_LIMIT: u64 = 65_536;
#[derive(Clone, Copy, Debug)]
pub enum Ctype {
    Isalpha,
    Isalnum,
    Isxdigit,
    Ispunct,
    Isspace,
    Isupper,
    Islower,
    Isprint,
    Isgraph,
    Iscntrl,
    Toupper,
    Tolower,
    Isblank,
}
pub fn apply(operation: Ctype, value: i32) -> Result<i32> {
    if !(-1..=255).contains(&value) {
        return Err("Ctype input outside unsigned-byte/EOF domain".into());
    }
    let upper = (65..=90).contains(&value);
    let lower = (97..=122).contains(&value);
    let number = (48..=57).contains(&value);
    let graph = (33..=126).contains(&value);
    Ok(match operation {
        Ctype::Isalpha => i32::from(upper || lower),
        Ctype::Isalnum => i32::from(upper || lower || number),
        Ctype::Isxdigit => {
            i32::from(number || (65..=70).contains(&value) || (97..=102).contains(&value))
        }
        Ctype::Ispunct => i32::from(graph && !(upper || lower || number)),
        Ctype::Isspace => i32::from(matches!(value, 9..=13 | 32)),
        Ctype::Isupper => i32::from(upper),
        Ctype::Islower => i32::from(lower),
        Ctype::Isprint => i32::from((32..=126).contains(&value)),
        Ctype::Isgraph => i32::from(graph),
        Ctype::Iscntrl => i32::from((0..=31).contains(&value) || value == 127),
        Ctype::Toupper => {
            if lower {
                value - 32
            } else {
                value
            }
        }
        Ctype::Tolower => {
            if upper {
                value + 32
            } else {
                value
            }
        }
        Ctype::Isblank => i32::from(matches!(value, 9 | 32)),
    })
}
fn fold(value: u8) -> i32 {
    i32::from(if value.is_ascii_uppercase() {
        value + 32
    } else {
        value
    })
}
pub fn strncasecmp(memory: &Memory<'_>, left: u64, right: u64, count: u64) -> Result<i32> {
    for index in 0..count.min(COMPARE_LIMIT) {
        let a = memory.load(
            left.checked_add(index)
                .ok_or("Case comparison address overflow")?,
            1,
        )? as u8;
        let b = memory.load(
            right
                .checked_add(index)
                .ok_or("Case comparison address overflow")?,
            1,
        )? as u8;
        let difference = fold(a) - fold(b);
        if difference != 0 || a == 0 {
            return Ok(difference);
        }
    }
    if count <= COMPARE_LIMIT {
        Ok(0)
    } else {
        Err("Case comparison exceeds prototype limit".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_bytes_and_eof() {
        // Explicit character-set membership is independent of production range predicates.
        let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let digits = b"0123456789";
        let punctuation = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";
        for c in -1..=255 {
            let has = |set: &[u8]| c >= 0 && set.contains(&(c as u8));
            for (op, truth) in [
                (Ctype::Isalpha, has(alpha)),
                (Ctype::Isalnum, has(alpha) || has(digits)),
                (Ctype::Isxdigit, has(b"0123456789ABCDEFabcdef")),
                (Ctype::Ispunct, has(punctuation)),
                (Ctype::Isspace, has(b"\t\n\x0b\x0c\r ")),
                (Ctype::Isupper, has(&alpha[..26])),
                (Ctype::Islower, has(&alpha[26..])),
                (
                    Ctype::Isprint,
                    has(alpha) || has(digits) || has(punctuation) || c == 32,
                ),
                (
                    Ctype::Isgraph,
                    has(alpha) || has(digits) || has(punctuation),
                ),
                (Ctype::Iscntrl, (0..32).chain([127]).any(|v| v == c)),
                (Ctype::Isblank, has(b"\t ")),
            ] {
                assert_eq!(apply(op, c).unwrap(), i32::from(truth), "{op:?} {c}");
            }
            let idx = alpha.iter().position(|v| i32::from(*v) == c);
            let low = idx.map_or(c, |i| i32::from(alpha[26 + i % 26]));
            let up = idx.map_or(c, |i| i32::from(alpha[i % 26]));
            assert_eq!(apply(Ctype::Tolower, c).unwrap(), low);
            assert_eq!(apply(Ctype::Toupper, c).unwrap(), up);
        }
        for c in [i32::MIN, -2, 256, i32::MAX] {
            assert!(apply(Ctype::Isalpha, c).is_err());
            assert!(apply(Ctype::Tolower, c).is_err());
        }
    }
    #[test]
    fn all_byte_pairs_unsigned_fold_and_guards() {
        let mut bytes = [0u8; 2];
        for a in 0u8..=255 {
            for b in 0u8..=255 {
                bytes = [a, b];
                let mut m = Memory::default();
                m.add(0x1_0000_0001, &mut bytes, false, false).unwrap();
                assert_eq!(
                    strncasecmp(&m, 0x1_0000_0001, 0x1_0000_0002, 1).unwrap(),
                    i32::from(a.to_ascii_lowercase()) - i32::from(b.to_ascii_lowercase())
                );
            }
        }
        let mut m = Memory::default();
        m.add(0x1000, &mut bytes, false, false).unwrap();
        assert_eq!(strncasecmp(&m, 0, u64::MAX, 0).unwrap(), 0);
        assert!(strncasecmp(&m, 0x1001, 0x1001, 2).is_err());
        let mut b = *b"Ab\0aB\0";
        let before = b;
        let mut m = Memory::default();
        m.add(0x1_0000_0001, &mut b, false, false).unwrap();
        assert_eq!(
            strncasecmp(&m, 0x1_0000_0001, 0x1_0000_0004, u64::MAX).unwrap(),
            0
        );
        assert_eq!(m.read(0x1_0000_0001, before.len()).unwrap(), before);
    }
    #[test]
    fn full_count_bounded_work() {
        let mut b = vec![b'A'; COMPARE_LIMIT as usize];
        let mut m = Memory::default();
        m.add(0x1000, &mut b, false, false).unwrap();
        assert_eq!(strncasecmp(&m, 0x1000, 0x1000, COMPARE_LIMIT).unwrap(), 0);
        assert!(strncasecmp(&m, 0x1000, 0x1000, 1u64 << 32).is_err());
    }
}
