//! Guest signal-set storage only; no host signals, masks, handlers or delivery.
//! Pinned SDK ABI: four u32 words, signals1..128:
//! https://github.com/ps5-payload-dev/sdk/blob/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_sigset.h
//! FreeBSD11.4 libc behavior (invalid signum checked before dereferencing set):
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/gen/sigsetops.c
use crate::{Result, guest_memory::Memory};
pub const SIZE: usize = 16;
pub const EINVAL: u32 = 22;
pub fn valid(signum: i32) -> bool {
    (1..=128).contains(&signum)
}
pub fn empty(memory: &mut Memory<'_>, address: u64) -> Result<()> {
    memory.write(address, &[0; SIZE])
}
/// Valid callers supply the full16byte object. Whole-object validation prevents
/// partial writes on a malformed mapping; other bits are preserved unchanged.
/// Invalid numbers return errno without accessing the set or changing memory.
pub fn add(
    memory: &mut Memory<'_>,
    address: u64,
    signum: i32,
) -> Result<std::result::Result<(), u32>> {
    if !valid(signum) {
        return Ok(Err(EINVAL));
    }
    memory.validate_write(address, SIZE)?;
    let mut bytes = [0; SIZE];
    bytes.copy_from_slice(memory.read(address, SIZE)?);
    let bit = (signum - 1) as usize;
    bytes[bit / 8] |= 1 << (bit % 8);
    memory.write(address, &bytes)?;
    Ok(Ok(()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_signal_bits_and_invalid_ordering() {
        let mut bytes = [0xa5; SIZE];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        empty(&mut memory, 0x1000).unwrap();
        for signal in 1..=128 {
            add(&mut memory, 0x1000, signal).unwrap().unwrap();
            let current = memory.read(0x1000, SIZE).unwrap();
            for bit in 0..128 {
                assert_eq!(
                    (current[bit / 8] >> (bit % 8)) & 1,
                    u8::from(bit < signal as usize)
                );
            }
        }
        for signal in [i32::MIN, -1, 0, 129, i32::MAX] {
            assert_eq!(add(&mut memory, u64::MAX, signal).unwrap(), Err(EINVAL));
        }
        assert_eq!(memory.read(0x1000, SIZE).unwrap(), [0xff; SIZE]);
    }
    #[test]
    fn bounded_writes_are_atomic() {
        for (length, writable) in [(15, true), (16, false)] {
            let mut bytes = vec![0xa5; length];
            let mut memory = Memory::default();
            memory.add(0x1000, &mut bytes, writable, false).unwrap();
            assert!(empty(&mut memory, 0x1000).is_err());
            assert!(add(&mut memory, 0x1000, 1).is_err());
            assert_eq!(memory.read(0x1000, length).unwrap(), vec![0xa5; length]);
        }
    }
}
