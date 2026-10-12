//! Owned pthread attributes, separate from scheduling or host pthread objects.
//! ABI: SDK 10b539de8f2b6773a9a78caa7576a97dbd99b29a pthread.h,
//! x86/_limits.h; semantics: FreeBSD releng/11.4 lib/libthr/thread/
//! thr_attr.c, thr_init.c, thr_private.h. Default stack is the referenced
//! FreeBSD 64-bit policy, not a measured console implementation property.
use crate::{Result, guest_memory::Memory};

pub const ATTRIBUTE_LIMIT: usize = 256;
pub const STACK_MIN: u64 = 2048;
pub const DEFAULT_STACK_SIZE: u64 = 2 * 1024 * 1024;
const EINVAL: i32 = 22;

/// Copy at creation: later attribute mutations cannot affect an existing thread.
/// Allocation limits are enforced by the scheduler, not by this metadata table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadOptions {
    pub detached: bool,
    pub stack_size: u64,
}
impl Default for ThreadOptions {
    fn default() -> Self {
        Self {
            detached: false,
            stack_size: DEFAULT_STACK_SIZE,
        }
    }
}

#[derive(Debug, Default)]
pub struct GuestThreadAttributes {
    entries: Vec<(u64, ThreadOptions)>,
    next: u64,
}
impl GuestThreadAttributes {
    pub fn live_count(&self) -> usize {
        self.entries.len()
    }

    pub fn init(&mut self, memory: &mut Memory<'_>, slot: u64) -> Result<i32> {
        if slot == 0 {
            return Ok(EINVAL);
        }
        memory.validate_write(slot, 8)?;
        if self.entries.len() == ATTRIBUTE_LIMIT {
            return Err("Guest thread attribute budget exhausted".into());
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or("Thread attribute identity exhausted")?;
        // Separate opaque namespace from mutex identities; never a host pointer.
        let identity = next
            .checked_add(0x1_0000_0000)
            .ok_or("Thread attribute identity exhausted")?;
        self.entries
            .try_reserve(1)
            .map_err(|_| "Cannot allocate thread attribute")?;
        memory.write(slot, &identity.to_le_bytes())?;
        self.entries.push((identity, ThreadOptions::default()));
        self.next = next;
        Ok(0)
    }

    fn index(&self, memory: &Memory<'_>, slot: u64) -> Result<Option<usize>> {
        if slot == 0 {
            return Ok(None);
        }
        let identity = memory.load(slot, 8)?;
        Ok(self.entries.iter().position(|(id, _)| *id == identity))
    }

    pub fn destroy(&mut self, memory: &mut Memory<'_>, slot: u64) -> Result<i32> {
        let Some(index) = self.index(memory, slot)? else {
            return Ok(EINVAL);
        };
        memory.validate_write(slot, 8)?;
        memory.write(slot, &0u64.to_le_bytes())?;
        self.entries.remove(index);
        Ok(0)
    }

    pub fn set_detach_state(&mut self, memory: &Memory<'_>, slot: u64, state: i32) -> Result<i32> {
        if !matches!(state, 0 | 1) {
            return Ok(EINVAL);
        }
        let Some(index) = self.index(memory, slot)? else {
            return Ok(EINVAL);
        };
        self.entries[index].1.detached = state == 1;
        Ok(0)
    }

    pub fn set_stack_size(&mut self, memory: &Memory<'_>, slot: u64, size: u64) -> Result<i32> {
        if size < STACK_MIN {
            return Ok(EINVAL);
        }
        let Some(index) = self.index(memory, slot)? else {
            return Ok(EINVAL);
        };
        self.entries[index].1.stack_size = size;
        Ok(0)
    }

    /// NULL attribute pointer selects defaults; invalid non-NULL identity is None.
    /// This does not validate or reserve memory for a future thread's stack.
    pub fn snapshot(&self, memory: &Memory<'_>, slot: u64) -> Result<Option<ThreadOptions>> {
        if slot == 0 {
            return Ok(Some(ThreadOptions::default()));
        }
        Ok(self.index(memory, slot)?.map(|index| self.entries[index].1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_lifecycle_and_invalid_inputs() {
        let mut storage = [0u8; 24];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut storage, true, false).unwrap();
        let mut attrs = GuestThreadAttributes::default();
        assert_eq!(attrs.init(&mut memory, 0).unwrap(), EINVAL);
        assert_eq!(
            attrs.snapshot(&memory, 0).unwrap(),
            Some(ThreadOptions::default())
        );
        assert_eq!(attrs.init(&mut memory, 0x1000).unwrap(), 0);
        let old = memory.load(0x1000, 8).unwrap();
        memory.write(0x1008, &old.to_le_bytes()).unwrap();
        assert_eq!(
            attrs
                .set_stack_size(&memory, 0x1000, STACK_MIN - 1)
                .unwrap(),
            EINVAL
        );
        assert_eq!(attrs.set_detach_state(&memory, 0x1000, 2).unwrap(), EINVAL);
        assert_eq!(
            attrs.snapshot(&memory, 0x1000).unwrap(),
            Some(ThreadOptions::default())
        );
        attrs
            .set_stack_size(&memory, 0x1000, STACK_MIN + 1)
            .unwrap();
        attrs.set_detach_state(&memory, 0x1000, 1).unwrap();
        let captured = attrs.snapshot(&memory, 0x1000).unwrap().unwrap();
        attrs.set_stack_size(&memory, 0x1000, u64::MAX).unwrap();
        attrs.set_detach_state(&memory, 0x1000, 0).unwrap();
        assert_eq!(
            captured,
            ThreadOptions {
                detached: true,
                stack_size: STACK_MIN + 1
            }
        );
        assert_eq!(attrs.destroy(&mut memory, 0x1000).unwrap(), 0);
        assert_eq!(memory.load(0x1000, 8).unwrap(), 0);
        assert_eq!(attrs.snapshot(&memory, 0x1008).unwrap(), None);
        attrs.init(&mut memory, 0x1000).unwrap();
        assert_ne!(memory.load(0x1000, 8).unwrap(), old);
        assert_eq!(attrs.destroy(&mut memory, 0x1008).unwrap(), EINVAL);
        assert_eq!(attrs.live_count(), 1);
    }

    #[test]
    fn output_guards_and_quota_are_atomic() {
        let mut slot = [0u8; 8];
        let mut readonly = [0u8; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut slot, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        let mut attrs = GuestThreadAttributes::default();
        assert!(attrs.init(&mut memory, 0x1001).is_err());
        assert!(attrs.init(&mut memory, 0x2000).is_err());
        assert_eq!(attrs.live_count(), 0);
        for _ in 0..ATTRIBUTE_LIMIT {
            attrs.init(&mut memory, 0x1000).unwrap();
        }
        let identity = memory.load(0x1000, 8).unwrap();
        assert!(attrs.init(&mut memory, 0x1000).is_err());
        assert_eq!(memory.load(0x1000, 8).unwrap(), identity);
        assert_eq!(attrs.live_count(), ATTRIBUTE_LIMIT);
        let mut ro_alias = identity.to_le_bytes();
        let mut view = Memory::default();
        view.add(0x3000, &mut ro_alias, false, false).unwrap();
        assert!(attrs.destroy(&mut view, 0x3000).is_err());
        assert!(attrs.snapshot(&view, 0x3000).unwrap().is_some());
        assert_eq!(attrs.live_count(), ATTRIBUTE_LIMIT);
        attrs.destroy(&mut memory, 0x1000).unwrap();
        attrs.init(&mut memory, 0x1000).unwrap();
        assert_ne!(memory.load(0x1000, 8).unwrap(), identity);
    }
}
