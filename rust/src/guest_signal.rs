//! Guest signal sets and owned thread masks; no host signals, handlers or delivery.
//! Pinned SDK ABI: four u32 words, signals1..128:
//! https://github.com/ps5-payload-dev/sdk/blob/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_sigset.h
//! FreeBSD11.4 libc behavior (invalid signum checked before dereferencing set):
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/gen/sigsetops.c
use crate::{Result, guest_memory::Memory};
pub const SIZE: usize = 16;
pub const EINVAL: u32 = 22;
/// Pure mask transition; memory is snapshotted separately before this operation.
/// Primary FreeBSD11.4 lib/libthr/thread/thr_sig.c (__thr_sigprocmask),
/// sys/kern/kern_sig.c (kern_sigprocmask), sys/sys/signalvar.h (SIG_CANTMASK).
/// The SDK signal.h defines BLOCK=1, UNBLOCK=2, SETMASK=3, KILL=9, STOP=17, THR=32.
pub fn transition(
    old: [u8; SIZE],
    how: i32,
    input: Option<[u8; SIZE]>,
) -> std::result::Result<[u8; SIZE], u32> {
    let Some(mut input) = input else {
        return Ok(old);
    };
    if !(1..=3).contains(&how) {
        return Err(EINVAL);
    }
    if how != 2 {
        for signal in [9, 17, 32] {
            let bit = signal - 1;
            input[bit / 8] &= !(1 << (bit % 8));
        }
    }
    Ok(std::array::from_fn(|i| match how {
        1 => old[i] | input[i],
        2 => old[i] & !input[i],
        _ => input[i],
    }))
}
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
    fn mask_modes_filtering_and_null_query() {
        let mut expected = [0xff; SIZE];
        expected[1] = 0xfe;
        expected[2] = 0xfe;
        expected[3] = 0x7f;
        assert_eq!(transition([0; SIZE], 3, Some([0xff; SIZE])), Ok(expected));
        assert_eq!(transition(expected, i32::MIN, None), Ok(expected));
        assert_eq!(transition(expected, 0, Some([0; SIZE])), Err(EINVAL));
        for bit in 0..128 {
            let mut one = [0; SIZE];
            one[bit / 8] = 1 << (bit % 8);
            let removed = transition(expected, 2, Some(one)).unwrap();
            assert_eq!(removed[bit / 8] & one[bit / 8], 0);
            assert_eq!(transition(removed, 1, Some(one)), Ok(expected));
        }
    }
    #[test]
    fn inherited_masks_are_owned_and_retired_with_threads() {
        let mut tls = crate::guest_tls::GuestTls::default();
        tls.register_thread(1).unwrap();
        let mut mask = [0; SIZE];
        mask[0] = 1;
        mask[15] = 0x80;
        tls.replace_signal_mask(1, mask).unwrap();
        assert!(tls.register_thread_inheriting(2, 99).is_err());
        assert_eq!(tls.next_thread_id().unwrap(), 2);
        tls.register_thread_inheriting(2, 1).unwrap();
        assert_eq!(tls.signal_mask(2).unwrap(), mask);
        tls.replace_signal_mask(2, [0; SIZE]).unwrap();
        assert_eq!(tls.signal_mask(1).unwrap(), mask);
        tls.retire_thread(2).unwrap();
        assert!(tls.signal_mask(2).is_err());
        tls.register_thread(3).unwrap();
        assert_eq!(tls.signal_mask(3).unwrap(), [0; SIZE]);
    }
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
