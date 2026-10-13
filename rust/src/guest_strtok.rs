//! Original Rust implementation of FreeBSD11.4 strtok_r policy with bounded,
//! guarded guest memory and transactional writes. Source audit:
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strtok.c
//! At end, *last becomes NULL; on a separator it becomes the following address.
//! A fresh nonNULL input never reads old *last. NULL input with NULL *last does
//! not access delimiters or write *last. No errno or guest allocation.
//! Actual read/write restrict overlaps are rejected, not emulated as undefined
//! alias behavior. Guard/budget faults leave all guest bytes unchanged.
use crate::{Result, guest_memory::Memory};
pub const SCAN_LIMIT: usize = 65_536;
#[derive(Debug)]
pub struct Plan {
    pub token: u64,
    pub separator: Option<u64>,
    pub save: Option<(u64, u64)>,
}
impl Plan {
    pub fn write_ranges(&self) -> impl Iterator<Item = (u64, usize)> + '_ {
        self.separator
            .map(|address| (address, 1))
            .into_iter()
            .chain(self.save.map(|(address, _)| (address, 8)))
    }
    pub fn validate(&self, memory: &Memory<'_>) -> Result<()> {
        for (address, length) in self.write_ranges() {
            memory.validate_write(address, length)?;
        }
        Ok(())
    }
    /// Dispatcher must guard all write_ranges against live return frames first.
    /// Both destinations are validated before committing the separator then last.
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<()> {
        self.validate(memory)?;
        if let Some(address) = self.separator {
            memory.store(address, 0, 1)?;
        }
        if let Some((address, next)) = self.save {
            memory.store(address, next, 8)?;
        }
        Ok(())
    }
}
struct Delimiters {
    seen: [bool; 256],
    scanned: usize,
    complete: bool,
}
impl Default for Delimiters {
    fn default() -> Self {
        Self {
            seen: [false; 256],
            scanned: 0,
            complete: false,
        }
    }
}
impl Delimiters {
    // Snapshot only the prefix actually inspected. Once a character is known
    // present, repeated scans need not reread unchanged guest memory. A full
    // delimiter pass then bounds total work to source bytes + delimiter bytes.
    fn matches(
        &mut self,
        memory: &Memory<'_>,
        base: u64,
        byte: u8,
        nul_matches: bool,
    ) -> Result<bool> {
        if byte != 0 && self.seen[byte as usize] {
            return Ok(true);
        }
        while !self.complete {
            if self.scanned == SCAN_LIMIT {
                return Err("strtok_r delimiter scan exceeds bound".into());
            }
            let address = base
                .checked_add(self.scanned as u64)
                .ok_or("strtok_r delimiter address overflow")?;
            let d = memory.load(address, 1)? as u8;
            self.scanned += 1;
            if d == 0 {
                self.complete = true;
            } else {
                self.seen[d as usize] = true;
                if d == byte {
                    return Ok(true);
                }
            }
        }
        Ok(byte == 0 && nul_matches)
    }
}
fn overlap(a: u64, alen: usize, b: u64, blen: usize) -> Result<bool> {
    if alen == 0 || blen == 0 {
        return Ok(false);
    }
    let ae = a
        .checked_add(alen as u64)
        .ok_or("strtok_r range overflow")?;
    let be = b
        .checked_add(blen as u64)
        .ok_or("strtok_r range overflow")?;
    Ok(a < be && b < ae)
}
#[allow(clippy::too_many_arguments)]
fn finish(
    token: u64,
    separator: Option<u64>,
    next: u64,
    last: u64,
    source: u64,
    source_len: usize,
    delim: u64,
    delim_len: usize,
) -> Result<Plan> {
    if overlap(last, 8, source, source_len)?
        || overlap(last, 8, delim, delim_len)?
        || separator.is_some_and(|a| a >= last && a.saturating_sub(last) < 8)
    {
        return Err("strtok_r save pointer overlaps accessed input/delimiter".into());
    }
    if let Some(address) = separator {
        if overlap(address, 1, delim, delim_len)? {
            return Err("strtok_r separator write overlaps accessed delimiter".into());
        }
    }
    Ok(Plan {
        token,
        separator,
        save: Some((last, next)),
    })
}
/// Full64 source/save pointers. `last` is an eight-byte guest pointer object;
/// `delim` is an unsigned-byte set, not a locale or Unicode character sequence.
pub fn plan(memory: &Memory<'_>, input: u64, delim: u64, last: u64) -> Result<Plan> {
    let source = if input == 0 {
        memory.load(last, 8)?
    } else {
        input
    };
    if source == 0 {
        return Ok(Plan {
            token: 0,
            separator: None,
            save: None,
        });
    }
    let mut delimiters = Delimiters::default();
    let mut token = None;
    for index in 0..SCAN_LIMIT {
        let address = source
            .checked_add(index as u64)
            .ok_or("strtok_r source address overflow")?;
        let byte = memory.load(address, 1)? as u8;
        if token.is_none() {
            if delimiters.matches(memory, delim, byte, false)? {
                continue;
            }
            if byte == 0 {
                return finish(
                    0,
                    None,
                    0,
                    last,
                    source,
                    index + 1,
                    delim,
                    delimiters.scanned,
                );
            }
            token = Some(address);
            continue;
        }
        if delimiters.matches(memory, delim, byte, true)? {
            let separator = (byte != 0).then_some(address);
            let next = if byte == 0 {
                0
            } else {
                address
                    .checked_add(1)
                    .ok_or("strtok_r next address overflow")?
            };
            return finish(
                token.unwrap(),
                separator,
                next,
                last,
                source,
                index + 1,
                delim,
                delimiters.scanned,
            );
        }
    }
    Err("strtok_r source scan exceeds bound".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    const S: u64 = 0x1_00000001;
    const D: u64 = 0x2_00000003;
    const P: u64 = 0x3_00000005;
    #[test]
    fn tokens_changed_delimiters_and_null_end_pointer() {
        let mut source = b",,alpha::beta,gamma\0".to_vec();
        let mut delim = b",:\0".to_vec();
        let mut last = [0xa5; 8];
        let mut m = Memory::default();
        m.add(S, &mut source, true, false).unwrap();
        m.add(D, &mut delim, true, false).unwrap();
        m.add(P, &mut last, true, false).unwrap();
        let p = plan(&m, S, D, P).unwrap();
        assert_eq!(p.token, S + 2);
        assert_eq!(p.separator, Some(S + 7));
        p.commit(&mut m).unwrap();
        assert_eq!(m.load(P, 8).unwrap(), S + 8);
        // Delimiter can change between invocations; leading colon is skipped.
        let p = plan(&m, 0, D + 1, P).unwrap();
        assert_eq!(p.token, S + 9);
        assert_eq!(p.separator, None);
        p.commit(&mut m).unwrap();
        assert_eq!(m.load(P, 8).unwrap(), 0);
        assert_eq!(m.read(S + 9, 11).unwrap(), b"beta,gamma\0");
        let p = plan(&m, 0, u64::MAX, P).unwrap();
        assert_eq!(p.token, 0);
        assert_eq!(p.write_ranges().count(), 0);
        p.commit(&mut m).unwrap();
    }
    #[test]
    fn empty_delimiter_all_delimiters_and_high_bytes() {
        for (input, delim, token, separator, next) in [
            (vec![0], vec![0], 0, None, 0),
            (vec![255, 128, 0], vec![0], S, None, 0),
            (vec![255, 255, 0], vec![255, 0], 0, None, 0),
            (vec![128, 255, 7], vec![255, 0], S, Some(S + 1), S + 2),
        ] {
            let mut source = input;
            let mut delimiter = delim;
            let mut last = [0xa5; 8];
            let mut m = Memory::default();
            m.add(S, &mut source, true, false).unwrap();
            m.add(D, &mut delimiter, false, false).unwrap();
            m.add(P, &mut last, true, false).unwrap();
            let p = plan(&m, S, D, P).unwrap();
            assert_eq!((p.token, p.separator), (token, separator));
            p.commit(&mut m).unwrap();
            assert_eq!(m.load(P, 8).unwrap(), next);
        }
    }
    #[test]
    fn no_token_continuation_reads_only_last_and_does_not_require_write() {
        let mut last = [0; 8];
        let mut m = Memory::default();
        m.add(P, &mut last, false, false).unwrap();
        let p = plan(&m, 0, u64::MAX, P).unwrap();
        assert_eq!(p.token, 0);
        p.commit(&mut m).unwrap();
        assert!(plan(&m, 0, D, P + 1).is_err());
    }
    #[test]
    fn validation_is_atomic_for_separator_and_save_pointer() {
        for (source_write, last_len, last_write) in
            [(false, 8, true), (true, 7, true), (true, 8, false)]
        {
            let mut source = b"a,b".to_vec();
            let mut delimiter = b",\0".to_vec();
            let mut last = vec![0xa5; last_len];
            let mut m = Memory::default();
            m.add(S, &mut source, source_write, false).unwrap();
            m.add(D, &mut delimiter, false, false).unwrap();
            m.add(P, &mut last, last_write, false).unwrap();
            let p = plan(&m, S, D, P).unwrap();
            assert!(p.commit(&mut m).is_err());
            assert_eq!(m.read(S, 3).unwrap(), b"a,b");
            assert_eq!(m.read(P, last_len).unwrap(), vec![0xa5; last_len]);
        }
    }
    #[test]
    fn readonly_final_token_and_delimiter_bound() {
        let mut source = b"final\0".to_vec();
        let mut delimiter = vec![b','; SCAN_LIMIT];
        let mut last = [0xa5; 8];
        let mut memory = Memory::default();
        memory.add(S, &mut source, false, false).unwrap();
        memory.add(D, &mut delimiter, true, false).unwrap();
        memory.add(P, &mut last, true, false).unwrap();
        assert!(plan(&memory, S, D, P).is_err());
        assert_eq!(memory.load(P, 8).unwrap(), 0xa5a5a5a5a5a5a5a5);
        memory.store(D + SCAN_LIMIT as u64 - 1, 0, 1).unwrap();
        let p = plan(&memory, S, D, P).unwrap();
        assert_eq!(p.token, S);
        assert_eq!(p.separator, None);
        p.commit(&mut memory).unwrap();
        assert_eq!(memory.load(P, 8).unwrap(), 0);
        assert_eq!(memory.read(S, 6).unwrap(), b"final\0");
    }
    #[test]
    fn read_failures_bounds_and_actual_overlap_reject_without_writes() {
        let mut source = b"a,b\0xxxxxxxx".to_vec();
        let mut delimiter = b",\0".to_vec();
        let mut last = [0xa5; 8];
        let mut m = Memory::default();
        m.add(S, &mut source, true, false).unwrap();
        m.add(D, &mut delimiter, false, false).unwrap();
        m.add(P, &mut last, true, false).unwrap();
        assert!(plan(&m, S, D, S).is_err());
        assert!(plan(&m, S, D, D).is_err());
        assert!(plan(&m, S, u64::MAX, P).is_err());
        assert!(plan(&m, u64::MAX, D, P).is_err());
        assert_eq!(m.read(S, 4).unwrap(), b"a,b\0");
        assert_eq!(m.load(P, 8).unwrap(), 0xa5a5a5a5a5a5a5a5);
        let mut source = vec![b'x'; SCAN_LIMIT];
        let mut delimiter = [0];
        let mut last = [0; 8];
        let mut m = Memory::default();
        m.add(S, &mut source, true, false).unwrap();
        m.add(D, &mut delimiter, false, false).unwrap();
        m.add(P, &mut last, true, false).unwrap();
        assert!(plan(&m, S, D, P).is_err());
        m.store(S + SCAN_LIMIT as u64 - 1, 0, 1).unwrap();
        let p = plan(&m, S, D, P).unwrap();
        assert_eq!(p.token, S);
        p.commit(&mut m).unwrap();
        assert_eq!(m.load(P, 8).unwrap(), 0);
    }
}
