//! Owned guest mutexes using the FreeBSD pointer-object ABI, never host locks.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a include/freebsd/pthread.h;
//! FreeBSD releng/11.4 lib/libthr/thread/{thr_mutex.c,thr_mutexattr.c}.
//! Static initializer lazy allocation, shared/robust/adaptive locks unsupported.
use crate::{Result, guest_memory::Memory};
use std::rc::Rc;
pub const COND_WAITER_LIMIT: usize = 4096;
pub const ATTR_LIMIT: usize = 256;
pub const MUTEX_LIMIT: usize = 256;
pub const ERRORCHECK: i32 = 1;
pub const RECURSIVE: i32 = 2;
pub const NORMAL: i32 = 3;
pub const EINVAL: i32 = 22;
pub const EBUSY: i32 = 16;
pub const EPERM: i32 = 1;
pub const EDEADLK: i32 = 11;
pub const EAGAIN: i32 = 35;
#[derive(Debug)]
struct Attribute {
    identity: u64,
    kind: i32,
}
#[derive(Debug)]
struct Mutex {
    identity: u64,
    kind: i32,
    owner: Option<u64>,
    depth: u32,
}
/// Unforgeable through public fields. Identity is monotonic and never reused.
/// Trusted executor must consume its pending continuation exactly once on
/// acquisition. Copy permits inspection/rechecking while pending, not replay of
/// a completed acquisition; this model does not own a scheduler ticket registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockTicket {
    slot: u64,
    identity: u64,
    thread: u64,
}
impl LockTicket {
    pub fn slot_address(&self) -> u64 {
        self.slot
    }
    pub fn thread_id(&self) -> u64 {
        self.thread
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    Status(i32),
    Pending(LockTicket),
}
/// A retained condition wait cannot be cancelled by dropping this token. The
/// registry keeps the mutex alive until the original owner reacquires it.
#[derive(Debug)]
pub struct CondMutexTicket {
    owner: Rc<()>,
    serial: u64,
    slot: u64,
    identity: u64,
    thread: u64,
}
impl CondMutexTicket {
    pub fn slot_address(&self) -> u64 {
        self.slot
    }
    pub fn thread_id(&self) -> u64 {
        self.thread
    }
}
#[derive(Debug)]
struct CondWaiter {
    serial: u64,
    identity: u64,
    thread: u64,
    depth: u32,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CondReacquire {
    Acquired,
    Pending,
}
/// All validation and capacity reservation precedes this exclusive guard.
/// Dropping it leaves ownership, depth, identities and waiter count unchanged;
/// spare Vec capacity may remain. While held, mutex state cannot change. Caller must preserve the guest
/// mutex slot after preparation (including rejecting overlapping condition slots). Prepare the condition queue and continuation first, then
/// commit both without intervening fallible work. This commit never writes guest
/// memory; condition-slot publication must be validated separately.
pub struct CondDetachPlan<'a> {
    model: &'a mut GuestMutex,
    index: usize,
    ticket: CondMutexTicket,
}
impl CondDetachPlan<'_> {
    pub fn commit(self) -> CondMutexTicket {
        let m = &mut self.model.mutexes[self.index];
        self.model.cond_waiters.push(CondWaiter {
            serial: self.ticket.serial,
            identity: m.identity,
            thread: self.ticket.thread,
            depth: m.depth,
        });
        self.model.cond_next = self.ticket.serial;
        m.owner = None;
        m.depth = 0;
        self.ticket
    }
}
/// A validated reacquisition reserves exclusive model access. Drop preserves the
/// released wait; commit restores ownership and consumes the saved depth once.
/// The caller must preserve the mutex slot while publishing condition completion.
pub struct CondReacquirePlan<'a> {
    model: &'a mut GuestMutex,
    waiter: usize,
    mutex: usize,
}
impl CondReacquirePlan<'_> {
    pub fn commit(self) {
        let w = &self.model.cond_waiters[self.waiter];
        let m = &mut self.model.mutexes[self.mutex];
        m.owner = Some(w.thread);
        m.depth = w.depth;
        self.model.cond_waiters.remove(self.waiter);
    }
}
#[derive(Debug)]
pub struct GuestMutex {
    attributes: Vec<Attribute>,
    mutexes: Vec<Mutex>,
    next: u64,
    cond_owner: Rc<()>,
    cond_next: u64,
    cond_waiters: Vec<CondWaiter>,
}
impl Default for GuestMutex {
    fn default() -> Self {
        Self {
            attributes: Vec::new(),
            mutexes: Vec::new(),
            next: 0x10000,
            cond_owner: Rc::new(()),
            cond_next: 0,
            cond_waiters: Vec::new(),
        }
    }
}
impl GuestMutex {
    fn identity(&self) -> Result<u64> {
        self.next
            .checked_add(1)
            .ok_or_else(|| "Mutex identity exhausted".into())
    }
    pub fn attribute_count(&self) -> usize {
        self.attributes.len()
    }
    pub fn mutex_count(&self) -> usize {
        self.mutexes.len()
    }
    pub fn attr_init(&mut self, memory: &mut Memory<'_>, slot: u64) -> Result<i32> {
        memory.validate_write(slot, 8)?;
        if self.attributes.len() == ATTR_LIMIT {
            return Err("Guest mutex attribute budget exhausted".into());
        }
        let identity = self.identity()?;
        self.attributes
            .try_reserve(1)
            .map_err(|_| "Cannot allocate mutex attribute")?;
        memory.write(slot, &identity.to_le_bytes())?;
        self.attributes.push(Attribute {
            identity,
            kind: ERRORCHECK,
        });
        self.next = identity;
        Ok(0)
    }
    pub fn attr_settype(&mut self, memory: &Memory<'_>, slot: u64, kind: i32) -> Result<i32> {
        if !matches!(kind, ERRORCHECK | RECURSIVE | NORMAL) {
            return Ok(EINVAL);
        }
        let identity = memory.load(slot, 8)?;
        let Some(attr) = self.attributes.iter_mut().find(|a| a.identity == identity) else {
            return Ok(EINVAL);
        };
        attr.kind = kind;
        Ok(0)
    }
    pub fn attr_destroy(&mut self, memory: &mut Memory<'_>, slot: u64) -> Result<i32> {
        let identity = memory.load(slot, 8)?;
        let Some(index) = self.attributes.iter().position(|a| a.identity == identity) else {
            return Ok(EINVAL);
        };
        memory.validate_write(slot, 8)?;
        memory.write(slot, &0u64.to_le_bytes())?;
        self.attributes.remove(index);
        Ok(0)
    }
    pub fn init(&mut self, memory: &mut Memory<'_>, slot: u64, attr_slot: u64) -> Result<i32> {
        memory.validate_write(slot, 8)?;
        let kind = if attr_slot == 0 {
            ERRORCHECK
        } else {
            let identity = memory.load(attr_slot, 8)?;
            let Some(attr) = self.attributes.iter().find(|a| a.identity == identity) else {
                return Ok(EINVAL);
            };
            attr.kind
        };
        if self.mutexes.len() == MUTEX_LIMIT {
            return Err("Guest mutex budget exhausted".into());
        }
        let identity = self.identity()?;
        self.mutexes
            .try_reserve(1)
            .map_err(|_| "Cannot allocate guest mutex")?;
        memory.write(slot, &identity.to_le_bytes())?;
        self.mutexes.push(Mutex {
            identity,
            kind,
            owner: None,
            depth: 0,
        });
        self.next = identity;
        Ok(0)
    }
    pub fn destroy(&mut self, memory: &mut Memory<'_>, slot: u64) -> Result<i32> {
        let identity = memory.load(slot, 8)?;
        let Some(index) = self.mutexes.iter().position(|m| m.identity == identity) else {
            return Ok(EINVAL);
        };
        if self.mutexes[index].owner.is_some()
            || self.cond_waiters.iter().any(|w| w.identity == identity)
        {
            return Ok(EBUSY);
        }
        memory.validate_write(slot, 8)?;
        memory.write(slot, &2u64.to_le_bytes())?;
        self.mutexes.remove(index);
        Ok(0)
    }
    fn acquire(
        &mut self,
        memory: &Memory<'_>,
        slot: u64,
        thread: u64,
        attempt: bool,
    ) -> Result<LockOutcome> {
        if thread == 0 {
            return Err("Invalid guest mutex thread identity".into());
        }
        let identity = memory.load(slot, 8)?;
        if identity == 0 {
            return Err("Static mutex lazy initialization unsupported".into());
        }
        let Some(m) = self.mutexes.iter_mut().find(|m| m.identity == identity) else {
            return Ok(LockOutcome::Status(EINVAL));
        };
        if m.owner.is_none() {
            m.owner = Some(thread);
            m.depth = 1;
            return Ok(LockOutcome::Acquired);
        }
        if m.owner == Some(thread) && m.kind == RECURSIVE {
            if m.depth == i32::MAX as u32 + 1 {
                return Ok(LockOutcome::Status(EAGAIN));
            }
            m.depth += 1;
            return Ok(LockOutcome::Acquired);
        }
        if attempt {
            return Ok(LockOutcome::Status(EBUSY));
        }
        if m.owner == Some(thread) && m.kind == ERRORCHECK {
            return Ok(LockOutcome::Status(EDEADLK));
        }
        Ok(LockOutcome::Pending(LockTicket {
            slot,
            identity,
            thread,
        }))
    }
    pub fn lock(&mut self, memory: &Memory<'_>, slot: u64, thread: u64) -> Result<LockOutcome> {
        self.acquire(memory, slot, thread, false)
    }
    pub fn try_lock(&mut self, memory: &Memory<'_>, slot: u64, thread: u64) -> Result<i32> {
        match self.acquire(memory, slot, thread, true)? {
            LockOutcome::Acquired => Ok(0),
            LockOutcome::Status(v) => Ok(v),
            LockOutcome::Pending(_) => unreachable!(),
        }
    }
    /// Scheduling hint only; the ticket and guest slot are revalidated on resume.
    pub(crate) fn pending_owner(&self, ticket: LockTicket) -> Option<u64> {
        self.mutexes
            .iter()
            .find(|m| m.identity == ticket.identity)
            .and_then(|m| m.owner)
            .filter(|owner| *owner != ticket.thread)
    }
    pub fn resume_lock(&mut self, memory: &Memory<'_>, ticket: LockTicket) -> Result<LockOutcome> {
        if memory.load(ticket.slot, 8)? != ticket.identity {
            return Ok(LockOutcome::Status(EINVAL));
        }
        self.acquire(memory, ticket.slot, ticket.thread, false)
    }
    /// Guest argument errors are returned as pthread statuses; unsupported host
    /// resource limits and invalid guest memory remain explicit diagnostics.
    pub fn prepare_cond_wait<'a>(
        &'a mut self,
        memory: &Memory<'_>,
        slot: u64,
        thread: u64,
    ) -> Result<std::result::Result<CondDetachPlan<'a>, i32>> {
        if thread == 0 {
            return Err("Invalid guest mutex thread identity".into());
        }
        let identity = memory.load(slot, 8)?;
        let Some(index) = self.mutexes.iter().position(|m| m.identity == identity) else {
            return Ok(Err(EINVAL));
        };
        if self.mutexes[index].owner != Some(thread) {
            return Ok(Err(EPERM));
        }
        if self.cond_waiters.len() >= COND_WAITER_LIMIT {
            return Err("Guest condition mutex waiter budget exhausted".into());
        }
        let serial = self
            .cond_next
            .checked_add(1)
            .ok_or("Condition mutex ticket identity exhausted")?;
        self.cond_waiters
            .try_reserve(1)
            .map_err(|_| "Cannot reserve condition mutex waiter")?;
        let ticket = CondMutexTicket {
            owner: Rc::clone(&self.cond_owner),
            serial,
            slot,
            identity,
            thread,
        };
        Ok(Ok(CondDetachPlan {
            model: self,
            index,
            ticket,
        }))
    }
    fn cond_waiter_index(&self, memory: &Memory<'_>, ticket: &CondMutexTicket) -> Result<usize> {
        if !Rc::ptr_eq(&self.cond_owner, &ticket.owner) {
            return Err("Foreign condition mutex ticket".into());
        }
        let index = self
            .cond_waiters
            .iter()
            .position(|w| {
                w.serial == ticket.serial
                    && w.identity == ticket.identity
                    && w.thread == ticket.thread
            })
            .ok_or("Stale or consumed condition mutex ticket")?;
        if memory.load(ticket.slot, 8)? != ticket.identity {
            return Err("Condition mutex slot changed while waiting".into());
        }
        Ok(index)
    }
    /// Scheduler hint without accessing guest memory. Resume must validate the
    /// retained slot before changing ownership or returning to guest code.
    pub fn cond_owner_hint(&self, ticket: &CondMutexTicket) -> Result<Option<u64>> {
        if !Rc::ptr_eq(&self.cond_owner, &ticket.owner) {
            return Err("Foreign condition mutex ticket".into());
        }
        if !self.cond_waiters.iter().any(|w| {
            w.serial == ticket.serial && w.identity == ticket.identity && w.thread == ticket.thread
        }) {
            return Err("Stale or consumed condition mutex ticket".into());
        }
        self.mutexes
            .iter()
            .find(|m| m.identity == ticket.identity)
            .map(|m| m.owner)
            .ok_or_else(|| "Retained condition mutex disappeared".into())
    }
    /// Scheduling hint, validated again at reacquisition. No wait is consumed.
    pub fn cond_pending_owner(
        &self,
        memory: &Memory<'_>,
        ticket: &CondMutexTicket,
    ) -> Result<Option<u64>> {
        self.cond_waiter_index(memory, ticket)?;
        self.mutexes
            .iter()
            .find(|m| m.identity == ticket.identity)
            .map(|m| m.owner)
            .ok_or_else(|| "Retained condition mutex disappeared".into())
    }
    /// Called only after signal/timeout selection. A blocked reacquisition keeps
    /// its saved recursion and destruction guard. All failures precede mutation;
    /// successful acquisition consumes the registered identity exactly once.
    pub fn prepare_cond_reacquire<'a>(
        &'a mut self,
        memory: &Memory<'_>,
        ticket: &CondMutexTicket,
    ) -> Result<Option<CondReacquirePlan<'a>>> {
        let waiter = self.cond_waiter_index(memory, ticket)?;
        let mutex = self
            .mutexes
            .iter()
            .position(|m| m.identity == ticket.identity)
            .ok_or("Retained condition mutex disappeared")?;
        if self.mutexes[mutex].owner.is_some() {
            return Ok(None);
        }
        Ok(Some(CondReacquirePlan {
            model: self,
            waiter,
            mutex,
        }))
    }
    pub fn reacquire_cond_wait(
        &mut self,
        memory: &Memory<'_>,
        ticket: &CondMutexTicket,
    ) -> Result<CondReacquire> {
        match self.prepare_cond_reacquire(memory, ticket)? {
            Some(plan) => {
                plan.commit();
                Ok(CondReacquire::Acquired)
            }
            None => Ok(CondReacquire::Pending),
        }
    }
    pub fn unlock(&mut self, memory: &Memory<'_>, slot: u64, thread: u64) -> Result<i32> {
        if thread == 0 {
            return Err("Invalid guest mutex thread identity".into());
        }
        let identity = memory.load(slot, 8)?;
        let Some(m) = self.mutexes.iter_mut().find(|m| m.identity == identity) else {
            return Ok(EINVAL);
        };
        if m.owner != Some(thread) {
            return Ok(EPERM);
        }
        m.depth -= 1;
        if m.depth == 0 {
            m.owner = None;
        }
        Ok(0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recursive_ownership_contention_and_stale_tickets() {
        let mut bytes = [0u8; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut m = GuestMutex::default();
        m.attr_init(&mut memory, 0x1000).unwrap();
        m.attr_settype(&memory, 0x1000, RECURSIVE).unwrap();
        m.init(&mut memory, 0x1008, 0x1000).unwrap();
        m.attr_settype(&memory, 0x1000, NORMAL).unwrap(); // existing mutex retains recursive kind
        assert_eq!(m.lock(&memory, 0x1008, 1).unwrap(), LockOutcome::Acquired);
        assert_eq!(m.try_lock(&memory, 0x1008, 1).unwrap(), 0);
        assert_eq!(m.try_lock(&memory, 0x1008, 2).unwrap(), EBUSY);
        let LockOutcome::Pending(ticket) = m.lock(&memory, 0x1008, 2).unwrap() else {
            panic!()
        };
        assert_eq!(m.unlock(&memory, 0x1008, 2).unwrap(), EPERM);
        assert_eq!(m.destroy(&mut memory, 0x1008).unwrap(), EBUSY);
        m.unlock(&memory, 0x1008, 1).unwrap();
        assert!(matches!(
            m.resume_lock(&memory, ticket).unwrap(),
            LockOutcome::Pending(_)
        ));
        m.unlock(&memory, 0x1008, 1).unwrap();
        assert_eq!(
            m.resume_lock(&memory, ticket).unwrap(),
            LockOutcome::Acquired
        );
        m.unlock(&memory, 0x1008, 2).unwrap();
        m.destroy(&mut memory, 0x1008).unwrap();
        assert_eq!(
            m.resume_lock(&memory, ticket).unwrap(),
            LockOutcome::Status(EINVAL)
        );
        m.init(&mut memory, 0x1008, 0).unwrap();
        assert_eq!(
            m.resume_lock(&memory, ticket).unwrap(),
            LockOutcome::Status(EINVAL)
        );
        m.attr_destroy(&mut memory, 0x1000).unwrap();
        assert_eq!(m.attribute_count(), 0);
    }
    #[test]
    fn kinds_guards_and_budgets() {
        let mut bytes = [0u8; 32];
        let mut ro = [0u8; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.add(0x2000, &mut ro, false, false).unwrap();
        let mut m = GuestMutex::default();
        for bad in [0x2000, 0x1019, u64::MAX] {
            assert!(m.attr_init(&mut memory, bad).is_err());
            assert!(m.init(&mut memory, bad, 0).is_err());
        }
        assert_eq!(m.attribute_count(), 0);
        assert_eq!(m.mutex_count(), 0);
        m.init(&mut memory, 0x1008, 0).unwrap();
        m.lock(&memory, 0x1008, 1).unwrap();
        assert_eq!(
            m.lock(&memory, 0x1008, 1).unwrap(),
            LockOutcome::Status(EDEADLK)
        );
        assert_eq!(m.try_lock(&memory, 0x1008, 1).unwrap(), EBUSY);
        m.unlock(&memory, 0x1008, 1).unwrap();
        m.destroy(&mut memory, 0x1008).unwrap();
        m.attr_init(&mut memory, 0x1000).unwrap();
        m.attr_settype(&memory, 0x1000, NORMAL).unwrap();
        m.init(&mut memory, 0x1008, 0x1000).unwrap();
        m.lock(&memory, 0x1008, 1).unwrap();
        assert!(matches!(
            m.lock(&memory, 0x1008, 1).unwrap(),
            LockOutcome::Pending(_)
        ));
        assert!(m.lock(&memory, 0x1010, 1).is_err());
        assert!(m.lock(&memory, 0x1008, 0).is_err());
        for _ in 1..ATTR_LIMIT {
            m.attr_init(&mut memory, 0x1000).unwrap();
        }
        let before = memory.load(0x1000, 8).unwrap();
        assert!(m.attr_init(&mut memory, 0x1000).is_err());
        assert_eq!(memory.load(0x1000, 8).unwrap(), before);
        for _ in 1..MUTEX_LIMIT {
            m.init(&mut memory, 0x1010, 0).unwrap();
        }
        let before = memory.load(0x1010, 8).unwrap();
        assert!(m.init(&mut memory, 0x1010, 0).is_err());
        assert_eq!(memory.load(0x1010, 8).unwrap(), before);
        assert_eq!(m.mutex_count(), MUTEX_LIMIT);
    }
}
#[cfg(test)]
mod atomic_tests {
    use super::*;
    #[test]
    fn readonly_destroy_preserves_registry_and_recursion_overflow() {
        let mut slots = [0u8; 16];
        let mut alias = [0u8; 16];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut slots, true, false).unwrap();
        let mut m = GuestMutex::default();
        m.attr_init(&mut memory, 0x1000).unwrap();
        m.attr_settype(&memory, 0x1000, RECURSIVE).unwrap();
        m.init(&mut memory, 0x1008, 0x1000).unwrap();
        alias.copy_from_slice(memory.read(0x1000, 16).unwrap());
        memory.add(0x2000, &mut alias, false, false).unwrap();
        assert!(m.attr_destroy(&mut memory, 0x2000).is_err());
        assert!(m.destroy(&mut memory, 0x2008).is_err());
        assert_eq!(m.attribute_count(), 1);
        assert_eq!(m.mutex_count(), 1);
        m.lock(&memory, 0x1008, 1).unwrap();
        m.mutexes[0].depth = i32::MAX as u32 + 1;
        assert_eq!(m.try_lock(&memory, 0x1008, 1).unwrap(), EAGAIN);
        assert_eq!(m.mutexes[0].depth, i32::MAX as u32 + 1);
        assert_eq!(m.attr_settype(&memory, 0x1000, 99).unwrap(), EINVAL);
    }
}

#[cfg(test)]
mod cond_tests {
    use super::*;
    #[test]
    fn recursive_detach_restore_and_single_use() {
        let mut bytes = [0u8; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut m = GuestMutex::default();
        m.attr_init(&mut memory, 0x1000).unwrap();
        m.attr_settype(&memory, 0x1000, RECURSIVE).unwrap();
        m.init(&mut memory, 0x1008, 0x1000).unwrap();
        for _ in 0..3 {
            assert_eq!(m.try_lock(&memory, 0x1008, 1).unwrap(), 0);
        }
        let ticket = m
            .prepare_cond_wait(&memory, 0x1008, 1)
            .unwrap()
            .ok()
            .unwrap()
            .commit();
        assert_eq!(m.mutexes[0].depth, 0);
        assert_eq!(m.destroy(&mut memory, 0x1008).unwrap(), EBUSY);
        assert_eq!(m.try_lock(&memory, 0x1008, 2).unwrap(), 0);
        assert_eq!(m.cond_pending_owner(&memory, &ticket).unwrap(), Some(2));
        assert_eq!(
            m.reacquire_cond_wait(&memory, &ticket).unwrap(),
            CondReacquire::Pending
        );
        m.unlock(&memory, 0x1008, 2).unwrap();
        assert_eq!(
            m.reacquire_cond_wait(&memory, &ticket).unwrap(),
            CondReacquire::Acquired
        );
        assert!(m.reacquire_cond_wait(&memory, &ticket).is_err());
        for _ in 0..2 {
            m.unlock(&memory, 0x1008, 1).unwrap();
            assert_eq!(m.try_lock(&memory, 0x1008, 2).unwrap(), EBUSY);
        }
        m.unlock(&memory, 0x1008, 1).unwrap();
        assert_eq!(m.destroy(&mut memory, 0x1008).unwrap(), 0);
    }
    #[test]
    fn invalid_dropped_foreign_and_replaced_slot_are_transactional() {
        let mut bytes = [0u8; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut m = GuestMutex::default();
        m.init(&mut memory, 0x1000, 0).unwrap();
        m.lock(&memory, 0x1000, 1).unwrap();
        assert!(matches!(
            m.prepare_cond_wait(&memory, 0x1000, 2),
            Ok(Err(EPERM))
        ));
        assert!(matches!(
            m.prepare_cond_wait(&memory, 0x1008, 1),
            Ok(Err(EINVAL))
        ));
        assert!(m.prepare_cond_wait(&memory, u64::MAX, 1).is_err());
        let plan = m
            .prepare_cond_wait(&memory, 0x1000, 1)
            .unwrap()
            .ok()
            .unwrap();
        assert!(memory.write(u64::MAX, &[0; 8]).is_err());
        drop(plan);
        assert_eq!(m.mutexes[0].owner, Some(1));
        assert_eq!(m.mutexes[0].depth, 1);
        assert!(m.cond_waiters.is_empty());
        assert_eq!(m.cond_next, 0);
        assert_eq!(Rc::strong_count(&m.cond_owner), 1);
        let ticket = m
            .prepare_cond_wait(&memory, 0x1000, 1)
            .unwrap()
            .ok()
            .unwrap()
            .commit();
        let mut other = GuestMutex::default();
        other.init(&mut memory, 0x1010, 0).unwrap();
        assert!(other.reacquire_cond_wait(&memory, &ticket).is_err());
        let identity = memory.load(0x1000, 8).unwrap();
        memory.store(0x1000, 0, 8).unwrap();
        assert!(m.reacquire_cond_wait(&memory, &ticket).is_err());
        assert_eq!(m.cond_waiters.len(), 1);
        memory.store(0x1000, identity, 8).unwrap();
        drop(ticket);
        assert_eq!(m.destroy(&mut memory, 0x1000).unwrap(), EBUSY);
    }
    #[test]
    fn multiple_waiters_retain_destroy_guard() {
        let mut bytes = [0u8; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut m = GuestMutex::default();
        m.init(&mut memory, 0x1000, 0).unwrap();
        m.lock(&memory, 0x1000, 1).unwrap();
        let first = m
            .prepare_cond_wait(&memory, 0x1000, 1)
            .unwrap()
            .ok()
            .unwrap()
            .commit();
        m.lock(&memory, 0x1000, 2).unwrap();
        let second = m
            .prepare_cond_wait(&memory, 0x1000, 2)
            .unwrap()
            .ok()
            .unwrap()
            .commit();
        assert_eq!(
            m.reacquire_cond_wait(&memory, &first).unwrap(),
            CondReacquire::Acquired
        );
        m.unlock(&memory, 0x1000, 1).unwrap();
        assert_eq!(m.destroy(&mut memory, 0x1000).unwrap(), EBUSY);
        assert_eq!(
            m.reacquire_cond_wait(&memory, &second).unwrap(),
            CondReacquire::Acquired
        );
        m.unlock(&memory, 0x1000, 2).unwrap();
        assert_eq!(m.destroy(&mut memory, 0x1000).unwrap(), 0);
    }
}
