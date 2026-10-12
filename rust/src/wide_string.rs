//! Guest SCE wchar_t is unsigned16, unlike host macOS/FreeBSD wchar_t.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/_types.h
//! defines __SCE__ unsigned short, WCHAR_MIN0/MAXffff. Pinned SDL wcslcpy
//! uses two-byte stride. Comparisons operate on code units, not Unicode scalars.
use crate::{Result, guest_memory::Memory};
pub const UNIT_LIMIT: u64 = 65_536;
fn unit(memory: &Memory<'_>, address: u64, index: u64) -> Result<u16> {
    let offset = index
        .checked_mul(2)
        .and_then(|n| address.checked_add(n))
        .ok_or("Wide string address overflow")?;
    let raw = memory.read(offset, 2)?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}
pub fn wcslen(memory: &Memory<'_>, address: u64) -> Result<u64> {
    for index in 0..UNIT_LIMIT {
        if unit(memory, address, index)? == 0 {
            return Ok(index);
        }
    }
    Err("Wide string scan exceeds prototype limit".into())
}
pub fn wcscmp(memory: &Memory<'_>, left: u64, right: u64) -> Result<i32> {
    wcsncmp(memory, left, right, u64::MAX)
}
pub fn wcsncmp(memory: &Memory<'_>, left: u64, right: u64, count: u64) -> Result<i32> {
    for index in 0..count.min(UNIT_LIMIT) {
        let a = unit(memory, left, index)?;
        let b = unit(memory, right, index)?;
        if a != b {
            return Ok(i32::from(a) - i32::from(b));
        }
        if a == 0 {
            return Ok(0);
        }
    }
    if count <= UNIT_LIMIT {
        Ok(0)
    } else {
        Err("Wide comparison exceeds prototype limit".into())
    }
}
/// Linear-time KMP search bounds both input scans and comparison work. Empty
/// needle returns haystack without reading it. Returned pointer is full-width.
pub fn wcsstr(memory: &Memory<'_>, haystack: u64, needle: u64) -> Result<u64> {
    let mut pattern = Vec::new();
    for index in 0..UNIT_LIMIT {
        let value = unit(memory, needle, index)?;
        if value == 0 {
            break;
        }
        pattern
            .try_reserve(1)
            .map_err(|_| "Wide needle allocation failed")?;
        pattern.push(value);
    }
    if pattern.is_empty() {
        return Ok(haystack);
    }
    if pattern.len() as u64 == UNIT_LIMIT {
        return Err("Wide needle exceeds prototype limit".into());
    }
    let mut prefix = Vec::new();
    prefix
        .try_reserve_exact(pattern.len())
        .map_err(|_| "Wide search allocation failed")?;
    prefix.resize(pattern.len(), 0usize);
    let mut matched = 0;
    for index in 1..pattern.len() {
        while matched > 0 && pattern[index] != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix[index] = matched;
    }
    matched = 0;
    for index in 0..UNIT_LIMIT {
        let value = unit(memory, haystack, index)?;
        if value == 0 {
            return Ok(0);
        }
        while matched > 0 && value != pattern[matched] {
            matched = prefix[matched - 1];
        }
        if value == pattern[matched] {
            matched += 1;
        }
        if matched == pattern.len() {
            return haystack
                .checked_add((index + 1 - pattern.len() as u64) * 2)
                .ok_or("Wide result address overflow".into());
        }
    }
    Err("Wide haystack exceeds prototype limit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefix_fallback_first_match_and_absent_needle() {
        let mut hay: Vec<u8> = [65u16, 65, 65, 65, 66, 65, 65, 66, 0]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut needle: Vec<u8> = [65u16, 65, 66, 0]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut absent: Vec<u8> = [65u16, 66, 66, 0]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let mut m = Memory::default();
        m.add(0x1001, &mut hay, false, false).unwrap();
        m.add(0x2001, &mut needle, false, false).unwrap();
        m.add(0x3001, &mut absent, false, false).unwrap();
        assert_eq!(wcsstr(&m, 0x1001, 0x2001).unwrap(), 0x1005);
        assert_eq!(wcsstr(&m, 0x1001, 0x3001).unwrap(), 0);
        assert_eq!(wcsstr(&m, 0x1011, 0x2001).unwrap(), 0);
    }
    #[test]
    fn unsigned16_search_compare_and_guarded_edges() {
        let mut data = vec![0xa5];
        for value in [
            0x41u16, 0xd800, 0xffff, 0x41, 0xd800, 0, 0x41, 0xd800, 0, 0xffff, 0,
        ] {
            data.extend(value.to_le_bytes());
        }
        let before = data.clone();
        let mut m = Memory::default();
        m.add(0x1_0000_0000, &mut data, false, false).unwrap();
        let a = 0x1_0000_0001;
        assert_eq!(wcslen(&m, a).unwrap(), 5);
        assert_eq!(wcsstr(&m, a, a + 12).unwrap(), a);
        assert_eq!(wcsstr(&m, a, a + 18).unwrap(), a + 4);
        assert!(wcscmp(&m, a + 2, a).unwrap() > 0);
        assert_eq!(wcsncmp(&m, a, a + 12, 2).unwrap(), 0);
        assert!(wcsncmp(&m, a, a + 12, 3).unwrap() > 0);
        assert_eq!(wcsncmp(&m, u64::MAX, 0, 0).unwrap(), 0);
        assert_eq!(wcsncmp(&m, a, a, u64::MAX).unwrap(), 0);
        assert_eq!(wcsstr(&m, u64::MAX, a + 10).unwrap(), u64::MAX);
        assert!(wcslen(&m, a + 21).is_err());
        assert!(wcslen(&m, u64::MAX).is_err());
        assert_eq!(m.read(0x1_0000_0000, before.len()).unwrap(), before);
    }
    #[test]
    fn bounded_work_and_first_match_without_trailing_read() {
        let mut nozero = vec![1u8; (UNIT_LIMIT * 2) as usize];
        let mut needle = [1, 1, 0, 0];
        let mut m = Memory::default();
        m.add(0x1000, &mut nozero, false, false).unwrap();
        m.add(0x50000, &mut needle, false, false).unwrap();
        assert!(wcslen(&m, 0x1000).is_err());
        assert!(wcscmp(&m, 0x1000, 0x1000).is_err());
        assert_eq!(wcsncmp(&m, 0x1000, 0x1000, UNIT_LIMIT).unwrap(), 0);
        assert!(wcsncmp(&m, 0x1000, 0x1000, 1u64 << 32).is_err());
        assert_eq!(wcsstr(&m, 0x1000, 0x50000).unwrap(), 0x1000);
    }
}
