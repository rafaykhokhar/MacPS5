//! Guarded byte-string services; FreeBSD11.4 libc/string semantics.
use crate::{Result, guest_memory::Memory};
const LIMIT: usize = 65536;
fn byte(memory: &Memory<'_>, base: u64, index: usize) -> Result<u8> {
    Ok(memory.load(
        base.checked_add(index as u64)
            .ok_or("String address overflow")?,
        1,
    )? as u8)
}
fn snapshot(memory: &Memory<'_>, base: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for i in 0..LIMIT {
        let b = byte(memory, base, i)?;
        if b == 0 {
            return Ok(bytes);
        }
        bytes
            .try_reserve(1)
            .map_err(|_| "Cannot allocate string snapshot")?;
        bytes.push(b);
    }
    Err("Guest string exceeds64KiB scan limit".into())
}
pub struct CopyPlan {
    pub destination: u64,
    pub bytes: Vec<u8>,
    pub result: u64,
}
impl CopyPlan {
    pub fn commit(self, memory: &mut Memory<'_>) -> Result<u64> {
        if !self.bytes.is_empty() {
            memory.write(self.destination, &self.bytes)?;
        }
        Ok(self.result)
    }
}
/// Prepares exactly the bytes which will change, so dispatcher can guard the
/// actual output against its return frame before any write. size_t stays u64.
pub fn copy_plan(
    memory: &Memory<'_>,
    destination: u64,
    source: u64,
    size: u64,
    append: bool,
) -> Result<CopyPlan> {
    let mut used = 0u64;
    if append {
        loop {
            if used == size {
                break;
            }
            if used == LIMIT as u64 {
                return Err("Guest destination exceeds64KiB scan limit".into());
            }
            if byte(memory, destination, used as usize)? == 0 {
                break;
            }
            used += 1;
        }
    }
    let mut source = snapshot(memory, source)?;
    let result = used
        .checked_add(source.len() as u64)
        .ok_or("String length overflow")?;
    if size == used {
        return Ok(CopyPlan {
            destination,
            bytes: Vec::new(),
            result,
        });
    }
    let copy = (size - used - 1).min(source.len() as u64) as usize;
    source.truncate(copy);
    source
        .try_reserve(1)
        .map_err(|_| "Cannot allocate string terminator")?;
    source.push(0);
    let destination = destination
        .checked_add(used)
        .ok_or("String destination overflow")?;
    memory.validate_write(destination, source.len())?;
    Ok(CopyPlan {
        destination,
        bytes: source,
        result,
    })
}
pub fn strchr(memory: &Memory<'_>, source: u64, character: u64, last: bool) -> Result<u64> {
    let mut found = 0;
    for index in 0..LIMIT {
        let value = byte(memory, source, index)?;
        if value == character as u8 {
            found = source
                .checked_add(index as u64)
                .ok_or("String result overflow")?;
            if !last {
                return Ok(found);
            }
        }
        if value == 0 {
            return Ok(found);
        }
    }
    Err("Guest character search exceeds64KiB scan limit".into())
}
/// Streaming KMP bounds comparison work as well as guest reads. Empty needle
/// returns the original pointer without dereferencing the haystack.
pub fn strstr(memory: &Memory<'_>, haystack: u64, needle: u64) -> Result<u64> {
    let needle = snapshot(memory, needle)?;
    if needle.is_empty() {
        return Ok(haystack);
    }
    let mut prefix = Vec::new();
    prefix
        .try_reserve_exact(needle.len())
        .map_err(|_| "Cannot allocate substring search")?;
    prefix.resize(needle.len(), 0usize);
    let mut matched = 0;
    for i in 1..needle.len() {
        while matched > 0 && needle[i] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[i] == needle[matched] {
            matched += 1;
        }
        prefix[i] = matched;
    }
    matched = 0;
    for index in 0..LIMIT {
        let b = byte(memory, haystack, index)?;
        if b == 0 {
            return Ok(0);
        }
        while matched > 0 && b != needle[matched] {
            matched = prefix[matched - 1];
        }
        if b == needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            return haystack
                .checked_add((index + 1 - needle.len()) as u64)
                .ok_or_else(|| "Substring result overflow".into());
        }
    }
    Err("Guest substring search exceeds64KiB scan limit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_prefix_search_and_scan_budget() {
        let mut hay = b"aaaaaaaab\0".to_vec();
        let mut needle = b"aaaab\0".to_vec();
        let mut missing = b"aaaac\0".to_vec();
        let mut unterminated = vec![b'a'; LIMIT];
        let mut memory = Memory::default();
        memory.add(100, &mut hay, false, false).unwrap();
        memory.add(200, &mut needle, false, false).unwrap();
        memory.add(300, &mut missing, false, false).unwrap();
        memory.add(1000, &mut unterminated, false, false).unwrap();
        assert_eq!(strstr(&memory, 100, 200).unwrap(), 104);
        assert_eq!(strstr(&memory, 100, 300).unwrap(), 0);
        assert!(
            strstr(&memory, 1000, 300)
                .unwrap_err()
                .contains("scan limit")
        );
        assert!(
            strchr(&memory, 1000, b'z' as u64, false)
                .unwrap_err()
                .contains("scan limit")
        );
        assert!(
            copy_plan(&memory, u64::MAX, 1000, 0, false)
                .err()
                .unwrap()
                .contains("scan limit")
        );
    }
    #[test]
    fn copy_sizes_truncation_and_atomicity() {
        let mut src = *b"abcdef\0";
        let mut dst = *b"xy\0!!!!!";
        let mut m = Memory::default();
        m.add(100, &mut src, false, false).unwrap();
        m.add(200, &mut dst, true, false).unwrap();
        assert_eq!(
            copy_plan(&m, u64::MAX, 100, 0, false)
                .unwrap()
                .commit(&mut m)
                .unwrap(),
            6
        );
        assert_eq!(
            copy_plan(&m, 200, 100, 4, false)
                .unwrap()
                .commit(&mut m)
                .unwrap(),
            6
        );
        assert_eq!(m.read(200, 8).unwrap(), b"abc\0!!!!");
        assert_eq!(
            copy_plan(&m, 200, 100, 5, true)
                .unwrap()
                .commit(&mut m)
                .unwrap(),
            9
        );
        assert_eq!(m.read(200, 8).unwrap(), b"abca\0!!!");
        let before = m.read(200, 8).unwrap().to_vec();
        assert_eq!(
            copy_plan(&m, 200, 100, 2, true)
                .unwrap()
                .commit(&mut m)
                .unwrap(),
            8
        );
        assert_eq!(m.read(200, 8).unwrap(), before);
        assert!(copy_plan(&m, 205, 100, u64::MAX, false).is_err());
        assert_eq!(m.read(200, 8).unwrap(), before);
        assert_eq!(
            copy_plan(&m, 200, 100, u64::MAX, false)
                .unwrap()
                .commit(&mut m)
                .unwrap(),
            6
        );
    }
    #[test]
    fn searches_bytes_nul_empty_and_earlymatch() {
        let mut bytes = *b"ababa\xff\0aba\0\0";
        let mut m = Memory::default();
        m.add(0x1_0000_0000, &mut bytes, false, false).unwrap();
        let p = 0x1_0000_0000;
        assert_eq!(strchr(&m, p, 0x1ff, false).unwrap(), p + 5);
        assert_eq!(strchr(&m, p, 0, true).unwrap(), p + 6);
        assert_eq!(strchr(&m, p, b'a' as u64, true).unwrap(), p + 4);
        assert_eq!(strstr(&m, p, p + 7).unwrap(), p);
        assert_eq!(strstr(&m, u64::MAX, p + 11).unwrap(), u64::MAX);
        let mut one = *b"Q";
        let mut short = Memory::default();
        short.add(99, &mut one, false, false).unwrap();
        assert_eq!(strchr(&short, 99, b'Q' as u64, false).unwrap(), 99);
        assert!(strchr(&short, 99, b'Q' as u64, true).is_err());
    }
}
