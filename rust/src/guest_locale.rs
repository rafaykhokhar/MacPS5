//! Restricted owned C locale; no host locale/environment access.
//! Category ABI: https://github.com/ps5-payload-dev/sdk/blob/master/include/freebsd/locale.h
//! Query/NULL failure/default-C semantics:
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/locale/setlocale.3
//! Pinned ELF main0x7fc2 calls setlocale(5,0x276e49), whose first byte is NUL.
//! Thus its empty-name request needs an explicitly configured guest environment.
use crate::{Result, guest_memory::Memory};
pub const LC_ALL: i32 = 0;
pub const LC_COLLATE: i32 = 1;
pub const LC_CTYPE: i32 = 2;
pub const LC_MONETARY: i32 = 3;
pub const LC_NUMERIC: i32 = 4;
pub const LC_TIME: i32 = 5;
pub const LC_MESSAGES: i32 = 6;
pub const C_NAME: [u8; 2] = *b"C\0";
pub const NAME_LIMIT: usize = 255;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmptyEnvironmentPolicy {
    #[default]
    Unconfigured,
    /// Caller explicitly chooses a guest environment resolving all categories
    /// to C. This does not read or modify macOS environment variables.
    C,
}
#[derive(Debug, Default)]
pub struct GuestLocale {
    empty_environment: EmptyEnvironmentPolicy,
}
impl GuestLocale {
    pub fn configure_empty_environment(&mut self, policy: EmptyEnvironmentPolicy) {
        self.empty_environment = policy;
    }
    /// True requests the canonical readonly C name from the executor; false
    /// requests a guest NULL result. Every supported category remains C.
    pub fn setlocale(&self, memory: &Memory<'_>, category: i32, name: u64) -> Result<bool> {
        if !(LC_ALL..=LC_MESSAGES).contains(&category) {
            return Ok(false);
        }
        if name == 0 {
            return Ok(true);
        }
        let mut bytes = Vec::new();
        for offset in 0..=NAME_LIMIT {
            let byte = memory.load(
                name.checked_add(offset as u64)
                    .ok_or("Locale name address overflow")?,
                1,
            )? as u8;
            if byte == 0 {
                return match bytes.as_slice() {
                    b"C" | b"POSIX" => Ok(true),
                    b"" => match self.empty_environment {
                        EmptyEnvironmentPolicy::C => Ok(true),
                        EmptyEnvironmentPolicy::Unconfigured => Err(
                            "Empty locale name requires explicit guest environment policy".into(),
                        ),
                    },
                    _ => Ok(false),
                };
            }
            if offset < NAME_LIMIT {
                bytes.push(byte);
            }
        }
        Err("Locale name exceeds bounded limit".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn categories_queries_c_and_posix_are_owned() {
        assert_eq!(
            [
                LC_ALL,
                LC_COLLATE,
                LC_CTYPE,
                LC_MONETARY,
                LC_NUMERIC,
                LC_TIME,
                LC_MESSAGES
            ],
            [0, 1, 2, 3, 4, 5, 6]
        );
        let mut bytes = *b"C\0POSIX\0fr_FR.UTF-8\0\0";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, false, false).unwrap();
        let mut locale = GuestLocale::default();
        let other = GuestLocale::default();
        for category in 0..=6 {
            for name in [0, 0x1000, 0x1002] {
                assert!(locale.setlocale(&memory, category, name).unwrap());
            }
            assert!(!locale.setlocale(&memory, category, 0x1008).unwrap());
        }
        assert!(locale.setlocale(&memory, 5, 0x1014).is_err());
        locale.configure_empty_environment(EmptyEnvironmentPolicy::C);
        assert!(locale.setlocale(&memory, 5, 0x1014).unwrap());
        assert!(other.setlocale(&memory, 5, 0x1014).is_err());
        for category in [-1, 7, i32::MAX] {
            assert!(!locale.setlocale(&memory, category, 0).unwrap());
        }
        assert!(locale.setlocale(&memory, 0, 0).unwrap());
    }
    #[test]
    fn invalid_strings_fail_without_changing_current_locale() {
        let mut bytes = [65; NAME_LIMIT + 1];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, false, false).unwrap();
        let locale = GuestLocale::default();
        for name in [1, 0x1000, 0x10ff, u64::MAX] {
            assert!(locale.setlocale(&memory, 0, name).is_err());
            assert!(locale.setlocale(&memory, 0, 0).unwrap());
        }
        assert_eq!(
            memory.read(0x1000, NAME_LIMIT + 1).unwrap(),
            [65; NAME_LIMIT + 1]
        );
    }
}
