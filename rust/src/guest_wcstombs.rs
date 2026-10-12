//! Guest unsigned16 wchar_t conversion for the runtime's C/POSIX locale.
//!
//! SDK10b539de include/freebsd/x86/_types.h selects unsigned short under __SCE__.
//! FreeBSD releng/11.4 lib/libc/locale/wcstombs.c uses a fresh initial state and
//! none.c:_none_wcsnrtombs converts units0..255 directly, including high bytes.
//! This original Rust implementation does not call host locale APIs or interpret
//! UTF-16 surrogate pairs. Encoding errors preserve the successfully converted
//! prefix; guarded memory/budget faults instead reject the whole plan. Source
//! and destination overlap is outside the restrict-qualified libc contract.
use crate::{guest_memory::Memory, Result};
pub const UNIT_LIMIT: u64 = 65_536;
pub const EILSEQ: u32 = 86;

pub struct Plan {
    pub destination: u64,
    pub bytes: Vec<u8>,
    pub value: u64,
    pub errno: Option<u32>,
}
impl Plan {
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<()> {
        if !self.bytes.is_empty() {
            memory.validate_write(self.destination, self.bytes.len())?;
            memory.write(self.destination, &self.bytes)?;
        }
        Ok(())
    }
}
/// The executor guards the actual prefix and optional errno against its return
/// frame before committing prefix bytes, followed by EILSEQ if present.
pub fn plan(memory: &Memory<'_>, destination: u64, source: u64, capacity: u64) -> Result<Plan> {
    let mut result = Plan {
        destination,
        bytes: Vec::new(),
        value: 0,
        errno: None,
    };
    for index in 0..UNIT_LIMIT {
        if destination != 0 && index == capacity {
            result.value = index;
            return Ok(result);
        }
        let address = index
            .checked_mul(2)
            .and_then(|offset| source.checked_add(offset))
            .ok_or("wcstombs source address overflow")?;
        let unit = memory.load(address, 2)?;
        if unit > 255 {
            result.value = u64::MAX;
            result.errno = Some(EILSEQ);
            return Ok(result);
        }
        if destination != 0 {
            result.bytes.push(unit as u8);
        }
        if unit == 0 {
            result.value = index;
            return Ok(result);
        }
    }
    if destination != 0 && capacity == UNIT_LIMIT {
        result.value = UNIT_LIMIT;
        Ok(result)
    } else {
        Err("wcstombs source scan exceeds bounded unit limit".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn c_bytes_sizing_terminator_and_zero_work() {
        let mut source: Vec<u8> = (1..=255u16).chain([0]).flat_map(u16::to_le_bytes).collect();
        let mut target = vec![0xa5; 257];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut source, false, false).unwrap();
        memory.add(0x2000, &mut target, true, false).unwrap();
        for capacity in [0, 1, 254, 255, 256, u64::MAX] {
            let sizing = plan(&memory, 0, 0x1000, capacity).unwrap();
            assert_eq!(sizing.value, 255);
            assert!(sizing.bytes.is_empty());
            let converted = plan(&memory, 0x2000, 0x1000, capacity).unwrap();
            assert_eq!(converted.value, capacity.min(255));
            assert_eq!(converted.bytes.len() as u64, capacity.min(256));
            assert_eq!(converted.errno, None);
            converted.commit(&mut memory).unwrap();
        }
        assert_eq!(
            memory.read(0x2000, 256).unwrap(),
            (1..=255u8).chain([0]).collect::<Vec<_>>()
        );
        assert_eq!(memory.load(0x2100, 1).unwrap(), 0xa5);
        let zero = plan(&memory, u64::MAX, u64::MAX, 0).unwrap();
        assert_eq!(zero.value, 0);
        assert!(zero.bytes.is_empty());
    }
    #[test]
    fn encoding_error_prefix_and_capacity_order() {
        for bad in [256u16, 0xd800, 0xdfff, 0xffff] {
            let mut source: Vec<u8> = [65u16, 255, bad, 0]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect();
            let mut target = [0xa5; 8];
            let mut memory = Memory::default();
            memory.add(0x1000, &mut source, false, false).unwrap();
            memory.add(0x2000, &mut target, true, false).unwrap();
            let short = plan(&memory, 0x2000, 0x1000, 2).unwrap();
            assert_eq!(short.value, 2);
            assert_eq!(short.errno, None);
            let failed = plan(&memory, 0x2000, 0x1000, 3).unwrap();
            assert_eq!(failed.value, u64::MAX);
            assert_eq!(failed.errno, Some(86));
            failed.commit(&mut memory).unwrap();
            assert_eq!(memory.read(0x2000, 4).unwrap(), [65, 255, 0xa5, 0xa5]);
            let sizing = plan(&memory, 0, 0x1000, 0).unwrap();
            assert_eq!(sizing.errno, Some(86));
            assert!(sizing.bytes.is_empty());
        }
    }
    #[test]
    fn memory_and_work_bounds_do_not_commit_partial_plans() {
        let mut source = vec![0u8; UNIT_LIMIT as usize * 2];
        for unit in source.chunks_exact_mut(2) {
            unit[0] = b'A';
        }
        let mut target = [0xa5; 4];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut source, false, false).unwrap();
        memory.add(0x40000, &mut target, true, false).unwrap();
        assert_eq!(
            plan(&memory, 0x40000, 0x1000, UNIT_LIMIT).unwrap().value,
            UNIT_LIMIT
        );
        assert!(plan(&memory, 0, 0x1000, 0).is_err());
        assert!(plan(&memory, 0x40000, 0x1000, u64::MAX).is_err());
        let p = plan(&memory, 0x40000, 0x1000, 5).unwrap();
        assert!(p.commit(&mut memory).is_err());
        assert_eq!(memory.read(0x40000, 4).unwrap(), [0xa5; 4]);
        assert!(plan(&memory, 0x40000, u64::MAX, 1).is_err());
    }
}
