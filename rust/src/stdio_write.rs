//! Unbuffered stream access policy, independently of descriptor storage.
//! Primary sources: FreeBSD releng/11.4 libc/stdio/flags.c, fwrite.c,
//! wsetup.c and refill.c. Verified-execution (`v`) opens remain unsupported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
    pub exclusive: bool,
    pub cloexec: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeError {
    Invalid,
    VerifyUnsupported,
}
impl Mode {
    pub fn parse(bytes: &[u8]) -> Result<Self, ModeError> {
        let first = *bytes.first().ok_or(ModeError::Invalid)?;
        if !matches!(first, b'r' | b'w' | b'a') {
            return Err(ModeError::Invalid);
        }
        let mut mode = Self {
            read: first == b'r',
            write: first != b'r',
            create: first != b'r',
            truncate: first == b'w',
            append: first == b'a',
            exclusive: false,
            cloexec: false,
        };
        let mut verify = false;
        // __sflags stops at the first unknown byte, including the terminator.
        for byte in &bytes[1..] {
            match byte {
                b'b' => {}
                b'+' => {
                    mode.read = true;
                    mode.write = true;
                }
                b'x' => mode.exclusive = true,
                b'e' => mode.cloexec = true,
                b'v' => verify = true,
                _ => break,
            }
        }
        if mode.exclusive && !mode.write {
            return Err(ModeError::Invalid);
        }
        if verify {
            return Err(ModeError::VerifyUnsupported);
        }
        Ok(mode)
    }
    pub fn initial_direction(self) -> Direction {
        if self.read && self.write {
            Direction::Neither
        } else if self.read {
            Direction::Read
        } else {
            Direction::Write
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Neither,
    Read,
    Write,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WritePreparation {
    pub total: u64,
    pub errno: Option<u32>,
    pub orient: bool,
    pub clear_eof: bool,
    pub direction: Direction,
}
/// This precedes any source-memory access; the caller commits resulting FILE
/// metadata only after guarding it and any conditional errno output.
pub fn prepare(mode: Mode, direction: Direction, size: u64, count: u64) -> WritePreparation {
    let mut result = WritePreparation {
        total: 0,
        errno: None,
        orient: false,
        clear_eof: false,
        direction,
    };
    if size == 0 || count == 0 {
        return result;
    }
    let Some(total) = size.checked_mul(count) else {
        result.errno = Some(22);
        return result;
    };
    result.orient = true;
    if !mode.write {
        result.errno = Some(9);
        return result;
    }
    result.total = total;
    result.clear_eof = direction == Direction::Read;
    result.direction = Direction::Write;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_follow_freebsd_modifier_order_and_termination() {
        for mode in [b"r+".as_slice(), b"rb+", b"r+b", b"rbb++e"] {
            let parsed = Mode::parse(mode).unwrap();
            assert!(parsed.read && parsed.write && !parsed.create);
            assert_eq!(parsed.initial_direction(), Direction::Neither);
        }
        assert_eq!(Mode::parse(b"rx"), Err(ModeError::Invalid));
        assert!(Mode::parse(b"rx+").unwrap().exclusive);
        assert_eq!(Mode::parse(b"rvx"), Err(ModeError::Invalid));
        assert_eq!(Mode::parse(b"rv"), Err(ModeError::VerifyUnsupported));
        assert_eq!(Mode::parse(b"r?+v"), Mode::parse(b"r"));
        assert_eq!(Mode::parse(b"w\0+"), Mode::parse(b"w"));
        assert!(Mode::parse(b"a+xe").unwrap().append);
        assert!(Mode::parse(b"w").unwrap().truncate);
        for bad in [b"".as_slice(), b"b", b"+", b"R"] {
            assert_eq!(Mode::parse(bad), Err(ModeError::Invalid));
        }
    }
    #[test]
    fn write_access_order_and_update_direction() {
        let ro = Mode::parse(b"r").unwrap();
        let zero = prepare(ro, Direction::Read, 0, u64::MAX);
        assert_eq!(zero.errno, None);
        assert!(!zero.orient);
        let overflow = prepare(ro, Direction::Read, 2, u64::MAX);
        assert_eq!(overflow.errno, Some(22));
        assert!(!overflow.orient);
        let denied = prepare(ro, Direction::Read, 1, 1);
        assert_eq!(denied.errno, Some(9));
        assert!(denied.orient);
        assert!(!denied.clear_eof);
        let update = prepare(Mode::parse(b"r+").unwrap(), Direction::Read, 2, 3);
        assert_eq!(update.total, 6);
        assert!(update.clear_eof);
        assert_eq!(update.direction, Direction::Write);
    }
}
