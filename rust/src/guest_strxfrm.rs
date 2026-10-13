//! Bounded C/POSIX strxfrm planning for the owned guest locale.
//!
//! Primary policy: FreeBSD releng/11.4 lib/libc/string/strxfrm.c, lines58–97:
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strxfrm.c
//! The C collation object's __collate_load_error takes the strlcpy fallback:
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strlcpy.c
//! For n>0 this copies at most n−1 bytes and always writes a NUL, even on
//! truncation; return is full source length. No errno or guest allocation.
//! This original Rust implementation implements that policy, not non-C locale
//! weights. Read/write/budget faults reject the plan rather than expose partial
//! host-libc writes. Overlap is outside the restrict contract and is rejected.
use crate::{Result, guest_memory::Memory};
pub const SCAN_LIMIT: usize = 65_536;
#[derive(Debug)]
pub struct Plan {
    pub destination: u64,
    pub bytes: Vec<u8>,
    pub value: u64,
}
impl Plan {
    /// Executor must guard this exact output range against its live frame before
    /// commit. Ordinary memory validation also protects runtime-owned storage.
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<()> {
        if !self.bytes.is_empty() {
            memory.validate_write(self.destination, self.bytes.len())?;
            memory.write(self.destination, &self.bytes)?;
        }
        Ok(())
    }
}
/// Call only under GuestLocale's supported C/POSIX collation policy. Capacity
/// zero is a length query, including when destination is null or unmapped;
/// source must still be readable through its terminator.
pub fn plan(memory: &Memory<'_>, destination: u64, source: u64, capacity: u64) -> Result<Plan> {
    let mut bytes = Vec::new();
    for index in 0..SCAN_LIMIT {
        let address = source
            .checked_add(index as u64)
            .ok_or("strxfrm source address overflow")?;
        let byte = memory.load(address, 1)? as u8;
        if byte != 0 {
            bytes.push(byte);
            continue;
        }
        let value = index as u64;
        if capacity == 0 {
            bytes.clear();
        } else {
            // Min precedes usize conversion: huge guest capacities cannot
            // truncate on the host or allocate a capacity-sized buffer.
            bytes.truncate(value.min(capacity - 1) as usize);
            bytes.push(0);
            let output_end = destination
                .checked_add(bytes.len() as u64)
                .ok_or("strxfrm destination address overflow")?;
            let source_end = address
                .checked_add(1)
                .ok_or("strxfrm source address overflow")?;
            if destination < source_end && source < output_end {
                return Err(
                    "strxfrm source/destination overlap is outside supported restrict contract"
                        .into(),
                );
            }
        }
        return Ok(Plan {
            destination,
            bytes,
            value,
        });
    }
    Err("strxfrm source exceeds bounded scan limit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: u64 = 0x1_00000001;
    const DEST: u64 = 0x2_00000003;
    #[test]
    fn c_byte_order_full_length_and_source_termination_policy() {
        let source: Vec<u8> = (1..=255u8).chain([0]).collect();
        for capacity in [0, 1, 2, 254, 255, 256, 257, u64::MAX] {
            let mut source = source.clone();
            let mut target = vec![0xa5; 257];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            memory.add(DEST, &mut target, true, false).unwrap();
            let got = plan(&memory, DEST, SOURCE, capacity).unwrap();
            assert_eq!(got.value, 255);
            assert_eq!(got.bytes.len() as u64, capacity.min(256));
            if capacity > 0 {
                assert_eq!(got.bytes.last(), Some(&0));
                assert_eq!(
                    &got.bytes[..got.bytes.len() - 1],
                    &(1..=255u8)
                        .take((capacity - 1).min(255) as usize)
                        .collect::<Vec<_>>()
                );
            }
            got.commit(&mut memory).unwrap();
            assert_eq!(memory.load(DEST + got.bytes.len() as u64, 1).unwrap(), 0xa5);
            let query = plan(&memory, u64::MAX, SOURCE, 0).unwrap();
            query.commit(&mut memory).unwrap();
            assert_eq!((query.value, query.bytes.len()), (255, 0));
        }
    }
    #[test]
    fn empty_and_capacity_one_touch_only_nul_not_declared_capacity() {
        for input in [b"\0".as_slice(), b"abcdef\0"] {
            let mut source = input.to_vec();
            let mut target = [0xa5];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            memory.add(DEST, &mut target, true, false).unwrap();
            let p = plan(
                &memory,
                DEST,
                SOURCE,
                if input.len() == 1 { u64::MAX } else { 1 },
            )
            .unwrap();
            assert_eq!(p.bytes, [0]);
            p.commit(&mut memory).unwrap();
            assert_eq!(memory.load(DEST, 1).unwrap(), 0);
            let p = plan(&memory, 0, SOURCE, 1).unwrap();
            assert!(p.commit(&mut memory).is_err());
        }
    }
    #[test]
    fn source_scan_and_write_faults_are_atomic_and_queries_still_read() {
        for capacity in [0, 1, 3, u64::MAX] {
            let mut source = b"abc".to_vec();
            let mut target = [0xa5; 8];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            memory.add(DEST, &mut target, true, false).unwrap();
            assert!(plan(&memory, DEST, SOURCE, capacity).is_err());
            assert_eq!(memory.read(DEST, 8).unwrap(), [0xa5; 8]);
        }
        for (length, writable) in [(0, true), (1, true), (3, true), (4, false)] {
            let mut source = b"abc\0".to_vec();
            let mut target = vec![0xa5; length];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            if length > 0 {
                memory.add(DEST, &mut target, writable, false).unwrap();
            }
            let p = plan(&memory, DEST, SOURCE, 4).unwrap();
            assert!(p.commit(&mut memory).is_err());
            drop(memory);
            assert_eq!(target, vec![0xa5; length]);
        }
        let mut source = b"abc\0".to_vec();
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut source, false, false).unwrap();
        assert!(plan(&memory, u64::MAX - 1, SOURCE, 4).is_err());
    }
    #[test]
    fn restrict_alias_rejection_and_explicit_scan_limit() {
        let mut data = b"abc\0unused".to_vec();
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut data, true, false).unwrap();
        for destination in [SOURCE, SOURCE + 1, SOURCE + 3] {
            assert!(plan(&memory, destination, SOURCE, 2).is_err());
        }
        assert_eq!(plan(&memory, SOURCE, SOURCE, 0).unwrap().value, 3);
        let p = plan(&memory, SOURCE + 4, SOURCE, 2).unwrap();
        p.commit(&mut memory).unwrap();
        assert_eq!(memory.read(SOURCE, 6).unwrap(), b"abc\0a\0");
        let mut source = vec![b'a'; SCAN_LIMIT];
        source[SCAN_LIMIT - 1] = 0;
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut source, true, false).unwrap();
        assert_eq!(
            plan(&memory, 0, SOURCE, 0).unwrap().value,
            SCAN_LIMIT as u64 - 1
        );
        memory.write(SOURCE + SCAN_LIMIT as u64 - 1, b"a").unwrap();
        assert!(plan(&memory, 0, SOURCE, 0).is_err());
    }
}
