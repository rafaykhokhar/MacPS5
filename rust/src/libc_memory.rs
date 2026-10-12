//! Bounded libc operations over owned guest memory, never host guest-pointers.
use crate::{Result, guest_memory::Memory};
pub const STRING_LIMIT: usize = 65_536;
pub const FILL_LIMIT: u64 = 16 * 1024 * 1024;
/// Bounded temporary-copy semantics support both directions of overlap.
pub fn memmove(memory: &mut Memory<'_>, destination: u64, source: u64, count: u64) -> Result<u64> {
    if count > FILL_LIMIT {
        return Err("Guest memmove exceeds16MiB limit".into());
    }
    if count == 0 {
        return Ok(destination);
    }
    let count = count as usize;
    memory.validate_write(destination, count)?;
    let input = memory.read(source, count)?;
    let mut snapshot = Vec::new();
    snapshot
        .try_reserve_exact(count)
        .map_err(|_| "Cannot allocate bounded memmove snapshot")?;
    snapshot.extend_from_slice(input);
    memory.write(destination, &snapshot)?;
    Ok(destination)
}
/// Compares unsigned bytes, including embedded zero bytes; only the sign is ABI
/// significant. Validate complete ranges before comparing, no host pointers.
pub fn memcmp(memory: &Memory<'_>, left: u64, right: u64, count: u64) -> Result<i32> {
    if count > FILL_LIMIT {
        return Err("Guest memcmp exceeds16MiB limit".into());
    }
    if count == 0 {
        return Ok(0);
    }
    let left = memory.read(left, count as usize)?;
    let right = memory.read(right, count as usize)?;
    Ok(left
        .iter()
        .zip(right)
        .find_map(|(&a, &b)| (a != b).then_some(i32::from(a) - i32::from(b)))
        .unwrap_or(0))
}
/// Outer failures diagnose invalid guest memory or unsupported resource bounds.
/// Inner errno preserves the existing allocator's failure semantics (NULL/errno).
pub fn strdup(memory: &mut Memory<'_>, source: u64) -> Result<std::result::Result<u64, u32>> {
    if source == 0 {
        return Err("Null guest strdup source".into());
    }
    let mut snapshot = Vec::new();
    for offset in 0..STRING_LIMIT {
        let address = source
            .checked_add(offset as u64)
            .ok_or("Guest strdup source overflow")?;
        let byte = memory.load(address, 1)? as u8;
        snapshot
            .try_reserve(1)
            .map_err(|_| "Cannot allocate bounded strdup snapshot")?;
        snapshot.push(byte);
        if byte == 0 {
            let destination = match memory.malloc(snapshot.len() as u64) {
                Ok(address) => address,
                Err(errno) => return Ok(Err(errno)),
            };
            if let Err(error) = memory.write(destination, &snapshot) {
                memory.free(destination).map_err(|rollback| {
                    format!("strdup write failed: {error}; rollback failed: {rollback}")
                })?;
                return Err(error);
            }
            return Ok(Ok(destination));
        }
    }
    Err("Guest strdup exceeds bounded string limit".into())
}
pub fn memset(memory: &mut Memory<'_>, destination: u64, value: u64, count: u64) -> Result<u64> {
    if count > FILL_LIMIT {
        return Err("Guest memset exceeds16MiB limit".into());
    }
    if count == 0 {
        return Ok(destination);
    }
    if destination == 0 {
        return Err("Null guest memset output".into());
    }
    destination
        .checked_add(count)
        .ok_or("Guest memset range overflow")?;
    memory.validate_write(destination, count as usize)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(count as usize)
        .map_err(|_| "Cannot allocate bounded memset fill")?;
    bytes.resize(count as usize, value as u8);
    memory.write(destination, &bytes)?;
    Ok(destination)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    #[test]
    fn duplicate_owns_independent_nul_terminated_heap_and_frees() {
        let mut source = *b"hello\0tail";
        let mut owner = OwnedMappings::default();
        {
            let mut memory = Memory::default();
            memory.add(0x1000, &mut source, true, false).unwrap();
            memory.attach_owned(&mut owner).unwrap();
            let copy = strdup(&mut memory, 0x1000).unwrap().unwrap();
            assert_ne!(copy, 0x1000);
            assert_eq!(memory.read(copy, 6).unwrap(), b"hello\0");
            memory.write(0x1000, b"world").unwrap();
            assert_eq!(memory.read(copy, 6).unwrap(), b"hello\0");
            memory.write(copy, b"other").unwrap();
            assert_eq!(memory.read(0x1000, 6).unwrap(), b"world\0");
            memory.free(copy).unwrap();
            assert!(memory.read(copy, 1).is_err());
            let empty = strdup(&mut memory, 0x1005).unwrap().unwrap();
            assert_eq!(memory.read(empty, 1).unwrap(), [0]);
            memory.free(empty).unwrap();
        }
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn invalid_scan_and_allocator_failure_leave_no_allocation() {
        let mut unterminated = vec![65; STRING_LIMIT];
        let mut owner = OwnedMappings::default();
        {
            let mut memory = Memory::default();
            memory.add(0x1000, &mut unterminated, false, false).unwrap();
            memory.attach_owned(&mut owner).unwrap();
            for source in [0, 1, 0x1000, 0x1000 + STRING_LIMIT as u64 - 1, u64::MAX] {
                assert!(strdup(&mut memory, source).is_err());
            }
        }
        assert_eq!(owner.bytes_allocated(), 0);
        let mut text = *b"a\0";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut text, false, false).unwrap();
        // No attached allocator: existing allocator returns ENOMEM without a block.
        assert_eq!(strdup(&mut memory, 0x1000).unwrap(), Err(12));
        assert_eq!(memory.read(0x1000, 2).unwrap(), b"a\0");
    }
    #[test]
    fn fill_validates_whole_range_and_uses_low_byte() {
        let mut bytes = [0xa5; 32];
        let mut readonly = [0xa5; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        for (address, count) in [
            (0, 1),
            (0x2000, 1),
            (0x101f, 2),
            (u64::MAX - 1, 4),
            (0x1000, FILL_LIMIT + 1),
        ] {
            assert!(memset(&mut memory, address, 0, count).is_err());
            assert_eq!(memory.read(0x1000, 32).unwrap(), [0xa5; 32]);
            assert_eq!(memory.read(0x2000, 32).unwrap(), [0xa5; 32]);
        }
        for address in [0, 1, u64::MAX] {
            assert_eq!(memset(&mut memory, address, 0, 0).unwrap(), address);
        }
        assert_eq!(memset(&mut memory, 0x1001, 0x1234, 30).unwrap(), 0x1001);
        assert_eq!(memory.read(0x1001, 30).unwrap(), [0x34; 30]);
        assert_eq!(memory.load(0x1000, 1).unwrap(), 0xa5);
        assert_eq!(memory.load(0x101f, 1).unwrap(), 0xa5);
    }
}
/// FreeBSD11.4 jemalloc realloc normalizes size zero to one before allocation:
/// https://github.com/freebsd/freebsd-src/blob/releng/11.4/contrib/jemalloc/src/jemalloc.c
/// NULL behaves as malloc. Allocation failure retains the old object unchanged.
/// Copy includes allocator rounding padding when it falls within the new size;
/// no guarantee is made about newly grown bytes. Temporary old+new storage must
/// fit the existing owned-memory budget; in-place resizing is not implemented.
pub fn realloc(
    memory: &mut Memory<'_>,
    source: u64,
    size: u64,
) -> Result<std::result::Result<u64, u32>> {
    if source == 0 {
        return Ok(memory.malloc(size));
    }
    let old_size = memory.heap_allocation_size(source)?;
    let copy_size = old_size.min(usize::try_from(size.max(1)).unwrap_or(usize::MAX));
    let mut snapshot = Vec::new();
    if snapshot.try_reserve_exact(copy_size).is_err() {
        return Ok(Err(12));
    }
    snapshot.extend_from_slice(memory.read(source, copy_size)?);
    let destination = match memory.malloc(size) {
        Ok(address) => address,
        Err(errno) => return Ok(Err(errno)),
    };
    if let Err(error) = memory.write(destination, &snapshot) {
        memory.free(destination).map_err(|rollback| {
            format!("realloc copy failed: {error}; rollback failed: {rollback}")
        })?;
        return Err(error);
    }
    // Exact-base ownership was checked above; neither malloc nor write removes
    // that object. Keep rollback defensive if this invariant ever changes.
    if let Err(error) = memory.free(source) {
        memory.free(destination).map_err(|rollback| {
            format!("realloc release failed: {error}; rollback failed: {rollback}")
        })?;
        return Err(error);
    }
    Ok(Ok(destination))
}
#[cfg(test)]
mod realloc_tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    #[test]
    fn resize_moves_preserves_and_accounts_owned_storage() {
        let mut owner = OwnedMappings::default();
        {
            let mut memory = Memory::default();
            memory.attach_owned(&mut owner).unwrap();
            let old = realloc(&mut memory, 0, 17).unwrap().unwrap();
            memory.write(old, b"abcdefghijklmnopq").unwrap();
            let grown = realloc(&mut memory, old, 65).unwrap().unwrap();
            assert_ne!(grown, old);
            assert!(memory.read(old, 1).is_err());
            assert_eq!(memory.read(grown, 17).unwrap(), b"abcdefghijklmnopq");
            assert_eq!(memory.heap_allocation_size(grown).unwrap(), 80);
            let small = realloc(&mut memory, grown, 3).unwrap().unwrap();
            assert_eq!(memory.read(small, 3).unwrap(), b"abc");
            assert!(memory.read(grown, 1).is_err());
            let zero = realloc(&mut memory, small, 0).unwrap().unwrap();
            assert_ne!(zero, 0);
            assert_eq!(memory.heap_allocation_size(zero).unwrap(), 16);
            assert_eq!(memory.read(zero, 1).unwrap(), b"a");
            assert!(memory.read(small, 1).is_err());
            memory.free(zero).unwrap();
            let nullzero = realloc(&mut memory, 0, 0).unwrap().unwrap();
            assert_ne!(nullzero, 0);
            memory.free(nullzero).unwrap();
        }
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn failures_preserve_old_bytes_and_reject_nonheap_bases() {
        let mut owner = OwnedMappings::default();
        let mut borrowed = [7u8; 16];
        {
            let mut memory = Memory::default();
            memory.attach_owned(&mut owner).unwrap();
            memory.add(0x1000, &mut borrowed, true, false).unwrap();
            let old = memory.malloc(16).unwrap();
            memory.write(old, b"retain unchanged").unwrap();
            for size in [u64::MAX, 16 * 1024 * 1024 + 1] {
                assert_eq!(realloc(&mut memory, old, size).unwrap(), Err(12));
                assert_eq!(memory.read(old, 16).unwrap(), b"retain unchanged");
            }
            for pointer in [old + 1, 0x1000, 0x1001, 99] {
                assert!(realloc(&mut memory, pointer, 8).is_err());
            }
            let anonymous = memory
                .map_anonymous([0, 16384, 3, 0x1002, u64::MAX, 0])
                .unwrap();
            assert!(realloc(&mut memory, anonymous, 8).is_err());
            assert!(memory.heap_allocation_size(anonymous).is_err());
            memory.unmap_anonymous(anonymous, 16384).unwrap();
            assert_eq!(memory.heap_allocation_size(old).unwrap(), 16);
        }
        assert_eq!(owner.bytes_allocated(), 16);
    }
}
#[cfg(test)]
mod move_compare_tests {
    use super::*;
    #[test]
    fn overlap_zero_unsigned_and_atomic_ranges() {
        let mut bytes = *b"0123456789abcdef";
        let mut readonly = [0x80, 0, 0xff, 1];
        let mut m = Memory::default();
        m.add(0x1000, &mut bytes, true, false).unwrap();
        m.add(0x2000, &mut readonly, false, false).unwrap();
        memmove(&mut m, 0x1002, 0x1000, 8).unwrap();
        assert_eq!(m.read(0x1000, 16).unwrap(), b"0101234567abcdef");
        memmove(&mut m, 0x1000, 0x1002, 8).unwrap();
        assert_eq!(m.read(0x1000, 8).unwrap(), b"01234567");
        memmove(&mut m, 0x1000, 0x1000, 8).unwrap();
        let before = m.read(0x1000, 16).unwrap().to_vec();
        for (dst, src, n) in [
            (0x2000, 0x1000, 1),
            (0x1000, 0x2003, 2),
            (0x100f, 0x1000, 2),
            (0x1000, 0x1000, 1u64 << 32),
            (u64::MAX, 0x1000, 1),
        ] {
            assert!(memmove(&mut m, dst, src, n).is_err());
            assert_eq!(m.read(0x1000, 16).unwrap(), before);
        }
        assert_eq!(memmove(&mut m, u64::MAX, 0, 0).unwrap(), u64::MAX);
        assert_eq!(memcmp(&m, 0, u64::MAX, 0).unwrap(), 0);
        assert!(memcmp(&m, 0x2000, 0x1000, 1).unwrap() > 0);
        assert!(memcmp(&m, 0x2001, 0x2002, 2).unwrap() < 0);
        assert_eq!(memcmp(&m, 0x2000, 0x2000, 4).unwrap(), 0);
        for n in [5, 1u64 << 32] {
            assert!(memcmp(&m, 0x2000, 0x2000, n).is_err());
        }
    }
}
