//! Incremental FreeBSD11.4 vfprintf format scanning. No output or %n writes.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vfprintf.c
//! Positional argument-table discovery, mixed/repeated nonstandard lengths and
//! unknown conversions are explicit boundaries. Guest bytes remain live: literal
//! spans are not copied, and star arguments are fetched during scanning.

pub trait Host {
    type Fault;
    fn read_byte(&mut self, address: u64) -> Result<u8, Self::Fault>;
    /// The parser currently calls only None. Some requires an audited positional
    /// type-table pass, which must not reuse a sequential argument cursor.
    fn fetch_gp(&mut self, positional: Option<usize>) -> Result<u64, Self::Fault>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestSpan {
    pub address: u64,
    pub len: usize,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub alternate: bool,
    pub left: bool,
    pub plus: bool,
    pub space: bool,
    pub zero: bool,
    pub grouping: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Length {
    #[default]
    Default,
    Char,
    Short,
    Long,
    LongLong,
    IntMax,
    Size,
    PtrDiff,
    LongDouble,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub flags: Flags,
    pub width: usize,
    pub precision: Option<usize>,
    pub length: Length,
    /// D/O/U/C/S are normalized to d/o/u/c/s plus Long length. '%' consumes
    /// no value; 'n' only describes a later count write, performed by the caller.
    pub conversion: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    Literal(GuestSpan),
    Conversion(Conversion),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Positional,
    LengthCombination,
    UnknownConversion(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bound {
    FormatReads,
    Width,
    Precision,
    NumericOverflow,
    AddressOverflow,
    InvalidLimits,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Guest(E),
    Unsupported(Unsupported),
    Bound(Bound),
    Poisoned,
}
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub reads: usize,
    pub width: usize,
    pub precision: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            reads: 65536,
            width: 1_048_576,
            precision: 1_048_576,
        }
    }
}
#[derive(Clone, Copy)]
enum State {
    Scan,
    Conversion,
    Done,
    Failed,
}
pub struct Parser {
    cursor: u64,
    remaining_reads: usize,
    limits: Limits,
    state: State,
}
impl Parser {
    pub fn new(address: u64, limits: Limits) -> Result<Self, Bound> {
        if limits.reads == 0
            || limits.width > i32::MAX as usize
            || limits.precision > i32::MAX as usize
        {
            return Err(Bound::InvalidLimits);
        }
        Ok(Self {
            cursor: address,
            remaining_reads: limits.reads,
            limits,
            state: State::Scan,
        })
    }
    /// An error is terminal for this parser: already-fetched '*' arguments cannot
    /// be replayed. A caller must not retry using a partially consumed va_list.
    pub fn next<H: Host>(&mut self, host: &mut H) -> Result<Option<Item>, Error<H::Fault>> {
        if matches!(self.state, State::Failed) {
            return Err(Error::Poisoned);
        }
        let result = self.next_inner(host);
        if result.is_err() {
            self.state = State::Failed;
        }
        result
    }
    fn read<H: Host>(&mut self, host: &mut H, address: u64) -> Result<u8, Error<H::Fault>> {
        self.remaining_reads = self
            .remaining_reads
            .checked_sub(1)
            .ok_or(Error::Bound(Bound::FormatReads))?;
        host.read_byte(address).map_err(Error::Guest)
    }
    fn take<H: Host>(&mut self, host: &mut H) -> Result<u8, Error<H::Fault>> {
        let byte = self.read(host, self.cursor)?;
        self.cursor = self
            .cursor
            .checked_add(1)
            .ok_or(Error::Bound(Bound::AddressOverflow))?;
        Ok(byte)
    }
    fn number<E>(value: usize, byte: u8) -> Result<usize, Error<E>> {
        value
            .checked_mul(10)
            .and_then(|n| n.checked_add((byte - b'0') as usize))
            .filter(|&n| n <= i32::MAX as usize)
            .ok_or(Error::Bound(Bound::NumericOverflow))
    }
    fn star<H: Host>(&mut self, host: &mut H) -> Result<i32, Error<H::Fault>> {
        // GETASTER peeks ahead for *nn$ before fetching anything. With no '$'
        // fmt stays at the first character after '*', even if it was a digit.
        let mut at = self.cursor;
        loop {
            let byte = self.read(host, at)?;
            if !byte.is_ascii_digit() {
                if byte == b'$' {
                    return Err(Error::Unsupported(Unsupported::Positional));
                }
                break;
            }
            at = at
                .checked_add(1)
                .ok_or(Error::Bound(Bound::AddressOverflow))?;
        }
        Ok(host.fetch_gp(None).map_err(Error::Guest)? as u32 as i32)
    }
    fn next_inner<H: Host>(&mut self, host: &mut H) -> Result<Option<Item>, Error<H::Fault>> {
        if matches!(self.state, State::Done) {
            return Ok(None);
        }
        if matches!(self.state, State::Scan) {
            let start = self.cursor;
            loop {
                let byte = self.read(host, self.cursor)?;
                if byte == 0 || byte == b'%' {
                    let len = usize::try_from(self.cursor - start)
                        .map_err(|_| Error::Bound(Bound::FormatReads))?;
                    self.state = if byte == 0 {
                        State::Done
                    } else {
                        State::Conversion
                    };
                    if byte == b'%' {
                        self.cursor = self
                            .cursor
                            .checked_add(1)
                            .ok_or(Error::Bound(Bound::AddressOverflow))?;
                    }
                    if len != 0 {
                        return Ok(Some(Item::Literal(GuestSpan {
                            address: start,
                            len,
                        })));
                    }
                    if byte == 0 {
                        return Ok(None);
                    }
                    break;
                }
                self.cursor = self
                    .cursor
                    .checked_add(1)
                    .ok_or(Error::Bound(Bound::AddressOverflow))?;
            }
        }
        let mut spec = Conversion {
            flags: Flags::default(),
            width: 0,
            precision: None,
            length: Length::Default,
            conversion: 0,
        };
        let mut ch = self.take(host)?;
        loop {
            match ch {
                b' ' => {
                    if !spec.flags.plus {
                        spec.flags.space = true;
                    }
                }
                b'+' => {
                    spec.flags.plus = true;
                    spec.flags.space = false;
                }
                b'#' => spec.flags.alternate = true,
                b'-' => spec.flags.left = true,
                b'0' => spec.flags.zero = true,
                b'\'' => spec.flags.grouping = true,
                b'*' => {
                    let width = self.star(host)?;
                    let magnitude = width.checked_abs().ok_or(Error::Bound(Bound::Width))? as usize;
                    if magnitude > self.limits.width {
                        return Err(Error::Bound(Bound::Width));
                    }
                    spec.width = magnitude;
                    if width < 0 {
                        spec.flags.left = true;
                    }
                }
                b'.' => {
                    ch = self.take(host)?;
                    if ch == b'*' {
                        let precision = self.star(host)?;
                        if precision >= 0 && precision as usize > self.limits.precision {
                            return Err(Error::Bound(Bound::Precision));
                        }
                        spec.precision = (precision >= 0).then_some(precision as usize);
                    } else {
                        let mut precision = 0;
                        while ch.is_ascii_digit() {
                            precision = Self::number(precision, ch)?;
                            ch = self.take(host)?;
                        }
                        if precision > self.limits.precision {
                            return Err(Error::Bound(Bound::Precision));
                        }
                        spec.precision = Some(precision);
                        continue;
                    }
                }
                b'1'..=b'9' => {
                    let mut width = 0;
                    while ch.is_ascii_digit() {
                        width = Self::number(width, ch)?;
                        ch = self.take(host)?;
                    }
                    if ch == b'$' {
                        return Err(Error::Unsupported(Unsupported::Positional));
                    }
                    if width > self.limits.width {
                        return Err(Error::Bound(Bound::Width));
                    }
                    spec.width = width;
                    continue;
                }
                b'h' | b'l' | b'j' | b'z' | b't' | b'L' | b'q' => {
                    spec.length = match (spec.length, ch) {
                        (Length::Default, b'h') => Length::Short,
                        (Length::Short, b'h') => Length::Char,
                        (Length::Default, b'l') => Length::Long,
                        (Length::Long, b'l') => Length::LongLong,
                        (Length::Default, b'j') => Length::IntMax,
                        (Length::Default, b'z') => Length::Size,
                        (Length::Default, b't') => Length::PtrDiff,
                        (Length::Default, b'L') => Length::LongDouble,
                        (Length::Default, b'q') => Length::LongLong,
                        _ => return Err(Error::Unsupported(Unsupported::LengthCombination)),
                    };
                }
                0 => {
                    self.state = State::Done;
                    return Ok(None);
                }
                _ => {
                    if matches!(ch, b'D' | b'O' | b'U' | b'C' | b'S') {
                        if !matches!(spec.length, Length::Default | Length::Long) {
                            return Err(Error::Unsupported(Unsupported::LengthCombination));
                        }
                        spec.length = Length::Long;
                        ch = ch.to_ascii_lowercase();
                    }
                    if !matches!(
                        ch,
                        b'd' | b'i'
                            | b'o'
                            | b'u'
                            | b'x'
                            | b'X'
                            | b'p'
                            | b'c'
                            | b's'
                            | b'n'
                            | b'm'
                            | b'%'
                            | b'f'
                            | b'F'
                            | b'e'
                            | b'E'
                            | b'g'
                            | b'G'
                            | b'a'
                            | b'A'
                    ) {
                        return Err(Error::Unsupported(Unsupported::UnknownConversion(ch)));
                    }
                    spec.conversion = ch;
                    self.state = State::Scan;
                    return Ok(Some(Item::Conversion(spec)));
                }
            }
            ch = self.take(host)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    #[derive(Debug, PartialEq, Eq)]
    enum Access {
        Read(u64),
        Fetch,
    }
    struct Fixture {
        bytes: Vec<u8>,
        args: VecDeque<u64>,
        accesses: Vec<Access>,
        rewrite: Option<(usize, u8)>,
    }
    impl Fixture {
        fn new(bytes: &[u8], args: &[u64]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                args: args.iter().copied().collect(),
                accesses: Vec::new(),
                rewrite: None,
            }
        }
    }
    impl Host for Fixture {
        type Fault = &'static str;
        fn read_byte(&mut self, address: u64) -> Result<u8, Self::Fault> {
            self.accesses.push(Access::Read(address));
            self.bytes.get(address as usize).copied().ok_or("read")
        }
        fn fetch_gp(&mut self, positional: Option<usize>) -> Result<u64, Self::Fault> {
            assert_eq!(positional, None);
            self.accesses.push(Access::Fetch);
            if let Some((at, b)) = self.rewrite.take() {
                self.bytes[at] = b;
            }
            self.args.pop_front().ok_or("argument")
        }
    }
    fn parser() -> Parser {
        Parser::new(0, Limits::default()).unwrap()
    }
    fn conversion(parser: &mut Parser, fixture: &mut Fixture) -> Conversion {
        let Some(Item::Conversion(s)) = parser.next(fixture).unwrap() else {
            panic!("Expected conversion")
        };
        s
    }
    #[test]
    fn literals_remain_spans_and_cache_boundary() {
        let mut f = Fixture::new(b"abc%d\0", &[]);
        let mut p = parser();
        assert_eq!(
            p.next(&mut f).unwrap(),
            Some(Item::Literal(GuestSpan { address: 0, len: 3 }))
        );
        assert_eq!(
            f.accesses,
            vec![
                Access::Read(0),
                Access::Read(1),
                Access::Read(2),
                Access::Read(3)
            ]
        );
        f.bytes[3] = b'!';
        assert_eq!(conversion(&mut p, &mut f).conversion, b'd');
        assert_eq!(f.accesses[4], Access::Read(4));
        assert_eq!(p.next(&mut f).unwrap(), None);
        let mut f = Fixture::new(b"abc\0", &[]);
        let mut p = parser();
        assert!(matches!(p.next(&mut f).unwrap(), Some(Item::Literal(_))));
        f.bytes[3] = b'%';
        let count = f.accesses.len();
        assert_eq!(p.next(&mut f).unwrap(), None);
        assert_eq!(f.accesses.len(), count);
    }
    #[test]
    fn star_fetches_interleave_with_format_reads_and_aliases() {
        let mut f = Fixture::new(b"%*.*d\0", &[(-5i32) as u32 as u64, (-1i32) as u32 as u64]);
        let s = conversion(&mut parser(), &mut f);
        assert_eq!(s.width, 5);
        assert!(s.flags.left);
        assert_eq!(s.precision, None);
        assert_eq!(
            f.accesses,
            vec![
                Access::Read(0),
                Access::Read(1),
                Access::Read(2),
                Access::Fetch,
                Access::Read(2),
                Access::Read(3),
                Access::Read(4),
                Access::Fetch,
                Access::Read(4)
            ]
        );
        let mut f = Fixture::new(b"%*d\0", &[0xfeed00000003]);
        f.rewrite = Some((2, b's'));
        let s = conversion(&mut parser(), &mut f);
        assert_eq!(s.width, 3);
        assert_eq!(s.conversion, b's');
        let mut f = Fixture::new(b"%*12d\0", &[7]);
        let s = conversion(&mut parser(), &mut f);
        assert_eq!(s.width, 12);
        assert_eq!(
            f.accesses.iter().filter(|a| **a == Access::Fetch).count(),
            1
        );
    }
    #[test]
    fn positional_never_falls_back_to_sequential() {
        for bytes in [
            &b"%2$d\0"[..],
            &b"%*2$d\0"[..],
            &b"%.*3$d\0"[..],
            &b"%*$d\0"[..],
        ] {
            let mut f = Fixture::new(bytes, &[17]);
            let mut p = parser();
            assert_eq!(
                p.next(&mut f),
                Err(Error::Unsupported(Unsupported::Positional))
            );
            assert!(!f.accesses.contains(&Access::Fetch));
            assert_eq!(f.args.len(), 1);
        }
    }
    #[test]
    fn flags_lengths_aliases_and_count_write_metadata() {
        let mut f = Fixture::new(b"% +#0-'12.3lld%n%hhn%zn%D%S%%\0", &[]);
        let mut p = parser();
        let s = conversion(&mut p, &mut f);
        assert_eq!(s.width, 12);
        assert_eq!(s.precision, Some(3));
        assert_eq!(s.length, Length::LongLong);
        assert_eq!(
            s.flags,
            Flags {
                alternate: true,
                left: true,
                plus: true,
                space: false,
                zero: true,
                grouping: true
            }
        );
        for (length, byte) in [
            (Length::Default, b'n'),
            (Length::Char, b'n'),
            (Length::Size, b'n'),
            (Length::Long, b'd'),
            (Length::Long, b's'),
            (Length::Default, b'%'),
        ] {
            let s = conversion(&mut p, &mut f);
            assert_eq!((s.length, s.conversion), (length, byte));
        }
        assert!(!f.accesses.contains(&Access::Fetch));
        for (text, length) in [
            (&b"%hd\0"[..], Length::Short),
            (&b"%jd\0"[..], Length::IntMax),
            (&b"%td\0"[..], Length::PtrDiff),
            (&b"%Lf\0"[..], Length::LongDouble),
            (&b"%qd\0"[..], Length::LongLong),
        ] {
            assert_eq!(
                conversion(&mut parser(), &mut Fixture::new(text, &[])).length,
                length
            );
        }
    }
    #[test]
    fn empty_precision_trailing_percent_and_explicit_boundaries() {
        assert_eq!(
            conversion(&mut parser(), &mut Fixture::new(b"%.d\0", &[])).precision,
            Some(0)
        );
        for text in [&b"%\0"[..], &b"%.\0"[..], &b"%ll\0"[..]] {
            assert_eq!(parser().next(&mut Fixture::new(text, &[])).unwrap(), None);
        }
        for text in [&b"%hhh\0"[..], &b"%hlx\0"[..], &b"%lll\0"[..]] {
            assert_eq!(
                parser().next(&mut Fixture::new(text, &[])),
                Err(Error::Unsupported(Unsupported::LengthCombination))
            );
        }
        assert_eq!(
            parser().next(&mut Fixture::new(b"%?\0", &[])),
            Err(Error::Unsupported(Unsupported::UnknownConversion(b'?')))
        );
    }
    #[test]
    fn failure_order_and_no_replay() {
        let mut f = Fixture::new(b"%d", &[]);
        let mut p = parser();
        assert_eq!(conversion(&mut p, &mut f).conversion, b'd');
        assert_eq!(p.next(&mut f), Err(Error::Guest("read")));
        let mut f = Fixture::new(b"%*", &[3]);
        assert_eq!(parser().next(&mut f), Err(Error::Guest("read")));
        assert!(!f.accesses.contains(&Access::Fetch));
        let mut f = Fixture::new(b"%*d\0", &[]);
        let mut p = parser();
        assert_eq!(p.next(&mut f), Err(Error::Guest("argument")));
        let count = f.accesses.len();
        assert_eq!(p.next(&mut f), Err(Error::Poisoned));
        assert_eq!(f.accesses.len(), count);
    }
    #[test]
    fn resource_limits_are_not_guest_faults() {
        for (text, args, expected) in [
            (&b"%11d\0"[..], &[][..], Bound::Width),
            (&b"%.11d\0"[..], &[][..], Bound::Precision),
            (&b"%*d\0"[..], &[0x80000000][..], Bound::Width),
            (&b"%2147483648d\0"[..], &[][..], Bound::NumericOverflow),
        ] {
            let mut p = Parser::new(
                0,
                Limits {
                    reads: 100,
                    width: 10,
                    precision: 10,
                },
            )
            .unwrap();
            assert_eq!(
                p.next(&mut Fixture::new(text, args)),
                Err(Error::Bound(expected))
            );
        }
        let mut p = Parser::new(
            0,
            Limits {
                reads: 2,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(
            p.next(&mut Fixture::new(b"abc\0", &[])),
            Err(Error::Bound(Bound::FormatReads))
        );
        assert!(matches!(
            Parser::new(
                0,
                Limits {
                    reads: 0,
                    ..Limits::default()
                }
            ),
            Err(Bound::InvalidLimits)
        ));
    }
}
