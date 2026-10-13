//! Bounded C/POSIX wcsxfrm planning for the owned guest locale.
//!
//! FreeBSD11.4 wcsxfrm.c C fallback first computes wcslen. If length is below
//! capacity it uses wcscpy; otherwise capacity>0 uses wcsncpy(capacity−1) and an
//! explicit final NUL. That truncated branch cannot reach a source terminator,
//! so it never pads. The sufficient branch and empty-source fast path likewise
//! write only through one NUL, not the declared capacity.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/wcsxfrm.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/wcsncpy.c
//! Guest wchar is unsigned16 per pinned SDK10b539de x86/_types.h; capacity and
//! result are counts of those units. No locale conversion, errno or allocation.
//! Original Rust profile implementation: malformed memory/budget faults reject
//! the plan atomically; source/output overlap is outside restrict and rejected.
//! Presence of an import is not evidence this service ran in startup.
use crate::{Result, guest_memory::Memory};
pub const UNIT_LIMIT: usize = 65_536;
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
    for index in 0..UNIT_LIMIT {
        let address = (index as u64)
            .checked_mul(2)
            .and_then(|offset| source.checked_add(offset))
            .ok_or("wcsxfrm source address overflow")?;
        let unit = memory.load(address, 2)? as u16;
        if unit != 0 {
            bytes.extend_from_slice(&unit.to_le_bytes());
            continue;
        }
        let value = index as u64;
        if capacity == 0 {
            bytes.clear();
        } else {
            // Min precedes usize conversion: huge guest capacities cannot
            // truncate on the host or allocate a capacity-sized buffer.
            bytes.truncate(value.min(capacity - 1) as usize * 2);
            bytes.extend_from_slice(&[0, 0]);
            let output_end = destination
                .checked_add(bytes.len() as u64)
                .ok_or("wcsxfrm destination address overflow")?;
            let source_end = address
                .checked_add(2)
                .ok_or("wcsxfrm source address overflow")?;
            if destination < source_end && source < output_end {
                return Err(
                    "wcsxfrm source/destination overlap is outside supported restrict contract"
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
    Err("wcsxfrm source exceeds bounded scan limit".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    const SOURCE: u64 = 0x1_00000001;
    const DEST: u64 = 0x2_00000003;
    fn wide(units: &[u16]) -> Vec<u8> {
        units.iter().flat_map(|n| n.to_le_bytes()).collect()
    }
    #[test]
    fn unsigned_units_capacity_and_no_padding() {
        let units = [0x41, 0x100, 0xd800, 0xdfff, 0xffff, 0];
        for capacity in [0, 1, 2, 4, 5, 6, 7, u64::MAX] {
            let mut source = wide(&units);
            let mut target = [0xa5; 16];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            memory.add(DEST, &mut target, true, false).unwrap();
            let got = plan(&memory, DEST, SOURCE, capacity).unwrap();
            assert_eq!(got.value, 5);
            let expected = if capacity == 0 {
                vec![]
            } else {
                wide(
                    &units[..(capacity - 1).min(5) as usize]
                        .iter()
                        .copied()
                        .chain([0])
                        .collect::<Vec<_>>(),
                )
            };
            assert_eq!(got.bytes, expected);
            got.commit(&mut memory).unwrap();
            if !expected.is_empty() {
                assert_eq!(memory.read(DEST, expected.len()).unwrap(), expected);
            }
            assert_eq!(
                memory
                    .read(DEST + expected.len() as u64, 16 - expected.len())
                    .unwrap(),
                vec![0xa5; 16 - expected.len()]
            );
            let query = plan(&memory, u64::MAX, SOURCE, 0).unwrap();
            query.commit(&mut memory).unwrap();
            assert_eq!((query.value, query.bytes.len()), (5, 0));
        }
    }
    #[test]
    fn empty_and_capacity_one_only_write_one_guest_unit() {
        for units in [&[0u16][..], &[0xffff, 0][..]] {
            let mut source = wide(units);
            let mut target = [0xa5; 2];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            memory.add(DEST, &mut target, true, false).unwrap();
            let got = plan(
                &memory,
                DEST,
                SOURCE,
                if units.len() == 1 { u64::MAX } else { 1 },
            )
            .unwrap();
            assert_eq!(got.bytes, [0, 0]);
            got.commit(&mut memory).unwrap();
            assert_eq!(memory.load(DEST, 2).unwrap(), 0);
            assert!(
                plan(&memory, 0, SOURCE, 1)
                    .unwrap()
                    .commit(&mut memory)
                    .is_err()
            );
        }
    }
    #[test]
    fn source_and_output_guards_are_atomic() {
        for bytes in [vec![], vec![0], wide(&[65]), vec![65, 0, 0]] {
            let mut source = bytes;
            let mut target = [0xa5; 8];
            let mut memory = Memory::default();
            if !source.is_empty() {
                memory.add(SOURCE, &mut source, false, false).unwrap();
            }
            memory.add(DEST, &mut target, true, false).unwrap();
            for capacity in [0, 1, 4, u64::MAX] {
                assert!(plan(&memory, DEST, SOURCE, capacity).is_err());
            }
            assert_eq!(memory.read(DEST, 8).unwrap(), [0xa5; 8]);
        }
        for (length, writable) in [(0, true), (1, true), (3, true), (4, false)] {
            let mut source = wide(&[0xffff, 0]);
            let mut target = vec![0xa5; length];
            let mut memory = Memory::default();
            memory.add(SOURCE, &mut source, false, false).unwrap();
            if length > 0 {
                memory.add(DEST, &mut target, writable, false).unwrap();
            }
            let p = plan(&memory, DEST, SOURCE, 2).unwrap();
            assert!(p.commit(&mut memory).is_err());
            drop(memory);
            assert_eq!(target, vec![0xa5; length]);
        }
        let mut source = wide(&[65, 0]);
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut source, false, false).unwrap();
        assert!(plan(&memory, u64::MAX - 2, SOURCE, 2).is_err());
        assert!(plan(&memory, 0, u64::MAX, 0).is_err());
    }
    #[test]
    fn restrict_byte_overlap_and_unit_limit() {
        let mut source = wide(&[65, 66, 0, 99, 99]);
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut source, true, false).unwrap();
        for destination in [SOURCE, SOURCE + 1, SOURCE + 5] {
            assert!(plan(&memory, destination, SOURCE, 1).is_err());
        }
        assert_eq!(plan(&memory, SOURCE, SOURCE, 0).unwrap().value, 2);
        let p = plan(&memory, SOURCE + 6, SOURCE, 1).unwrap();
        p.commit(&mut memory).unwrap();
        assert_eq!(memory.load(SOURCE + 8, 2).unwrap(), 99);
        let mut source = wide(&vec![65; UNIT_LIMIT]);
        source[UNIT_LIMIT * 2 - 2..].copy_from_slice(&[0, 0]);
        let mut memory = Memory::default();
        memory.add(SOURCE, &mut source, true, false).unwrap();
        assert_eq!(
            plan(&memory, 0, SOURCE, 0).unwrap().value,
            UNIT_LIMIT as u64 - 1
        );
        memory
            .write(SOURCE + (UNIT_LIMIT as u64 - 1) * 2, &[65, 0])
            .unwrap();
        assert!(plan(&memory, 0, SOURCE, 0).is_err());
    }
}
