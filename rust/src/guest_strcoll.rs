//! C/POSIX collation only, matching the runtime's current GuestLocale policy.
//! FreeBSD11.4 strcoll_l uses strcmp when __collate_load_error is set; the
//! C collate object has that field set and both C/POSIX select this object.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strcoll.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/locale/collate.c
//! No host locale lookup, locale-table loading, allocation or errno mutation.
use crate::strings::{self, Fault, Memory};
/// Return the unsigned-byte difference as a low32 C int, using the same bounded
/// reads as strcmp. Call only for the explicitly supported C/POSIX profile.
/// Non-C locale collation must not silently route here if locale support grows.
pub fn compare_c(memory: &mut impl Memory, first: u64, second: u64) -> Result<u64, Fault> {
    strings::call(memory, 2, first, second, 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    struct Bytes {
        first: Vec<u8>,
        second: Vec<u8>,
        reads: RefCell<Vec<u64>>,
    }
    const A: u64 = 0x1_00000001;
    const B: u64 = 0x2_00000003;
    impl Memory for Bytes {
        fn read(&self, address: u64) -> Result<u8, Fault> {
            self.reads.borrow_mut().push(address);
            for (base, bytes) in [(A, &self.first), (B, &self.second)] {
                if let Some(byte) = address
                    .checked_sub(base)
                    .and_then(|at| bytes.get(at as usize))
                {
                    return Ok(*byte);
                }
            }
            Err(Fault::Read)
        }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Fault> {
            panic!("strcoll must not write")
        }
    }
    fn data(first: &[u8], second: &[u8]) -> Bytes {
        Bytes {
            first: first.to_vec(),
            second: second.to_vec(),
            reads: RefCell::new(Vec::new()),
        }
    }
    #[test]
    fn every_unsigned_byte_pair_has_exact_difference() {
        for a in 0u16..=255 {
            for b in 0u16..=255 {
                let mut m = data(&[a as u8, 0], &[b as u8, 0]);
                assert_eq!(
                    compare_c(&mut m, A, B).unwrap(),
                    (i32::from(a) - i32::from(b)) as u32 as u64
                );
            }
        }
    }
    #[test]
    fn early_difference_nul_and_alias_do_not_scan_unneeded_bytes() {
        let mut m = data(&[255], &[1]);
        assert_eq!(compare_c(&mut m, A, B), Ok(254));
        assert_eq!(*m.reads.borrow(), vec![A, B]);
        let mut m = data(&[0], &[0]);
        assert_eq!(compare_c(&mut m, A, B), Ok(0));
        assert_eq!(*m.reads.borrow(), vec![A, B]);
        let mut m = data(b"same\0", &[]);
        assert_eq!(compare_c(&mut m, A, A), Ok(0));
        assert_eq!(m.first, b"same\0");
        let mut m = data(&[1], &[]);
        assert_eq!(compare_c(&mut m, A, A), Err(Fault::Read)); // no address-equality shortcut
    }
    #[test]
    fn guarded_reads_and_limit_remain_explicit() {
        let mut m = data(&[], &[0]);
        assert_eq!(compare_c(&mut m, A, B), Err(Fault::Read));
        assert_eq!(*m.reads.borrow(), vec![A]);
        let mut m = data(&[1], &[1]);
        assert_eq!(compare_c(&mut m, A, B), Err(Fault::Read));
        let bytes = vec![b'x'; strings::LIMIT];
        let mut m = data(&bytes, &bytes);
        assert_eq!(compare_c(&mut m, A, B), Err(Fault::Limit));
        m.first[strings::LIMIT - 1] = 0;
        m.second[strings::LIMIT - 1] = 0;
        assert_eq!(compare_c(&mut m, A, B), Ok(0));
        struct Top;
        impl Memory for Top {
            fn read(&self, a: u64) -> Result<u8, Fault> {
                if a == u64::MAX {
                    Ok(1)
                } else {
                    Err(Fault::Read)
                }
            }
            fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Fault> {
                panic!()
            }
        }
        assert_eq!(compare_c(&mut Top, u64::MAX, u64::MAX), Err(Fault::Read));
    }
}
