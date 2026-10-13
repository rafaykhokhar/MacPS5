//! Sequential SysV AMD64 printf argument fetching, independent of rendering.
//!
//! The descriptor is caller-owned and reloaded per fetch. Each successful fetch
//! commits exactly one cursor field before returning the value. Unlike scanf's
//! snapshot planner, this permits later formatter writes to affect later reads.
//! Descriptor/argument overlap is explicitly unsupported until target alias
//! ordering is verified. Guard failures and invalid guest memory are runtime
//! faults, never libc ENOMEM. No allocator or host floating-point work occurs.
//!
//! ABI evidence: x86_64-unknown-freebsd11.4 Clang va_arg lowering uses GP offsets
//! 0..40 by eight, FP offsets 48..160 by sixteen, and stack slots of eight.
//! x87 extended arguments align the overflow pointer to sixteen, read ten
//! payload bytes and advance sixteen; padding is not read or interpreted.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Includes promoted int, long, pointers and width/precision stars.
    Gp,
    /// Variadic float is promoted to double; return raw binary64 bits.
    Binary64,
    /// Raw x87 payload only. Fetch support does not imply printf %L support.
    Extended80,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    Gp(u64),
    Binary64(u64),
    Extended80([u8; 10]),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    GpOffset,
    FpOffset,
    StackAlignment,
    AddressOverflow,
    VectorCount,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Read(E),
    Write(E),
    Guard(E),
    Malformed(Malformed),
    UnsupportedDescriptorAlias,
    /// AL was zero: the direct-call prologue did not save SIMD registers.
    UnavailableVectorRegisters,
    Budget,
}
/// Adapter reads and writes an entire requested span or fails. A failing write
/// MUST leave bytes unchanged; validate_write must not mutate memory. Calls are
/// synchronous and serialized. The caller's guard additionally protects live
/// return frames and any other runtime-owned destinations.
pub trait Memory {
    type Error;
    fn read(&self, address: u64, out: &mut [u8]) -> Result<(), Self::Error>;
    fn validate_write(&self, address: u64, length: usize) -> Result<(), Self::Error>;
    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Self::Error>;
}
#[derive(Debug)]
pub struct Cursor {
    source: Source,
    remaining: usize,
}
#[derive(Debug)]
enum Source {
    Guest(u64),
    Direct(Direct),
}
#[derive(Debug)]
struct Direct {
    gp: [u64; 5],
    fp: [u64; 8],
    gp_index: usize,
    fp_index: usize,
    vectors_saved: bool,
    overflow: u64,
}
impl Direct {
    fn fetch<M: Memory>(&mut self, memory: &M, class: Class) -> Result<Value, Error<M::Error>> {
        match class {
            Class::Gp if self.gp_index < self.gp.len() => {
                let value = self.gp[self.gp_index];
                self.gp_index += 1;
                return Ok(Value::Gp(value));
            }
            Class::Binary64 if self.fp_index < self.fp.len() => {
                if !self.vectors_saved {
                    return Err(Error::UnavailableVectorRegisters);
                }
                let value = self.fp[self.fp_index];
                self.fp_index += 1;
                return Ok(Value::Binary64(value));
            }
            _ => (),
        }
        if self.overflow & 7 != 0 {
            return Err(Error::Malformed(Malformed::StackAlignment));
        }
        let source = if class == Class::Extended80 {
            add(self.overflow, 15)? & !15
        } else {
            self.overflow
        };
        let next = add(source, if class == Class::Extended80 { 16 } else { 8 })?;
        let value = match class {
            Class::Gp => Value::Gp(u64::from_le_bytes(read::<8, M>(memory, source)?)),
            Class::Binary64 => Value::Binary64(u64::from_le_bytes(read::<8, M>(memory, source)?)),
            Class::Extended80 => Value::Extended80(read::<10, M>(memory, source)?),
        };
        // No host cursor state changes until the complete requested guest value
        // has been read successfully. No guest descriptor exists to write/guard.
        self.overflow = next;
        Ok(value)
    }
}
impl Cursor {
    /// No descriptor access occurs until an argument is actually requested.
    pub fn new(descriptor: u64, argument_budget: usize) -> Self {
        Self {
            source: Source::Guest(descriptor),
            remaining: argument_budget,
        }
    }
    /// Owned argument snapshot for printf(format, ...). GP values are RSI,
    /// RDX, RCX, R8, R9; FP values are the low binary64 bits of XMM0..XMM7.
    /// overflow is the checked service-entry RSP+8, not the pre-call RSP+8.
    /// SysV AL is an upper bound in 0..=8, not an exact saved-register count:
    /// the ordinary prologue saves all eight XMM slots whenever AL is nonzero.
    /// Zero AL leaves FP slots unavailable; do not fabricate deterministic zeros
    /// for an invalid call that subsequently requests a register FP argument.
    /// No guest heap, scratch mapping, descriptor writes, or FP arithmetic.
    pub fn from_call(
        gp: [u64; 5],
        fp: [u64; 8],
        al: u8,
        overflow: u64,
        argument_budget: usize,
    ) -> Result<Self, Malformed> {
        if al > 8 {
            return Err(Malformed::VectorCount);
        }
        Ok(Self {
            source: Source::Direct(Direct {
                gp,
                fp,
                gp_index: 0,
                fp_index: 0,
                vectors_saved: al != 0,
                overflow,
            }),
            remaining: argument_budget,
        })
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    /// Direct-call snapshots have no guest-visible descriptor address.
    pub fn descriptor(&self) -> Option<u64> {
        match self.source {
            Source::Guest(address) => Some(address),
            Source::Direct(_) => None,
        }
    }

    /// Failed preflight/read/write leaves the descriptor and budget unchanged.
    /// Earlier successful fetches are never rolled back by a later failure.
    /// Narrowing/sign-extension belongs to the conversion, not argument fetch.
    pub fn fetch<M: Memory>(
        &mut self,
        memory: &mut M,
        class: Class,
        mut guard: impl FnMut(&M, u64, usize) -> Result<(), M::Error>,
    ) -> Result<Value, Error<M::Error>> {
        if self.remaining == 0 {
            return Err(Error::Budget);
        }
        let descriptor = match &mut self.source {
            Source::Guest(address) => *address,
            Source::Direct(arguments) => {
                let value = arguments.fetch(memory, class)?;
                self.remaining -= 1;
                return Ok(value);
            }
        };
        let descriptor_end = add(descriptor, 24)?;
        let (source, field, next, width) = match class {
            Class::Gp | Class::Binary64 => {
                let (offset_at, limit, step) = if class == Class::Gp {
                    (descriptor, 48u32, 8u32)
                } else {
                    (add(descriptor, 4)?, 176, 16)
                };
                let offset = u32::from_le_bytes(read::<4, M>(memory, offset_at)?);
                if class == Class::Gp && (offset > 48 || offset % 8 != 0) {
                    return Err(Error::Malformed(Malformed::GpOffset));
                }
                if class == Class::Binary64
                    && (!(48..=176).contains(&offset) || (offset - 48) % 16 != 0)
                {
                    return Err(Error::Malformed(Malformed::FpOffset));
                }
                if offset < limit {
                    let save = u64::from_le_bytes(read::<8, M>(memory, add(descriptor, 16)?)?);
                    (
                        add(save, offset.into())?,
                        offset_at,
                        u64::from(offset + step),
                        4,
                    )
                } else {
                    Self::stack(descriptor, memory, false)?
                }
            }
            Class::Extended80 => Self::stack(descriptor, memory, true)?,
        };
        let length = if class == Class::Extended80 { 10 } else { 8 };
        let source_end = add(source, length)?;
        if source < descriptor_end && descriptor < source_end {
            return Err(Error::UnsupportedDescriptorAlias);
        }
        memory.validate_write(field, width).map_err(Error::Write)?;
        guard(memory, field, width).map_err(Error::Guard)?;
        // Read before committing: unsupported overlap is excluded, and a guard
        // fault must not leave a partially advanced guest argument descriptor.
        let value = match class {
            Class::Gp => Value::Gp(u64::from_le_bytes(read::<8, M>(memory, source)?)),
            Class::Binary64 => Value::Binary64(u64::from_le_bytes(read::<8, M>(memory, source)?)),
            Class::Extended80 => Value::Extended80(read::<10, M>(memory, source)?),
        };
        memory
            .write(field, &next.to_le_bytes()[..width])
            .map_err(Error::Write)?;
        self.remaining -= 1;
        Ok(value)
    }
    fn stack<M: Memory>(
        descriptor: u64,
        memory: &M,
        extended: bool,
    ) -> Result<(u64, u64, u64, usize), Error<M::Error>> {
        let field = add(descriptor, 8)?;
        let stack = u64::from_le_bytes(read::<8, M>(memory, field)?);
        if stack % 8 != 0 {
            return Err(Error::Malformed(Malformed::StackAlignment));
        }
        let source = if extended {
            add(stack, 15)? & !15
        } else {
            stack
        };
        Ok((
            source,
            field,
            add(source, if extended { 16 } else { 8 })?,
            8,
        ))
    }
}
fn add<E>(address: u64, offset: u64) -> Result<u64, Error<E>> {
    address
        .checked_add(offset)
        .ok_or(Error::Malformed(Malformed::AddressOverflow))
}
fn read<const N: usize, M: Memory>(memory: &M, address: u64) -> Result<[u8; N], Error<M::Error>> {
    add(address, N as u64)?;
    let mut bytes = [0; N];
    memory.read(address, &mut bytes).map_err(Error::Read)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    const VA: u64 = 32;
    const SAVE: u64 = 128;
    const STACK: u64 = 400;
    #[derive(Clone)]
    struct Mem {
        bytes: Vec<u8>,
        readonly: Option<(u64, usize)>,
        denied_read: Option<(u64, usize)>,
        fail_write: bool,
        trace: RefCell<Vec<(char, u64, usize)>>,
    }
    fn overlap(a: u64, n: usize, b: u64, m: usize) -> bool {
        a < b + m as u64 && b < a + n as u64
    }
    impl Memory for Mem {
        type Error = &'static str;
        fn read(&self, address: u64, out: &mut [u8]) -> Result<(), Self::Error> {
            self.trace.borrow_mut().push(('r', address, out.len()));
            if self
                .denied_read
                .is_some_and(|(a, n)| overlap(a, n, address, out.len()))
            {
                return Err("read");
            }
            let bytes = self
                .bytes
                .get(address as usize..address as usize + out.len())
                .ok_or("read")?;
            out.copy_from_slice(bytes);
            Ok(())
        }
        fn validate_write(&self, address: u64, length: usize) -> Result<(), Self::Error> {
            if self
                .readonly
                .is_some_and(|(a, n)| overlap(a, n, address, length))
            {
                return Err("readonly");
            }
            self.bytes
                .get(address as usize..address as usize + length)
                .ok_or("write")?;
            Ok(())
        }
        fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Self::Error> {
            self.validate_write(address, bytes.len())?;
            if self.fail_write {
                return Err("write");
            }
            self.trace.borrow_mut().push(('w', address, bytes.len()));
            self.bytes[address as usize..address as usize + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
    }
    impl Mem {
        fn put(&mut self, address: u64, bytes: &[u8]) {
            self.bytes[address as usize..address as usize + bytes.len()].copy_from_slice(bytes);
        }
        fn u32(&self, address: u64) -> u32 {
            u32::from_le_bytes(
                self.bytes[address as usize..address as usize + 4]
                    .try_into()
                    .unwrap(),
            )
        }
        fn u64(&self, address: u64) -> u64 {
            u64::from_le_bytes(
                self.bytes[address as usize..address as usize + 8]
                    .try_into()
                    .unwrap(),
            )
        }
    }
    fn memory() -> Mem {
        let mut m = Mem {
            bytes: vec![0; 512],
            readonly: None,
            denied_read: None,
            fail_write: false,
            trace: RefCell::new(Vec::new()),
        };
        m.put(VA, &40u32.to_le_bytes());
        m.put(VA + 4, &160u32.to_le_bytes());
        m.put(VA + 8, &STACK.to_le_bytes());
        m.put(VA + 16, &SAVE.to_le_bytes());
        m.put(SAVE + 40, &0xfeed_dead_beef_cafeu64.to_le_bytes());
        m.put(SAVE + 160, &0x8000_0000_0000_0000u64.to_le_bytes());
        m.put(STACK, &0x7ff0_0000_0000_0001u64.to_le_bytes());
        m.put(STACK + 8, &0xaabb_ccdd_eeff_0011u64.to_le_bytes());
        m
    }
    fn fetch(c: &mut Cursor, m: &mut Mem, class: Class) -> Result<Value, Error<&'static str>> {
        c.fetch(m, class, |_, _, _| Ok(()))
    }
    #[test]
    fn direct_register_classes_share_only_the_overflow_cursor_and_never_write_guest() {
        let mut m = memory();
        m.readonly = Some((0, m.bytes.len()));
        m.fail_write = true;
        let before = m.bytes.clone();
        let gp = [u64::MAX, 0, 0x8000_0000_0000_0000, 17, 99];
        let fp = std::array::from_fn(|i| 0x7ff0_0000_0000_0001 + i as u64);
        let mut c = Cursor::from_call(gp, fp, 1, STACK, 15).unwrap();
        assert_eq!(c.descriptor(), None);
        assert_eq!(Cursor::new(VA, 1).descriptor(), Some(VA));
        for i in 0..8 {
            assert_eq!(
                c.fetch(&mut m, Class::Binary64, |_, _, _| panic!(
                    "no guest descriptor"
                )),
                Ok(Value::Binary64(fp[i]))
            );
            if i < 5 {
                assert_eq!(
                    c.fetch(&mut m, Class::Gp, |_, _, _| panic!("no guest descriptor")),
                    Ok(Value::Gp(gp[i]))
                );
            }
        }
        assert!(m.trace.borrow().is_empty());
        assert_eq!(
            fetch(&mut c, &mut m, Class::Gp),
            Ok(Value::Gp(0x7ff0_0000_0000_0001))
        );
        m.put(STACK + 8, &77u64.to_le_bytes()); // later stack reads stay live
        assert_eq!(
            fetch(&mut c, &mut m, Class::Binary64),
            Ok(Value::Binary64(77))
        );
        assert_eq!(c.remaining(), 0);
        assert_eq!(*m.trace.borrow(), [('r', STACK, 8), ('r', STACK + 8, 8)]);
        m.put(
            STACK + 8,
            &before[(STACK + 8) as usize..(STACK + 16) as usize],
        );
        assert_eq!(m.bytes, before);
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Err(Error::Budget));
    }
    #[test]
    fn direct_al_is_a_save_gate_not_an_exact_register_count() {
        for al in 1..=8 {
            let mut c = Cursor::from_call([0; 5], [u64::MAX; 8], al, u64::MAX, 8).unwrap();
            let mut m = memory();
            for _ in 0..8 {
                assert_eq!(
                    fetch(&mut c, &mut m, Class::Binary64),
                    Ok(Value::Binary64(u64::MAX))
                );
            }
            assert!(m.trace.borrow().is_empty());
        }
        for al in 9..=255 {
            assert!(matches!(
                Cursor::from_call([0; 5], [0; 8], al, 0, 0),
                Err(Malformed::VectorCount)
            ));
        }
        let mut c = Cursor::from_call([11, 22, 33, 44, 55], [123; 8], 0, STACK, 3).unwrap();
        let mut m = memory();
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Ok(Value::Gp(11)));
        assert_eq!(
            fetch(&mut c, &mut m, Class::Binary64),
            Err(Error::UnavailableVectorRegisters)
        );
        assert_eq!(c.remaining(), 2);
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Ok(Value::Gp(22)));
        assert!(m.trace.borrow().is_empty());
    }
    #[test]
    fn direct_stack_read_failure_is_retryable_without_cursor_or_budget_advance() {
        let mut c = Cursor::from_call([0; 5], [0; 8], 0, STACK, 7).unwrap();
        let mut m = memory();
        for _ in 0..5 {
            fetch(&mut c, &mut m, Class::Gp).unwrap();
        }
        m.denied_read = Some((STACK + 7, 1));
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Err(Error::Read("read")));
        assert_eq!(c.remaining(), 2);
        m.denied_read = None;
        assert_eq!(
            fetch(&mut c, &mut m, Class::Gp),
            Ok(Value::Gp(0x7ff0_0000_0000_0001))
        );
        assert_eq!(
            fetch(&mut c, &mut m, Class::Gp),
            Ok(Value::Gp(0xaabb_ccdd_eeff_0011))
        );
        assert_eq!(c.remaining(), 0);
        assert_eq!(
            *m.trace.borrow(),
            [('r', STACK, 8), ('r', STACK, 8), ('r', STACK + 8, 8)]
        );
    }
    #[test]
    fn direct_extended_payload_alignment_ignores_padding_and_preserves_register_slots() {
        let mut m = memory();
        let payload = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        m.put(416, &payload);
        m.put(432, &1234u64.to_le_bytes());
        m.denied_read = Some((426, 6));
        let mut c = Cursor::from_call([7; 5], [8; 8], 1, 408, 15).unwrap();
        assert_eq!(
            fetch(&mut c, &mut m, Class::Extended80),
            Ok(Value::Extended80(payload))
        );
        for _ in 0..5 {
            assert_eq!(fetch(&mut c, &mut m, Class::Gp), Ok(Value::Gp(7)));
        }
        for _ in 0..8 {
            assert_eq!(
                fetch(&mut c, &mut m, Class::Binary64),
                Ok(Value::Binary64(8))
            );
        }
        assert_eq!(
            fetch(&mut c, &mut m, Class::Binary64),
            Ok(Value::Binary64(1234))
        );
        assert_eq!(*m.trace.borrow(), [('r', 416, 10), ('r', 432, 8)]);
    }
    #[test]
    fn direct_stack_alignment_overflow_and_budget_fail_before_guest_access() {
        for (address, class, wanted) in [
            (401, Class::Extended80, Malformed::StackAlignment),
            (u64::MAX - 7, Class::Extended80, Malformed::AddressOverflow),
            (u64::MAX - 15, Class::Extended80, Malformed::AddressOverflow),
        ] {
            let mut m = memory();
            let mut c = Cursor::from_call([0; 5], [0; 8], 0, address, 2).unwrap();
            assert_eq!(fetch(&mut c, &mut m, class), Err(Error::Malformed(wanted)));
            assert_eq!(c.remaining(), 2);
            assert!(m.trace.borrow().is_empty());
        }
        let mut m = memory();
        let mut c = Cursor::from_call([0; 5], [0; 8], 0, u64::MAX, 0).unwrap();
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Err(Error::Budget));
        assert!(m.trace.borrow().is_empty());
    }
    #[test]
    fn mixed_register_classes_and_shared_overflow_are_ordered() {
        let mut m = memory();
        let mut c = Cursor::new(VA, 4);
        assert_eq!(
            fetch(&mut c, &mut m, Class::Gp),
            Ok(Value::Gp(0xfeed_dead_beef_cafe))
        );
        assert_eq!(m.u32(VA), 48);
        assert_eq!(m.u32(VA + 4), 160);
        assert_eq!(
            fetch(&mut c, &mut m, Class::Binary64),
            Ok(Value::Binary64(0x8000_0000_0000_0000))
        );
        assert_eq!(m.u32(VA + 4), 176);
        assert_eq!(
            fetch(&mut c, &mut m, Class::Binary64),
            Ok(Value::Binary64(0x7ff0_0000_0000_0001))
        );
        assert_eq!(
            fetch(&mut c, &mut m, Class::Gp),
            Ok(Value::Gp(0xaabb_ccdd_eeff_0011))
        );
        assert_eq!(m.u64(VA + 8), STACK + 16);
        assert_eq!(c.remaining(), 0);
        let writes: Vec<_> = m
            .trace
            .borrow()
            .iter()
            .filter(|x| x.0 == 'w')
            .copied()
            .collect();
        assert_eq!(
            writes,
            vec![
                ('w', VA, 4),
                ('w', VA + 4, 4),
                ('w', VA + 8, 8),
                ('w', VA + 8, 8)
            ]
        );
    }
    #[test]
    fn all_eight_fp_slots_and_six_gp_slots() {
        let mut m = memory();
        m.put(VA, &0u32.to_le_bytes());
        m.put(VA + 4, &48u32.to_le_bytes());
        for i in 0..6u64 {
            m.put(SAVE + i * 8, &(100 + i).to_le_bytes());
        }
        for i in 0..8u64 {
            m.put(SAVE + 48 + i * 16, &(200 + i).to_le_bytes());
        }
        let mut c = Cursor::new(VA, 14);
        for i in 0..8u64 {
            assert_eq!(
                fetch(&mut c, &mut m, Class::Binary64),
                Ok(Value::Binary64(200 + i))
            );
            if i < 6 {
                assert_eq!(fetch(&mut c, &mut m, Class::Gp), Ok(Value::Gp(100 + i)));
            }
        }
        assert_eq!(m.u64(VA + 8), STACK);
    }
    #[test]
    fn extended_aligns_and_ignores_six_padding_bytes_and_registers() {
        let mut m = memory();
        m.put(VA + 8, &408u64.to_le_bytes());
        m.put(416, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        m.denied_read = Some((426, 6));
        let mut c = Cursor::new(VA, 1);
        assert_eq!(
            fetch(&mut c, &mut m, Class::Extended80),
            Ok(Value::Extended80([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]))
        );
        assert_eq!(m.u64(VA + 8), 432);
        assert_eq!(m.u32(VA), 40);
        assert_eq!(m.u32(VA + 4), 160);
    }
    #[test]
    fn unused_descriptor_fields_are_not_required() {
        let mut m = memory();
        m.denied_read = Some((VA + 4, 12));
        assert!(fetch(&mut Cursor::new(VA, 1), &mut m, Class::Gp).is_ok());
        let mut m = memory();
        m.put(VA, &48u32.to_le_bytes());
        m.denied_read = Some((VA + 16, 8));
        assert!(fetch(&mut Cursor::new(VA, 1), &mut m, Class::Gp).is_ok());
    }
    #[test]
    fn later_fetch_observes_external_cursor_and_save_changes() {
        let mut m = memory();
        let mut c = Cursor::new(VA, 2);
        fetch(&mut c, &mut m, Class::Gp).unwrap();
        m.put(VA, &0u32.to_le_bytes());
        m.put(VA + 16, &480u64.to_le_bytes());
        m.put(480, &99u64.to_le_bytes());
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Ok(Value::Gp(99)));
        assert_eq!(m.u32(VA), 8);
    }
    #[test]
    fn guards_and_memory_failure_preserve_descriptor_and_budget() {
        for failure in 0..4 {
            let mut m = memory();
            let before = m.bytes.clone();
            let mut c = Cursor::new(VA, 2);
            match failure {
                0 => m.readonly = Some((VA, 4)),
                1 => m.denied_read = Some((SAVE + 40, 8)),
                2 => m.fail_write = true,
                _ => {}
            }
            let error = c
                .fetch(&mut m, Class::Gp, |_, _, _| {
                    if failure == 3 {
                        Err("return frame")
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err();
            assert_eq!(
                error,
                match failure {
                    0 => Error::Write("readonly"),
                    1 => Error::Read("read"),
                    2 => Error::Write("write"),
                    _ => Error::Guard("return frame"),
                }
            );
            assert_eq!(m.bytes, before);
            assert_eq!(c.remaining(), 2);
        }
    }
    #[test]
    fn malformed_alias_overflow_and_budget_are_distinct() {
        for (class, field, bits, wanted) in [
            (Class::Gp, VA, 49u32, Malformed::GpOffset),
            (Class::Binary64, VA + 4, 56, Malformed::FpOffset),
        ] {
            let mut m = memory();
            m.put(field, &bits.to_le_bytes());
            let before = m.bytes.clone();
            assert_eq!(
                fetch(&mut Cursor::new(VA, 1), &mut m, class),
                Err(Error::Malformed(wanted))
            );
            assert_eq!(m.bytes, before);
        }
        let mut m = memory();
        m.put(VA, &0u32.to_le_bytes());
        m.put(VA + 16, &VA.to_le_bytes());
        assert_eq!(
            fetch(&mut Cursor::new(VA, 1), &mut m, Class::Gp),
            Err(Error::UnsupportedDescriptorAlias)
        );
        let mut m = memory();
        m.put(VA, &48u32.to_le_bytes());
        m.put(VA + 8, &401u64.to_le_bytes());
        assert_eq!(
            fetch(&mut Cursor::new(VA, 1), &mut m, Class::Gp),
            Err(Error::Malformed(Malformed::StackAlignment))
        );
        m.put(VA + 8, &(u64::MAX - 7).to_le_bytes());
        assert_eq!(
            fetch(&mut Cursor::new(VA, 1), &mut m, Class::Extended80),
            Err(Error::Malformed(Malformed::AddressOverflow))
        );
        let mut m = memory();
        assert_eq!(
            fetch(&mut Cursor::new(u64::MAX, 1), &mut m, Class::Gp),
            Err(Error::Malformed(Malformed::AddressOverflow))
        );
        assert_eq!(
            fetch(&mut Cursor::new(u64::MAX, 0), &mut m, Class::Gp),
            Err(Error::Budget)
        );
        assert!(m.trace.borrow().is_empty());
    }
    #[test]
    fn failed_later_fetch_does_not_rollback_previous_consumption() {
        let mut m = memory();
        let mut c = Cursor::new(VA, 2);
        fetch(&mut c, &mut m, Class::Gp).unwrap();
        m.denied_read = Some((STACK, 8));
        assert_eq!(fetch(&mut c, &mut m, Class::Gp), Err(Error::Read("read")));
        assert_eq!(m.u32(VA), 48);
        assert_eq!(m.u64(VA + 8), STACK);
        assert_eq!(c.remaining(), 1);
    }
}
