//! Bounded C-locale strftime model using the pinned SDK's LP64 struct tm.
//! Sources: FreeBSD releng/11.4 libc/stdtime/{strftime.c,timelocal.c}; SDK
//! 10b539de8f2b6773a9a78caa7576a97dbd99b29a include/freebsd/time.h.
//! This is original Rust code, with no host calendar/locale/timezone calls.
//! %s (mktime), arbitrary locales/TZ databases and tzset/warning side effects
//! remain explicit boundaries. UTC fallback is opt-in; supplied tm_zone works
//! independently. Guard faults reject a plan atomically; valid capacity failure
//! preserves its source-defined prefix without adding a terminator.
use crate::guest_memory::Memory;
const LIMIT: usize = 65536;
#[derive(Clone, Copy, Debug, Default)]
pub enum Timezone {
    #[default]
    Unconfigured,
    Utc,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Policy {
    pub timezone: Timezone,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Memory(String),
    Bound,
    Unsupported(&'static str),
    Alias,
}
impl From<String> for Error {
    fn from(e: String) -> Self {
        Self::Memory(e)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Returned(u64),
    Unsupported(&'static str),
}
pub struct Plan {
    pub destination: u64,
    pub bytes: Vec<u8>,
    /// Unsupported must fault after guarded prefix commit; it is not return0.
    pub completion: Completion,
}
impl Plan {
    /// Caller guards the actual byte range against live return frames first.
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<(), Error> {
        if !self.bytes.is_empty() {
            memory.write(self.destination, &self.bytes)?;
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Format<'a> {
    Guest(u64),
    Static(&'a [u8]),
}
#[derive(Clone, Copy)]
enum Pad {
    Default,
    None,
    Space,
    Zero,
}
struct Engine<'a, 'm> {
    memory: &'a Memory<'m>,
    tm: u64,
    policy: Policy,
    bytes: Vec<u8>,
    capacity: usize,
    reads: Vec<(u64, usize)>,
    budget: usize,
    trailing_nul: bool,
}
impl Engine<'_, '_> {
    fn read(&mut self, address: u64, size: usize) -> Result<&[u8], Error> {
        self.budget = self.budget.checked_sub(size.max(1)).ok_or(Error::Bound)?;
        address.checked_add(size as u64).ok_or(Error::Bound)?;
        self.reads.try_reserve(1).map_err(|_| Error::Bound)?;
        self.reads.push((address, size));
        Ok(self.memory.read(address, size)?)
    }
    fn field(&mut self, index: u64) -> Result<i64, Error> {
        let address = self.tm.checked_add(index * 4).ok_or(Error::Bound)?;
        Ok(i32::from_le_bytes(self.read(address, 4)?.try_into().unwrap()) as i64)
    }
    fn long(&mut self, offset: u64) -> Result<u64, Error> {
        let address = self.tm.checked_add(offset).ok_or(Error::Bound)?;
        Ok(u64::from_le_bytes(
            self.read(address, 8)?.try_into().unwrap(),
        ))
    }
    fn ch(&mut self, format: Format<'_>, index: usize) -> Result<u8, Error> {
        match format {
            Format::Guest(base) => {
                Ok(self.read(base.checked_add(index as u64).ok_or(Error::Bound)?, 1)?[0])
            }
            Format::Static(bytes) => Ok(*bytes.get(index).unwrap_or(&0)),
        }
    }
    fn add(&mut self, text: &[u8]) -> Result<(), Error> {
        let count = text.len().min(self.capacity - self.bytes.len());
        self.bytes.try_reserve(count).map_err(|_| Error::Bound)?;
        self.bytes.extend_from_slice(&text[..count]);
        self.trailing_nul = self.bytes.len() < self.capacity;
        Ok(())
    }
    fn number(&mut self, value: i64, width: usize, pad: Pad, default: u8) -> Result<(), Error> {
        let mut raw = [0u8; 24];
        let mut at = raw.len();
        let mut magnitude = value.unsigned_abs();
        loop {
            at -= 1;
            raw[at] = b'0' + (magnitude % 10) as u8;
            magnitude /= 10;
            if magnitude == 0 {
                break;
            }
        }
        let digits = &raw[at..];
        let sign = usize::from(value < 0);
        let count = if matches!(pad, Pad::None) {
            0
        } else {
            width.saturating_sub(digits.len() + sign)
        };
        let padding = match pad {
            Pad::Space => b' ',
            Pad::Zero => b'0',
            _ => default,
        };
        if padding == b' ' {
            for _ in 0..count {
                self.add(b" ")?;
            }
        }
        if value < 0 {
            self.add(b"-")?;
        }
        if padding == b'0' {
            for _ in 0..count {
                self.add(b"0")?;
            }
        }
        self.add(digits)
    }
    fn year(&mut self, year: i64, top: bool, tail: bool) -> Result<(), Error> {
        // FreeBSD year formatting deliberately ignores padding flags.
        if top {
            let lead = year / 100;
            if lead == 0 && year < 0 {
                self.add(b"-0")?;
            } else {
                self.number(lead, 2, Pad::Zero, b'0')?;
            }
        }
        if tail {
            self.number((year % 100).abs(), 2, Pad::Zero, b'0')?;
        }
        Ok(())
    }
    fn zone(&mut self) -> Result<(), Error> {
        let pointer = self.long(48)?;
        if pointer != 0 {
            let mut index = 0;
            while self.bytes.len() < self.capacity {
                if index == LIMIT {
                    return Err(Error::Bound);
                }
                let byte = self.read(pointer.checked_add(index as u64).ok_or(Error::Bound)?, 1)?[0];
                if byte == 0 {
                    self.add(b"")?;
                    break;
                }
                self.add(&[byte])?;
                index += 1;
            }
        } else if self.field(8)? >= 0 {
            match self.policy.timezone {
                Timezone::Utc => self.add(b"UTC")?,
                Timezone::Unconfigured => {
                    return Err(Error::Unsupported(
                        "tzname fallback requires timezone policy",
                    ));
                }
            }
        }
        Ok(())
    }
    fn iso(&mut self) -> Result<(i64, i64), Error> {
        let year = self.field(5)? + 1900;
        let yday = self.field(7)?;
        let wday = self.field(6)?;
        if !(0..=365).contains(&yday) || !(0..=6).contains(&wday) {
            return Err(Error::Unsupported(
                "ISO week requires normalized weekday/year-day",
            ));
        }
        let january = (wday - yday.rem_euclid(7)).rem_euclid(7);
        let monday_index = if wday == 0 { 7 } else { wday };
        let week = (yday + 1 - monday_index + 10) / 7;
        let weeks = |year: i64, jan: i64| {
            if jan == 4 || (jan == 3 && leap(year)) {
                53
            } else {
                52
            }
        };
        if week == 0 {
            let previous = year - 1;
            let previous_jan = (january - if leap(previous) { 2 } else { 1 }).rem_euclid(7);
            Ok((previous, weeks(previous, previous_jan)))
        } else if week > weeks(year, january) {
            Ok((year + 1, 1))
        } else {
            Ok((year, week))
        }
    }
    fn format(&mut self, format: Format<'_>, depth: usize) -> Result<(), Error> {
        if depth > 8 {
            return Err(Error::Bound);
        }
        let mut index = 0;
        loop {
            let mut byte = self.ch(format, index)?;
            if byte == 0 {
                break;
            }
            index += 1;
            if byte != b'%' {
                if self.bytes.len() == self.capacity {
                    break;
                }
                self.add(&[byte])?;
                self.trailing_nul = false;
                continue;
            }
            let mut pad = Pad::Default;
            let (mut e, mut o) = (false, false);
            loop {
                byte = self.ch(format, index)?;
                index += 1;
                match byte {
                    b'E' if !e && !o => e = true,
                    b'O' if !e && !o => o = true,
                    b'-' if matches!(pad, Pad::Default) => pad = Pad::None,
                    b'_' if matches!(pad, Pad::Default) => pad = Pad::Space,
                    b'0' if matches!(pad, Pad::Default) => pad = Pad::Zero,
                    _ => break,
                }
            }
            let nested: Option<&[u8]> = match byte {
                b'c' => Some(b"%a %b %e %H:%M:%S %Y".as_slice()),
                b'x' | b'D' => Some(b"%m/%d/%y"),
                b'X' | b'T' => Some(b"%H:%M:%S"),
                b'F' => Some(b"%Y-%m-%d"),
                b'R' => Some(b"%H:%M"),
                b'r' => Some(b"%I:%M:%S %p"),
                b'v' => Some(b"%e-%b-%Y"),
                b'+' => Some(b"%a %b %e %H:%M:%S %Z %Y"),
                _ => None,
            };
            if let Some(nested) = nested {
                self.format(Format::Static(nested), depth + 1)?;
                continue;
            }
            match byte {
                b'A' | b'a' => {
                    let day = self.field(6)?;
                    let name = if (0..7).contains(&day) {
                        if byte == b'A' {
                            DAYS[day as usize]
                        } else {
                            &DAYS[day as usize][..3]
                        }
                    } else {
                        b"?"
                    };
                    self.add(name)?;
                }
                b'B' | b'b' | b'h' => {
                    let month = self.field(4)?;
                    let name = if (0..12).contains(&month) {
                        if byte == b'B' {
                            MONTHS[month as usize]
                        } else {
                            &MONTHS[month as usize][..3]
                        }
                    } else {
                        b"?"
                    };
                    self.add(name)?;
                }
                b'C' | b'Y' | b'y' => {
                    let year = self.field(5)? + 1900;
                    self.year(year, byte != b'y', byte != b'C')?;
                }
                b'd' | b'e' => {
                    let n = self.field(3)?;
                    self.number(n, 2, pad, if byte == b'e' { b' ' } else { b'0' })?;
                }
                b'H' | b'k' | b'I' | b'l' => {
                    let h = self.field(2)?;
                    let n = if matches!(byte, b'I' | b'l') {
                        if h % 12 == 0 { 12 } else { h % 12 }
                    } else {
                        h
                    };
                    self.number(
                        n,
                        2,
                        pad,
                        if matches!(byte, b'k' | b'l') {
                            b' '
                        } else {
                            b'0'
                        },
                    )?;
                }
                b'M' | b'S' => {
                    let n = self.field(if byte == b'M' { 1 } else { 0 })?;
                    self.number(n, 2, pad, b'0')?;
                }
                b'm' | b'j' => {
                    let n = self
                        .field(if byte == b'm' { 4 } else { 7 })?
                        .checked_add(1)
                        .filter(|n| i32::try_from(*n).is_ok())
                        .ok_or(Error::Bound)?;
                    self.number(n, if byte == b'j' { 3 } else { 2 }, pad, b'0')?;
                }
                b'p' => {
                    let hour = self.field(2)?;
                    self.add(if hour >= 12 { b"PM" } else { b"AM" })?;
                }
                b'U' | b'W' => {
                    let day = self.field(7)?;
                    let wd = self.field(6)?;
                    let shifted = if byte == b'W' {
                        if wd == 0 { 6 } else { wd - 1 }
                    } else {
                        wd
                    };
                    self.number((day + 7 - shifted) / 7, 2, pad, b'0')?;
                }
                b'w' | b'u' => {
                    let day = self.field(6)?;
                    self.number(
                        if byte == b'u' && day == 0 { 7 } else { day },
                        0,
                        Pad::None,
                        b'0',
                    )?;
                }
                b'V' | b'G' | b'g' => {
                    let (year, week) = self.iso()?;
                    if byte == b'V' {
                        self.number(week, 2, pad, b'0')?;
                    } else {
                        self.year(year, byte == b'G', true)?;
                    }
                }
                b'n' => self.add(b"\n")?,
                b't' => self.add(b"\t")?,
                b'Z' => self.zone()?,
                b'z' => {
                    if self.field(8)? >= 0 {
                        let seconds = self.long(40)? as i64;
                        let seconds = i32::try_from(seconds).map_err(|_| {
                            Error::Unsupported("timezone offset outside source int domain")
                        })?;
                        if seconds == i32::MIN {
                            return Err(Error::Bound);
                        }
                        self.add(if seconds < 0 { b"-" } else { b"+" })?;
                        let minutes = i64::from(seconds.abs()) / 60;
                        self.number(minutes / 60 * 100 + minutes % 60, 4, pad, b'0')?;
                    }
                }
                b's' => return Err(Error::Unsupported("strftime %s requires guest mktime")),
                // Unknown conversion and repeated modifiers print the byte;
                // trailing '%' or modifier prints its preceding character.
                _ => {
                    if self.bytes.len() == self.capacity {
                        break;
                    }
                    if byte == 0 {
                        index -= 1;
                        let previous = self.ch(format, index - 1)?;
                        self.add(&[previous])?;
                        self.trailing_nul = false;
                        break;
                    }
                    self.add(&[byte])?;
                    self.trailing_nul = false;
                }
            }
        }
        Ok(())
    }
}
const DAYS: [&[u8]; 7] = [
    b"Sunday",
    b"Monday",
    b"Tuesday",
    b"Wednesday",
    b"Thursday",
    b"Friday",
    b"Saturday",
];
const MONTHS: [&[u8]; 12] = [
    b"January",
    b"February",
    b"March",
    b"April",
    b"May",
    b"June",
    b"July",
    b"August",
    b"September",
    b"October",
    b"November",
    b"December",
];
fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
/// Plan the exact successful bytes (including NUL) or the full-capacity prefix
/// (return zero, no synthetic terminator). No errno access. Destination/source
/// overlap is rejected as outside the restrict-qualified source contract.
pub fn plan(
    memory: &Memory<'_>,
    destination: u64,
    capacity: u64,
    format: u64,
    tm: u64,
    policy: Policy,
) -> Result<Plan, Error> {
    let capacity = usize::try_from(capacity)
        .ok()
        .filter(|n| *n <= LIMIT)
        .ok_or(Error::Bound)?;
    destination
        .checked_add(capacity as u64)
        .ok_or(Error::Bound)?;
    let mut engine = Engine {
        memory,
        tm,
        policy,
        bytes: Vec::new(),
        capacity,
        reads: Vec::new(),
        budget: LIMIT * 8,
        trailing_nul: false,
    };
    let stopped = match engine.format(
        if format == 0 {
            Format::Static(b"%c")
        } else {
            Format::Guest(format)
        },
        0,
    ) {
        Ok(()) => None,
        Err(Error::Unsupported(reason)) => Some(reason),
        Err(error) => return Err(error),
    };
    let length = engine.bytes.len();
    let completion = if let Some(reason) = stopped {
        if engine.trailing_nul {
            engine.bytes.push(0);
        }
        Completion::Unsupported(reason)
    } else if length == capacity {
        Completion::Returned(0)
    } else {
        engine.add(b"\0")?;
        Completion::Returned(length as u64)
    };
    if !engine.bytes.is_empty() {
        let end = destination + engine.bytes.len() as u64;
        if engine
            .reads
            .iter()
            .any(|&(address, size)| destination < address + size as u64 && address < end)
        {
            return Err(Error::Alias);
        }
        memory.validate_write(destination, engine.bytes.len())?;
    }
    Ok(Plan {
        destination,
        bytes: engine.bytes,
        completion,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    const F: u64 = 0x1000;
    const T: u64 = 0x2000;
    const D: u64 = 0x3000;
    fn tm() -> [u8; 56] {
        let mut b = [0; 56];
        for (i, n) in [56i32, 34, 13, 13, 9, 126, 2, 285, 0]
            .into_iter()
            .enumerate()
        {
            b[i * 4..i * 4 + 4].copy_from_slice(&n.to_le_bytes());
        }
        b
    }
    fn check(format: &[u8], time: &mut [u8; 56], cap: u64) -> Plan {
        let mut f = format.to_vec();
        let mut d = vec![0xa5; 1024];
        let mut m = Memory::default();
        m.add(F, &mut f, false, false).unwrap();
        m.add(T, time, false, false).unwrap();
        m.add(D, &mut d, true, false).unwrap();
        plan(
            &m,
            D,
            cap,
            F,
            T,
            Policy {
                timezone: Timezone::Utc,
            },
        )
        .unwrap()
    }
    #[test]
    fn names_composites_numeric_and_c_locale() {
        let p = check(
            b"%c|%F|%x|%X|%r|%v|%A %B %h|%j %u %w|%C%y\0",
            &mut tm(),
            1024,
        );
        assert_eq!(p.bytes,b"Tue Oct 13 13:34:56 2026|2026-10-13|10/13/26|13:34:56|01:34:56 PM|13-Oct-2026|Tuesday October Oct|286 2 2|2026\0");
        assert_eq!(p.completion, Completion::Returned(p.bytes.len() as u64 - 1));
    }
    #[test]
    fn exact_capacity_preserves_prefix_without_nul() {
        for cap in 0..=5 {
            let p = check(b"abcd\0", &mut tm(), cap);
            assert_eq!(
                p.completion,
                Completion::Returned(if cap > 4 { 4 } else { 0 })
            );
            assert_eq!(
                p.bytes,
                if cap > 4 {
                    b"abcd\0".to_vec()
                } else {
                    b"abcd"[..cap as usize].to_vec()
                }
            );
        }
    }
    #[test]
    fn modifier_and_unknown_source_rules() {
        let mut t = tm();
        t[12..16].copy_from_slice(&3i32.to_le_bytes());
        assert_eq!(
            check(
                b"%d|%e|%-d|%_d|%0e|%Od|%OB|%EY|%-Y|%EE|%--d|%q|%%|%\0",
                &mut t,
                1024
            )
            .bytes,
            b"03| 3|3| 3|03|03|October|2026|2026|E|-d|q|%|%\0"
        );
        assert_eq!(check(b"%E\0", &mut t, 1024).bytes, b"E\0");
    }
    #[test]
    fn negative_and_large_years_have_source_century_rules() {
        for (year, want) in [
            (-1901, "-001|-0|01"),
            (-1900, "0000|00|00"),
            (-1801, "0099|00|99"),
            (8100, "10000|100|00"),
            (i32::MAX, "2147485547|21474855|47"),
        ] {
            let mut t = tm();
            t[20..24].copy_from_slice(&year.to_le_bytes());
            let mut expected = want.as_bytes().to_vec();
            expected.push(0);
            assert_eq!(check(b"%Y|%C|%y\0", &mut t, 1024).bytes, expected);
        }
    }
    #[test]
    fn iso_week_year_boundaries() {
        for (year, yday, wday, want) in [
            (121, 0, 5, "2020-53-20"),
            (119, 363, 1, "2020-01-20"),
            (116, 0, 5, "2015-53-15"),
            (124, 365, 2, "2025-01-25"),
        ] {
            let mut t = tm();
            for (i, n) in [(5, year), (7, yday), (6, wday)] {
                t[i * 4..i * 4 + 4].copy_from_slice(&i32::to_le_bytes(n));
            }
            let mut expected = want.as_bytes().to_vec();
            expected.push(0);
            assert_eq!(check(b"%G-%V-%g\0", &mut t, 1024).bytes, expected);
        }
    }
    #[test]
    fn invalid_names_question_marks_and_week_numbers() {
        let mut t = tm();
        t[16..20].copy_from_slice(&(-1i32).to_le_bytes());
        t[24..28].copy_from_slice(&7i32.to_le_bytes());
        assert_eq!(check(b"%A|%a|%B|%b\0", &mut t, 1024).bytes, b"?|?|?|?\0");
        assert_eq!(check(b"%U %W\0", &mut tm(), 1024).bytes, b"41 41\0");
    }
    #[test]
    fn timezone_offset_explicit_fallback_and_supplied_zone() {
        let mut t = tm();
        t[40..48].copy_from_slice(&(-19800i64).to_le_bytes());
        assert_eq!(check(b"%z %Z\0", &mut t, 1024).bytes, b"-0530 UTC\0");
        t[32..36].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(check(b"%z%Z\0", &mut t, 1024).bytes, b"\0");
        let mut f = b"%Z\0".to_vec();
        let mut zone = b"CUSTOM\0".to_vec();
        let mut d = [0; 16];
        t[48..56].copy_from_slice(&0x4000u64.to_le_bytes());
        let mut m = Memory::default();
        m.add(F, &mut f, false, false).unwrap();
        m.add(T, &mut t, false, false).unwrap();
        m.add(D, &mut d, true, false).unwrap();
        m.add(0x4000, &mut zone, false, false).unwrap();
        assert_eq!(
            plan(&m, D, 16, F, T, Policy::default()).unwrap().bytes,
            b"CUSTOM\0"
        );
    }
    #[test]
    fn lazy_tm_access_null_format_and_guards() {
        let mut f = b"literal\0".to_vec();
        let mut d = [0xa5; 16];
        let mut m = Memory::default();
        m.add(F, &mut f, false, false).unwrap();
        m.add(D, &mut d, true, false).unwrap();
        let p = plan(&m, D, 16, F, u64::MAX, Policy::default()).unwrap();
        p.commit(&mut m).unwrap();
        assert_eq!(m.read(D, 8).unwrap(), b"literal\0");
        assert!(plan(&m, D, 16, 0, u64::MAX, Policy::default()).is_err());
        assert!(matches!(
            plan(&m, F, 16, F, 0, Policy::default()),
            Err(Error::Alias)
        ));
        let mut t = tm();
        drop(m);
        let mut m = Memory::default();
        m.add(T, &mut t, false, false).unwrap();
        m.add(D, &mut d, true, false).unwrap();
        assert_eq!(
            plan(&m, D, 16, 0, T, Policy::default()).unwrap().completion,
            Completion::Returned(0)
        );
    }
    #[test]
    fn unsupported_conversion_preserves_source_prefix_and_existing_terminator() {
        for (format, expected) in [
            (b"abc%s\0".as_slice(), b"abc".as_slice()),
            (b"%Y%s\0", b"2026\0"),
            (b"%Yx%s\0", b"2026x"),
        ] {
            let p = check(format, &mut tm(), 1024);
            assert_eq!(
                p.completion,
                Completion::Unsupported("strftime %s requires guest mktime")
            );
            assert_eq!(p.bytes, expected);
        }
    }
    #[test]
    fn timezone_policy_is_required_only_when_fallback_is_consulted() {
        let mut f = b"prefix%Z\0".to_vec();
        let mut t = tm();
        let mut out = [0; 32];
        let mut m = Memory::default();
        m.add(F, &mut f, false, false).unwrap();
        m.add(T, &mut t, false, false).unwrap();
        m.add(D, &mut out, true, false).unwrap();
        let p = plan(&m, D, 32, F, T, Policy::default()).unwrap();
        assert_eq!(p.bytes, b"prefix");
        assert!(matches!(p.completion, Completion::Unsupported(_)));
    }
}
