//! Explicitly configured, owned unbuffered fd1 FILE; not FreeBSD's default
//! lazily allocated/terminal-dependent stdout buffering policy.
//! SDK FILE layout is shared with guest_stdio (312 bytes, _nbuf at119).
//! FreeBSD11.4 libc/stdio/{wbuf,fputc,putc,fflush}.c: __sputc may decrement
//! _w before __swbuf; __swbuf resets it, writes one unsigned byte and flushes.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/wbuf.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/fflush.c
//!
//! Capture/descriptor storage stays outside this model. Reserve the sink and
//! guard metadata/errno destinations before obtaining Commit. After that,
//! commit the reserved sink and metadata adjacently without yielding/failing.
//! No callbacks, buffering changes, wide orientation, stream redirection,
//! closing, general FILE locking or concurrent guest access are supported.
use std::rc::Rc;
pub const FILE_SIZE: usize = crate::guest_stdio::FILE_SIZE;
// Between stderr's pointer cell (0x1200) and the gdtoa pool (0x2000).
pub const FILE_ADDR: u64 = 0x7fff_0000_1400;
pub const STDOUT_PTR_ADDR: u64 = 0x7fff_0000_1600;
const W: usize = 12;
const FLAGS: usize = 16;
const NBUF: usize = 119;
const ORIENTATION: usize = 172;
const SERR: u16 = 0x40;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Fputc,
    Swbuf,
}
/// An actual backend result, never inferred from a host allocation/policy error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Success,
    Failed(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Address,
    UnsupportedState,
    ForeignPlan,
    StalePlan,
    ChangedMemory,
    InvalidErrno,
}
#[derive(Debug)]
pub struct StdoutFile {
    address: u64,
    pointer_address: u64,
    bytes: [u8; FILE_SIZE],
    pointer: [u8; 8],
    owner: Rc<()>,
    revision: u64,
}
#[derive(Debug)]
pub struct Plan {
    owner: Rc<()>,
    revision: u64,
    observed: [u8; FILE_SIZE],
    pointer: [u8; 8],
    next: [u8; FILE_SIZE],
    byte: Option<u8>,
    result: i32,
    errno: Option<u32>,
    address: u64,
}
impl Plan {
    pub fn emitted_byte(&self) -> Option<u8> {
        self.byte
    }
    pub fn result(&self) -> i32 {
        self.result
    }
    pub fn errno(&self) -> Option<u32> {
        self.errno
    }
    /// Exact changed byte runs. Guard every span before preparing a commit;
    /// backend errno (when present) requires its own external destination guard.
    pub fn write_ranges(&self) -> impl Iterator<Item = (u64, usize)> + '_ {
        let mut offset = 0;
        std::iter::from_fn(move || {
            while offset < FILE_SIZE && self.observed[offset] == self.next[offset] {
                offset += 1;
            }
            if offset == FILE_SIZE {
                return None;
            }
            let start = offset;
            while offset < FILE_SIZE && self.observed[offset] != self.next[offset] {
                offset += 1;
            }
            Some((self.address + start as u64, offset - start))
        })
    }
}
pub struct Commit<'a> {
    model: &'a mut StdoutFile,
    observed: &'a mut [u8; FILE_SIZE],
    plan: Plan,
}
impl Commit<'_> {
    /// All validation completed before this permit existed. No allocation,
    /// callbacks, guest-memory reads or recoverable failures occur here.
    pub fn commit(self) {
        *self.observed = self.plan.next;
        self.model.bytes = self.plan.next;
        self.model.revision += 1;
    }
}
impl StdoutFile {
    pub fn new(address: u64, pointer_address: u64) -> Result<Self, Error> {
        let end = address
            .checked_add(FILE_SIZE as u64)
            .ok_or(Error::Address)?;
        let pointer_end = pointer_address.checked_add(8).ok_or(Error::Address)?;
        if address == 0
            || pointer_address == 0
            || address % 8 != 0
            || pointer_address % 8 != 0
            || (address < pointer_end && pointer_address < end)
        {
            return Err(Error::Address);
        }
        let mut bytes = crate::guest_stdio::stderr_file_bytes();
        let buffer = (address + NBUF as u64).to_le_bytes();
        bytes[..8].copy_from_slice(&buffer);
        bytes[24..32].copy_from_slice(&buffer);
        bytes[18..20].copy_from_slice(&1i16.to_le_bytes());
        Ok(Self {
            address,
            pointer_address,
            bytes,
            pointer: address.to_le_bytes(),
            owner: Rc::new(()),
            revision: 0,
        })
    }
    pub fn address(&self) -> u64 {
        self.address
    }
    pub fn pointer_address(&self) -> u64 {
        self.pointer_address
    }
    pub fn file_bytes(&self) -> [u8; FILE_SIZE] {
        self.bytes
    }
    pub fn pointer_bytes(&self) -> [u8; 8] {
        self.pointer
    }
    fn validate(
        &self,
        observed: &[u8; FILE_SIZE],
        pointer: &[u8; 8],
        entry: Entry,
    ) -> Result<(), Error> {
        if pointer != &self.pointer {
            return Err(Error::UnsupportedState);
        }
        let mut normalized = *observed;
        if entry == Entry::Swbuf {
            let w = i32::from_le_bytes(observed[W..W + 4].try_into().unwrap());
            if w != 0 && w != -1 {
                return Err(Error::UnsupportedState);
            }
            normalized[W..W + 4].fill(0);
        }
        if normalized != self.bytes {
            return Err(Error::UnsupportedState);
        }
        Ok(())
    }
    pub fn prepare_byte(
        &self,
        observed: &[u8; FILE_SIZE],
        pointer: &[u8; 8],
        entry: Entry,
        c: i32,
        delivery: Delivery,
    ) -> Result<Plan, Error> {
        self.validate(observed, pointer, entry)?;
        let byte = c as u8;
        let mut next = self.bytes;
        // Full unbuffered flush resets _p to _bf._base and _w to zero even
        // on this one-byte backend failure. _nbuf retains the attempted byte.
        next[NBUF] = byte;
        next[ORIENTATION..ORIENTATION + 4].copy_from_slice(&(-1i32).to_le_bytes());
        let (emitted, result, errno) = match delivery {
            Delivery::Success => (Some(byte), i32::from(byte), None),
            Delivery::Failed(errno) => {
                if errno == 0 || errno > i32::MAX as u32 {
                    return Err(Error::InvalidErrno);
                }
                let flags = u16::from_le_bytes(next[FLAGS..FLAGS + 2].try_into().unwrap()) | SERR;
                next[FLAGS..FLAGS + 2].copy_from_slice(&flags.to_le_bytes());
                (None, -1, Some(errno))
            }
        };
        Ok(Plan {
            owner: self.owner.clone(),
            revision: self.revision,
            observed: *observed,
            pointer: *pointer,
            next,
            byte: emitted,
            result,
            errno,
            address: self.address,
        })
    }
    /// Metadata transition for successful byte-oriented bulk output (e.g. puts).
    /// FreeBSD puts.c orients before __sfvwrite; its unbuffered branch writes
    /// directly to the backend and leaves _nbuf/_p/_w unchanged. Caller owns
    /// source vectors, capture reservation, byte count, and any partial backend
    /// output; this plan neither duplicates bytes nor invents atomic device I/O.
    pub fn prepare_bulk(
        &self,
        observed: &[u8; FILE_SIZE],
        pointer: &[u8; 8],
        success_result: i32,
        delivery: Delivery,
    ) -> Result<Plan, Error> {
        self.validate(observed, pointer, Entry::Fputc)?;
        if success_result < 0 {
            return Err(Error::UnsupportedState);
        }
        let mut next = self.bytes;
        next[ORIENTATION..ORIENTATION + 4].copy_from_slice(&(-1i32).to_le_bytes());
        let (result, errno) = match delivery {
            Delivery::Success => (success_result, None),
            Delivery::Failed(errno) => {
                if errno == 0 || errno > i32::MAX as u32 {
                    return Err(Error::InvalidErrno);
                }
                let flags = u16::from_le_bytes(next[FLAGS..FLAGS + 2].try_into().unwrap()) | SERR;
                next[FLAGS..FLAGS + 2].copy_from_slice(&flags.to_le_bytes());
                (-1, Some(errno))
            }
        };
        Ok(Plan {
            owner: self.owner.clone(),
            revision: self.revision,
            observed: *observed,
            pointer: *pointer,
            next,
            byte: None,
            result,
            errno,
            address: self.address,
        })
    }
    /// No buffered bytes exist in supported quiescent state. Preserve
    /// orientation and sticky error; flushing is not a byte-orientation change.
    pub fn validate_flush(
        &self,
        observed: &[u8; FILE_SIZE],
        pointer: &[u8; 8],
    ) -> Result<(), Error> {
        self.validate(observed, pointer, Entry::Fputc)
    }
    pub fn prepare_commit<'a>(
        &'a mut self,
        observed: &'a mut [u8; FILE_SIZE],
        pointer: &[u8; 8],
        plan: Plan,
    ) -> Result<Commit<'a>, Error> {
        if !Rc::ptr_eq(&self.owner, &plan.owner) {
            return Err(Error::ForeignPlan);
        }
        if self.revision != plan.revision || self.revision == u64::MAX {
            return Err(Error::StalePlan);
        }
        if observed != &plan.observed || pointer != &plan.pointer {
            return Err(Error::ChangedMemory);
        }
        Ok(Commit {
            model: self,
            observed,
            plan,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (StdoutFile, [u8; FILE_SIZE], [u8; 8]) {
        let model = StdoutFile::new(0x1000, 0x1200).unwrap();
        let bytes = model.file_bytes();
        let pointer = model.pointer_bytes();
        (model, bytes, pointer)
    }
    #[test]
    fn configured_addresses_and_bulk_preserve_buffer() {
        let mut m = StdoutFile::new(FILE_ADDR, STDOUT_PTR_ADDR).unwrap();
        let mut b = m.file_bytes();
        let p = m.pointer_bytes();
        let plan = m.prepare_bulk(&b, &p, 10, Delivery::Success).unwrap();
        assert_eq!(plan.result(), 10);
        assert_eq!(plan.emitted_byte(), None);
        assert_eq!(
            plan.write_ranges().collect::<Vec<_>>(),
            vec![(FILE_ADDR + 172, 4)]
        );
        m.prepare_commit(&mut b, &p, plan).unwrap().commit();
        let byte = m
            .prepare_byte(&b, &p, Entry::Fputc, 0xab, Delivery::Success)
            .unwrap();
        m.prepare_commit(&mut b, &p, byte).unwrap().commit();
        let before = b;
        let plan = m.prepare_bulk(&b, &p, 10, Delivery::Success).unwrap();
        assert_eq!(plan.write_ranges().count(), 0);
        m.prepare_commit(&mut b, &p, plan).unwrap().commit();
        assert_eq!(b, before);
        m.validate_flush(&b, &p).unwrap();
    }
    #[test]
    fn owned_layout_and_unoriented_empty_flush() {
        let (m, b, p) = fixture();
        assert_eq!(m.address(), 0x1000);
        assert_eq!(m.pointer_address(), 0x1200);
        assert_eq!(p, 0x1000u64.to_le_bytes());
        assert_eq!(&b[..8], &0x1077u64.to_le_bytes());
        assert_eq!(&b[24..32], &b[..8]);
        assert_eq!(&b[18..20], &1i16.to_le_bytes());
        assert_eq!(&b[32..36], &1i32.to_le_bytes());
        m.validate_flush(&b, &p).unwrap();
        assert_eq!(&b[172..176], &[0; 4]);
    }
    #[test]
    fn inline_decrement_is_legitimate_only_at_swbuf() {
        let (mut m, mut b, p) = fixture();
        b[W..W + 4].copy_from_slice(&(-1i32).to_le_bytes());
        assert_eq!(
            m.prepare_byte(&b, &p, Entry::Fputc, 32, Delivery::Success)
                .unwrap_err(),
            Error::UnsupportedState
        );
        let plan = m
            .prepare_byte(&b, &p, Entry::Swbuf, 32, Delivery::Success)
            .unwrap();
        assert_eq!(plan.emitted_byte(), Some(32));
        assert_eq!(plan.result(), 32);
        assert_eq!(plan.errno(), None);
        m.prepare_commit(&mut b, &p, plan).unwrap().commit();
        assert_eq!(&b[W..W + 4], &[0; 4]);
        assert_eq!(b[NBUF], 32);
        assert_eq!(&b[172..176], &(-1i32).to_le_bytes());
        m.validate_flush(&b, &p).unwrap();
    }
    #[test]
    fn all_unsigned_char_results_and_repeated_flushes() {
        let (mut m, mut b, p) = fixture();
        for c in (-256..512).chain([i32::MIN, i32::MAX]) {
            let plan = m
                .prepare_byte(&b, &p, Entry::Fputc, c, Delivery::Success)
                .unwrap();
            assert_eq!(plan.result(), i32::from(c as u8));
            assert_eq!(plan.emitted_byte(), Some(c as u8));
            m.prepare_commit(&mut b, &p, plan).unwrap().commit();
            assert_eq!(b[NBUF], c as u8);
            assert_eq!(&b[..8], &0x1077u64.to_le_bytes());
            assert_eq!(&b[W..W + 4], &[0; 4]);
        }
    }
    #[test]
    fn abandoned_and_stale_plans_do_not_write() {
        let (mut m, mut b, p) = fixture();
        let before = b;
        let plan = m
            .prepare_byte(&b, &p, Entry::Swbuf, 10, Delivery::Success)
            .unwrap();
        drop(plan);
        assert_eq!(b, before);
        assert_eq!(m.file_bytes(), before);
        let old = m
            .prepare_byte(&b, &p, Entry::Fputc, 65, Delivery::Success)
            .unwrap();
        let now = m
            .prepare_byte(&b, &p, Entry::Fputc, 66, Delivery::Success)
            .unwrap();
        m.prepare_commit(&mut b, &p, now).unwrap().commit();
        let before = b;
        assert!(matches!(
            m.prepare_commit(&mut b, &p, old),
            Err(Error::StalePlan)
        ));
        assert_eq!(b, before);
    }
    #[test]
    fn backend_error_is_sticky_and_retry_keeps_error() {
        let (mut m, mut b, p) = fixture();
        let plan = m
            .prepare_byte(&b, &p, Entry::Fputc, 0x141, Delivery::Failed(5))
            .unwrap();
        assert_eq!(plan.result(), -1);
        assert_eq!(plan.errno(), Some(5));
        assert_eq!(plan.emitted_byte(), None);
        m.prepare_commit(&mut b, &p, plan).unwrap().commit();
        assert_eq!(b[FLAGS] & 0x40, 0x40);
        assert_eq!(b[NBUF], 65);
        let plan = m
            .prepare_byte(&b, &p, Entry::Fputc, 66, Delivery::Success)
            .unwrap();
        m.prepare_commit(&mut b, &p, plan).unwrap().commit();
        assert_eq!(b[FLAGS] & 0x40, 0x40);
        m.validate_flush(&b, &p).unwrap();
    }
    #[test]
    fn changed_and_foreign_plans_fail_without_mutation() {
        let (mut m, mut b, p) = fixture();
        let (other, _, _) = fixture();
        let plan = other
            .prepare_byte(&b, &p, Entry::Fputc, 1, Delivery::Success)
            .unwrap();
        assert!(matches!(
            m.prepare_commit(&mut b, &p, plan),
            Err(Error::ForeignPlan)
        ));
        let plan = m
            .prepare_byte(&b, &p, Entry::Fputc, 1, Delivery::Success)
            .unwrap();
        b[40] = 1;
        let before = b;
        assert!(matches!(
            m.prepare_commit(&mut b, &p, plan),
            Err(Error::ChangedMemory)
        ));
        assert_eq!(b, before);
    }
    #[test]
    fn exact_write_ranges_and_corrupted_metadata() {
        let (m, b, p) = fixture();
        let plan = m
            .prepare_byte(&b, &p, Entry::Fputc, 65, Delivery::Success)
            .unwrap();
        assert_eq!(
            plan.write_ranges().collect::<Vec<_>>(),
            vec![(0x1077, 1), (0x10ac, 4)]
        );
        for offset in [0, 12, 16, 18, 24, 32, 40, 48, 172, 300] {
            let mut bad = b;
            bad[offset] ^= 0x55;
            assert_eq!(
                m.prepare_byte(&bad, &p, Entry::Swbuf, 1, Delivery::Success)
                    .unwrap_err(),
                Error::UnsupportedState
            );
        }
        let mut badp = p;
        badp[0] ^= 1;
        assert!(
            m.prepare_byte(&b, &badp, Entry::Fputc, 1, Delivery::Success)
                .is_err()
        );
    }
    #[test]
    fn invalid_mapping_and_errno_are_not_fabricated_output() {
        for (a, p) in [
            (0, 0x1200),
            (0x1000, 0x1008),
            (0x1001, 0x1200),
            (u64::MAX - 7, 0x1000),
        ] {
            assert!(StdoutFile::new(a, p).is_err());
        }
        let (m, b, p) = fixture();
        assert_eq!(
            m.prepare_byte(&b, &p, Entry::Fputc, 0, Delivery::Failed(0))
                .unwrap_err(),
            Error::InvalidErrno
        );
    }
}
