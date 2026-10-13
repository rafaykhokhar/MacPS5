//! Private allocating printf output. Receives vectors in their source flush order;
//! it does not parse formats, batch vectors, or decide when guest spans are read.
//! FreeBSD printfcommon uses eight vectors and sixteen-byte padding chunks. Its
//! caller must preserve those boundaries: combining vectors changes growth.
use crate::{guest_memory::Memory, libc_memory};

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Allocation(u32),
    /// Source-defined libc failure such as EILSEQ; not a runtime limitation.
    Libc(u32),
    /// Actual stream delivery failure. None preserves existing errno when the
    /// backend supplied no error code; capture policy faults use Runtime.
    Stream(Option<u32>),
    Runtime(String),
    OutputLimit,
    PrivateStorageAlias,
    Poisoned,
}
#[derive(Debug, PartialEq, Eq)]
pub struct CleanupFault {
    /// Mapping remains owned by Memory; caller must fault, not publish success.
    pub address: u64,
    pub message: String,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub kind: Kind,
    pub cleanup: Option<CleanupFault>,
    /// A second retained allocation when scratch cleanup and output cleanup
    /// both fail. Neither owned address is discarded from the diagnostic.
    pub output_cleanup: Option<CleanupFault>,
}
impl From<Kind> for Error {
    fn from(kind: Kind) -> Self {
        Self {
            kind,
            cleanup: None,
            output_cleanup: None,
        }
    }
}
impl From<String> for Error {
    fn from(message: String) -> Self {
        Kind::Runtime(message).into()
    }
}
/// A bounded live sink exists only within run's closure. No implicit Drop
/// cleanup hides errors; run performs explicit cleanup on every returned error.
/// A failed append poisons the sink even if the closure swallows its error.
pub struct Buffer<'m, 'a> {
    memory: &'m mut Memory<'a>,
    address: Option<u64>,
    capacity: usize,
    used: usize,
    limit: usize,
    poisoned: bool,
}
impl<'a> Buffer<'_, 'a> {
    /// For cursor reads/writes and ordered %n. The private allocation must not be
    /// freed, resized or mutated through this access. It has not been published.
    pub fn memory(&mut self) -> &mut Memory<'a> {
        self.memory
    }
    pub fn len(&self) -> usize {
        self.used
    }
    pub fn is_empty(&self) -> bool {
        self.used == 0
    }
    pub fn logical_capacity(&self) -> usize {
        self.capacity
    }
    /// Snapshot immediately before borrowing memory for cursor/%n guards. It
    /// includes allocator padding. Do not retain across append/growth; combine
    /// this range exclusion with the executor's existing live-frame guards.
    pub fn private_span(&self) -> Result<(u64, usize), Error> {
        let address = self.live()?;
        Ok((address, self.memory.heap_allocation_size(address)?))
    }
    /// Reject guest-directed writes into all private storage, including padding.
    /// This supplements rather than replaces runtime return-frame validation.
    pub fn validate_external_write(&self, address: u64, length: usize) -> Result<(), Error> {
        let (base, capacity) = self.private_span()?;
        let end = address
            .checked_add(length as u64)
            .ok_or_else(|| Error::from(Kind::Runtime("Printf external write overflow".into())))?;
        if length != 0 && address < base + capacity as u64 && base < end {
            return Err(Kind::PrivateStorageAlias.into());
        }
        self.memory.validate_write(address, length)?;
        Ok(())
    }
    fn live(&self) -> Result<u64, Error> {
        self.address
            .ok_or_else(|| Kind::Runtime("Printf buffer is no longer live".into()).into())
    }
    fn grow(&mut self, count: usize) -> Result<u64, Error> {
        let address = self.live()?;
        let end = self
            .used
            .checked_add(count)
            .filter(|n| *n <= self.limit && *n <= i32::MAX as usize)
            .ok_or(Kind::OutputLimit)?;
        if end >= self.capacity {
            let capacity = end.checked_add(129).ok_or(Kind::OutputLimit)?;
            match libc_memory::realloc(self.memory, address, capacity as u64)? {
                Ok(next) => {
                    self.address = Some(next);
                    self.capacity = capacity;
                }
                Err(errno) => {
                    // realloc preserves old storage; reallocf must release it.
                    let mut error = Error::from(Kind::Allocation(errno));
                    self.cleanup_error(&mut error);
                    return Err(error);
                }
            }
        }
        self.live()?
            .checked_add(self.used as u64)
            .ok_or_else(|| Kind::Runtime("Printf output address overflow".into()).into())
    }
    /// One already ordered source vector; embedded NUL bytes count as output.
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.poisoned {
            return Err(Kind::Poisoned.into());
        }
        let result = self.append_inner(bytes);
        self.poisoned = result.is_err();
        result
    }
    fn append_inner(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.is_empty() {
            return Ok(());
        }
        let destination = self.grow(bytes.len())?;
        self.memory.write(destination, bytes)?;
        self.used += bytes.len();
        Ok(())
    }
    /// Deferred guest source vector. The entire source must be readable and
    /// disjoint from private storage before growth can invalidate its backing.
    pub fn append_guest(&mut self, source: u64, count: usize) -> Result<(), Error> {
        if self.poisoned {
            return Err(Kind::Poisoned.into());
        }
        let result = self.append_guest_inner(source, count);
        self.poisoned = result.is_err();
        result
    }
    fn append_guest_inner(&mut self, source: u64, count: usize) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        let address = self.live()?;
        let capacity = self.memory.heap_allocation_size(address)?;
        let end = source
            .checked_add(count as u64)
            .ok_or_else(|| Error::from(Kind::Runtime("Printf source address overflow".into())))?;
        if source < address + capacity as u64 && address < end {
            return Err(Kind::PrivateStorageAlias.into());
        }
        // Validate without retaining a borrowed slice across realloc.
        self.memory.read(source, count)?;
        let destination = self.grow(count)?;
        libc_memory::memmove(self.memory, destination, source, count as u64)?;
        self.used += count;
        Ok(())
    }
    fn cleanup_with(
        &mut self,
        free: impl FnOnce(&mut Memory<'a>, u64) -> crate::Result<()>,
    ) -> Result<(), CleanupFault> {
        if let Some(address) = self.address {
            free(self.memory, address).map_err(|message| CleanupFault { address, message })?;
            self.address = None;
        }
        Ok(())
    }
    fn cleanup_error(&mut self, error: &mut Error) {
        // An earlier reallocf failure may already record this exact output.
        // A different scratch allocation's failure must not skip output free.
        if self.address.is_some_and(|address| {
            error.cleanup.as_ref().is_some_and(|fault| fault.address == address)
                || error.output_cleanup.as_ref().is_some_and(|fault| fault.address == address)
        }) {
            return;
        }
        if let Err(fault) = self.cleanup_with(|memory, address| memory.free(address)) {
            if error.cleanup.is_none() {
                error.cleanup = Some(fault);
            } else {
                error.output_cleanup = Some(fault);
            }
        }
    }
}
/// Guard strp before allocating; format once; NUL-terminate then publish strp.
/// The closure owns formatter/cursor ordering. Successful publication does not
/// consult errno. On returned error private storage is freed, or Error carries
/// the exact retained allocation on cleanup failure. Guest writes performed by
/// the closure (cursor, %n) are not rolled back.
pub fn run(
    memory: &mut Memory<'_>,
    strp: u64,
    output_limit: usize,
    mut guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
    format: impl FnOnce(&mut Buffer<'_, '_>) -> Result<(), Error>,
) -> Result<i32, Error> {
    memory.validate_write(strp, 8)?;
    guard(memory, strp, 8)?;
    let address = memory.malloc(128).map_err(Kind::Allocation)?;
    let mut buffer = Buffer {
        memory,
        address: Some(address),
        capacity: 128,
        used: 0,
        limit: output_limit,
        poisoned: false,
    };
    let result = (|| {
        format(&mut buffer)?;
        if buffer.poisoned {
            return Err(Kind::Poisoned.into());
        }
        let address = buffer.live()?;
        buffer.memory.validate_write(strp, 8)?;
        guard(buffer.memory, strp, 8)?;
        buffer.memory.store(address + buffer.used as u64, 0, 1)?;
        buffer.memory.store(strp, address, 8)?;
        buffer.address = None; // ownership passes to the guest only after publish
        Ok(buffer.used as i32)
    })();
    match result {
        Ok(length) => Ok(length),
        Err(mut error) => {
            buffer.cleanup_error(&mut error);
            Err(error)
        }
    }
}
/// Call only for an actual libc failure after run has successfully cleaned up.
/// Validate both destinations, then write NULL before errno, including aliases.
/// Unsupported syntax, runtime guard faults and output budgets do not belong here.
pub fn publish_failure(
    memory: &mut Memory<'_>,
    strp: u64,
    errno: u64,
    code: u32,
    mut guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
) -> crate::Result<i32> {
    memory.validate_write(strp, 8)?;
    memory.validate_write(errno, 4)?;
    guard(memory, strp, 8)?;
    guard(memory, errno, 4)?;
    memory.store(strp, 0, 8)?;
    memory.store(errno, code.into(), 4)?;
    Ok(-1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    const SLOT: u64 = 0x1000;
    fn guard(_: &Memory<'_>, _: u64, _: usize) -> crate::Result<()> {
        Ok(())
    }
    #[test]
    fn logical_growth_is_independent_of_rounded_heap_capacity() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 16];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let length = run(&mut m, SLOT, 1000, guard, |b| {
            b.append(&[b'a'; 127])?;
            assert_eq!(b.logical_capacity(), 128);
            b.append(b"b")?;
            assert_eq!(b.logical_capacity(), 257);
            b.append(&[b'c'; 128])?;
            assert_eq!(b.logical_capacity(), 257);
            b.append(b"d")?;
            assert_eq!(b.logical_capacity(), 386);
            Ok(())
        })
        .unwrap();
        assert_eq!(length, 257);
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.heap_allocation_size(p).unwrap(), 400);
        assert_eq!(m.read(p + 126, 4).unwrap(), b"abcc");
        assert_eq!(m.read(p + 256, 2).unwrap(), b"d\0");
        assert_eq!(m.read(SLOT + 8, 8).unwrap(), [0xa5; 8]);
        m.free(p).unwrap();
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn vector_boundaries_change_growth_without_changing_text() {
        for (chunks, expected) in [
            (vec![vec![7; 300]], 432),
            (vec![vec![7; 150], vec![7; 150]], 432),
            (vec![vec![7; 100]; 3], 336),
        ] {
            let mut owner = OwnedMappings::default();
            let mut slot = [0; 8];
            let mut m = Memory::default();
            m.add(SLOT, &mut slot, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            assert_eq!(
                run(&mut m, SLOT, 300, guard, |b| {
                    for chunk in chunks {
                        b.append(&chunk)?;
                    }
                    Ok(())
                })
                .unwrap(),
                300
            );
            let p = m.load(SLOT, 8).unwrap();
            assert_eq!(m.heap_allocation_size(p).unwrap(), expected);
            assert_eq!(m.read(p, 300).unwrap(), [7; 300]);
        }
    }
    #[test]
    fn empty_embedded_nul_and_deferred_guest_reads() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0; 8];
        let mut source = *b"abc";
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.add(0x2000, &mut source, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        assert_eq!(run(&mut m, SLOT, 0, guard, |_| Ok(())).unwrap(), 0);
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.load(p, 1).unwrap(), 0);
        m.free(p).unwrap();
        assert_eq!(
            run(&mut m, SLOT, 6, guard, |b| {
                b.memory().write(0x2000, b"xyz")?;
                b.append_guest(0x2000, 3)?;
                b.append(b"\0!")
            })
            .unwrap(),
            5
        );
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, 6).unwrap(), b"xyz\0!\0");
    }
    #[test]
    fn known_guard_failure_precedes_allocation_and_success_ignores_errno() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 8];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = run(
            &mut m,
            SLOT,
            10,
            |_, _, _| Err("return frame".into()),
            |_| panic!("must not format"),
        )
        .unwrap_err();
        assert_eq!(e.kind, Kind::Runtime("return frame".into()));
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn runtime_failure_cleans_private_storage_but_preserves_ordered_writes() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 16];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = run(&mut m, SLOT, 2, guard, |b| {
            b.memory().store(SLOT + 8, 99, 4)?;
            b.append(b"abc")
        })
        .unwrap_err();
        assert_eq!(e.kind, Kind::OutputLimit);
        assert!(e.cleanup.is_none());
        assert_eq!(m.load(SLOT + 8, 4).unwrap(), 99);
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn initial_and_growth_exhaustion_are_real_allocation_failures() {
        let mut slot = [0xa5; 8];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        assert_eq!(
            run(&mut m, SLOT, 1000, guard, |_| panic!("no allocation"))
                .unwrap_err()
                .kind,
            Kind::Allocation(12)
        );
        drop(m);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        // The logical output is permitted, but growth (+129) exceeds the
        // allocator's real 16 MiB per-object cap. Avoid filling thousands of
        // first-fit heap slots merely to inject an allocation failure.
        let bytes = vec![b'x'; 16 * 1024 * 1024];
        let e = run(&mut m, SLOT, bytes.len(), guard, |b| b.append(&bytes)).unwrap_err();
        assert_eq!(e.kind, Kind::Allocation(12));
        assert!(e.cleanup.is_none());
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn failure_publication_guards_both_outputs_then_writes_errno_last() {
        let mut bytes = [0xa5; 16];
        let mut m = Memory::default();
        m.add(SLOT, &mut bytes, true, false).unwrap();
        assert!(publish_failure(&mut m, SLOT, 0xffff, 12, guard).is_err());
        assert_eq!(m.read(SLOT, 16).unwrap(), [0xa5; 16]);
        assert_eq!(
            publish_failure(&mut m, SLOT, SLOT + 4, 12, guard).unwrap(),
            -1
        );
        assert_eq!(m.load(SLOT, 8).unwrap(), 12u64 << 32);
    }
    #[test]
    fn scratch_cleanup_failure_does_not_skip_output_cleanup() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 16];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let mut output = 0;
        let mut scratch = 0;
        let error = run(&mut m, SLOT, 128, guard, |buffer| {
            output = buffer.private_span()?.0;
            scratch = buffer.memory().malloc(32).unwrap();
            // Model a scratch deallocator failure, retaining its real mapping.
            let mut error = Error::from(Kind::Runtime("scratch conversion fault".into()));
            error.cleanup = Some(CleanupFault { address: scratch, message: "injected scratch cleanup failure".into() });
            Err(error)
        }).unwrap_err();
        assert_eq!(error.cleanup.unwrap().address, scratch);
        assert!(error.output_cleanup.is_none());
        assert!(m.heap_allocation_size(output).is_err());
        assert_eq!(m.heap_allocation_size(scratch).unwrap(), 32);
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        m.free(scratch).unwrap();
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn cleanup_failure_retains_exact_allocation_metadata_for_retry() {
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.attach_owned(&mut owner).unwrap();
        let address = m.malloc(128).unwrap();
        let mut b = Buffer {
            memory: &mut m,
            address: Some(address),
            capacity: 128,
            used: 0,
            limit: 128,
            poisoned: false,
        };
        let e = b
            .cleanup_with(|_, _| Err("injected cleanup fault".into()))
            .unwrap_err();
        assert_eq!(e.address, address);
        assert_eq!(b.address, Some(address));
        assert_eq!(b.memory.heap_allocation_size(address).unwrap(), 128);
        b.cleanup_with(|m, p| m.free(p)).unwrap();
        assert_eq!(b.address, None);
    }
    #[test]
    fn private_source_alias_and_late_publication_guard_failure_cleanup() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 8];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = run(&mut m, SLOT, 128, guard, |b| b.append_guest(b.live()?, 1)).unwrap_err();
        assert_eq!(e.kind, Kind::PrivateStorageAlias);
        let mut calls = 0;
        let e = run(
            &mut m,
            SLOT,
            128,
            |_, _, _| {
                calls += 1;
                if calls == 2 {
                    Err("late guard".into())
                } else {
                    Ok(())
                }
            },
            |b| b.append(b"ok"),
        )
        .unwrap_err();
        assert_eq!(e.kind, Kind::Runtime("late guard".into()));
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn swallowed_append_error_cannot_publish_or_resume_output() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0xa5; 8];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = run(&mut m, SLOT, 1, guard, |b| {
            assert_eq!(b.append(b"bad").unwrap_err().kind, Kind::OutputLimit);
            assert_eq!(b.append(b"").unwrap_err().kind, Kind::Poisoned);
            assert_eq!(b.append_guest(0xffff, 0).unwrap_err().kind, Kind::Poisoned);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(e.kind, Kind::Poisoned);
        assert_eq!(m.read(SLOT, 8).unwrap(), [0xa5; 8]);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn external_write_guard_excludes_rounded_padding_after_growth() {
        let mut owner = OwnedMappings::default();
        let mut slot = [0; 16];
        let mut m = Memory::default();
        m.add(SLOT, &mut slot, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        run(&mut m, SLOT, 200, guard, |b| {
            b.append(&[1; 128])?;
            let (base, capacity) = b.private_span()?;
            assert_eq!(capacity, 272);
            assert_eq!(
                b.validate_external_write(base + 271, 1).unwrap_err().kind,
                Kind::PrivateStorageAlias
            );
            assert_eq!(
                b.validate_external_write(base - 1, 2).unwrap_err().kind,
                Kind::PrivateStorageAlias
            );
            b.validate_external_write(SLOT + 8, 4)?;
            b.memory().store(SLOT + 8, 77, 4)?;
            Ok(())
        })
        .unwrap();
        assert_eq!(m.load(SLOT + 8, 4).unwrap(), 77);
    }
}
