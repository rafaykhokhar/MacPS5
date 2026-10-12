/*
 * Copyright (C) 2010 David Xu <davidxu@freebsd.org>.
 * All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice(s), this list of conditions and the following disclaimer as
 *    the first lines of this file unmodified other than the possible
 *    addition of one or more copyright notices.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice(s), this list of conditions and the following disclaimer in
 *    the documentation and/or other materials provided with the
 *    distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDER(S) ``AS IS'' AND ANY
 * EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
 * PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE COPYRIGHT HOLDER(S) BE
 * LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
 * CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
 * SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR
 * BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
 * WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE
 * OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE,
 * EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 *
 * $FreeBSD$
 */
/*-
 * Copyright (c) 2015, 2016 The FreeBSD Foundation
 * Copyright (c) 2004, David Xu <davidxu@freebsd.org>
 * Copyright (c) 2002, Jeffrey Roberson <jeff@freebsd.org>
 * All rights reserved.
 *
 * Portions of this software were developed by Konstantin Belousov
 * under sponsorship from the FreeBSD Foundation.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice unmodified, this list of conditions, and the following
 *    disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 *
 * THIS SOFTWARE IS PROVIDED BY THE AUTHOR ``AS IS'' AND ANY EXPRESS OR
 * IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES
 * OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED.
 * IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY DIRECT, INDIRECT,
 * INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT
 * NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
 * DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
 * THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
 * (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
 * THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
 */
//! Process-private inline sem_t state and cooperative, replay-safe wait tickets.
//! ABI: SDK 10b539de8f2b6773a9a78caa7576a97dbd99b29a,
//! include/freebsd/{semaphore.h,sys/_umtx.h,sys/umtx.h}.
//! Policy: FreeBSD releng/11.4 lib/libc/gen/sem_new.c and
//! sys/kern/kern_umtx.c::do_sem2_wait/do_sem2_wake. See guest_semaphore/LICENSE.
//! No host semaphore, invented success, host sleep or process-shared mapping.
//! Named semaphores, signals/cancellation and destruction with pending waiters
//! remain explicit boundaries. The executor must retain the object's mapping
//! for a pending ticket; equal bytes cannot detect free/reallocation by itself.
use crate::{guest_memory::Memory, Result};
use std::rc::Rc;
pub const SIZE: usize = 16;
pub const MAGIC: u32 = 0x73656d32;
pub const MAX_VALUE: u32 = i32::MAX as u32;
pub const HAS_WAITERS: u32 = 1 << 31;
pub const OBJECT_LIMIT: usize = 256;
pub const WAIT_LIMIT: usize = 4096;
pub const EINVAL: u32 = 22;
pub const EAGAIN: u32 = 35;
pub const ETIMEDOUT: u32 = 60;
pub const EOVERFLOW: u32 = 84;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Realtime {
    pub seconds: i64,
    pub nanoseconds: i64,
}
impl Realtime {
    fn valid(self) -> bool {
        (0..1_000_000_000).contains(&self.nanoseconds)
    }
    fn from_memory(memory: &Memory<'_>, address: u64) -> Result<Self> {
        let b = memory.read(address, 16)?;
        Ok(Self {
            seconds: i64::from_le_bytes(b[..8].try_into().unwrap()),
            nanoseconds: i64::from_le_bytes(b[8..].try_into().unwrap()),
        })
    }
    fn nanos(self) -> i128 {
        i128::from(self.seconds) * 1_000_000_000 + i128::from(self.nanoseconds)
    }
}
#[derive(Clone, Debug)]
pub struct WaitTicket {
    owner: Rc<()>,
    address: u64,
    generation: u64,
    identity: u64,
    thread: u64,
}
impl WaitTicket {
    pub fn thread_id(&self) -> u64 {
        self.thread
    }
    pub fn object_range(&self) -> (u64, usize) {
        (self.address, SIZE)
    }
}
#[derive(Clone, Debug)]
pub enum Outcome {
    Success,
    Value(u32),
    Errno(u32),
    Pending(WaitTicket),
}
#[derive(Clone, Debug)]
struct Waiter {
    identity: u64,
    thread: u64,
    queued: bool,
    deadline: Option<Realtime>,
}
#[derive(Clone, Debug)]
struct Object {
    address: u64,
    generation: u64,
    bytes: [u8; SIZE],
    waiters: Vec<Waiter>,
}
impl Object {
    fn count(&self) -> u32 {
        u32::from_le_bytes(self.bytes[4..8].try_into().unwrap())
    }
    fn set_count(&mut self, count: u32) {
        self.bytes[4..8].copy_from_slice(&count.to_le_bytes());
    }
}
#[derive(Debug)]
enum Change {
    None,
    Put(Object),
    Remove(u64),
}
/// Caller may inspect result/errno and write range before guarding its own ABI
/// return frame and errno. Commit rechecks both model revision and all16 bytes.
/// A plan is not Clone; rejected plans never change guest bytes or ownership.
#[derive(Debug)]
pub struct Plan {
    owner: Rc<()>,
    revision: u64,
    address: u64,
    observed: Option<[u8; SIZE]>,
    write: Option<[u8; SIZE]>,
    change: Change,
    next: u64,
    outcome: Outcome,
}
impl Plan {
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }
    pub fn write_range(&self) -> Option<(u64, usize)> {
        self.write.as_ref().map(|_| (self.address, SIZE))
    }
    /// Protect errno/getvalue output from aliasing this owned object, including
    /// nonmutating failure plans. Invalid init value has no object access.
    pub fn object_range(&self) -> Option<(u64, usize)> {
        self.observed.as_ref().map(|_| (self.address, SIZE))
    }
}
pub struct GuestSemaphore {
    owner: Rc<()>,
    objects: Vec<Object>,
    next: u64,
    revision: u64,
}
impl Default for GuestSemaphore {
    fn default() -> Self {
        Self {
            owner: Rc::new(()),
            objects: Vec::new(),
            next: 1,
            revision: 0,
        }
    }
}
impl GuestSemaphore {
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
    pub fn pending_count(&self) -> usize {
        self.objects.iter().map(|o| o.waiters.len()).sum()
    }
    pub fn has_realtime_waits(&self) -> bool {
        self.objects.iter().any(|o| {
            o.waiters
                .iter()
                .any(|w| w.deadline.is_some() && (w.queued || o.count() & MAX_VALUE == 0))
        })
    }
    pub fn needs_realtime(&self, ticket: &WaitTicket, thread: u64) -> Result<bool> {
        let (o, index) = self.ticket(ticket, thread)?;
        let w = &o.waiters[index];
        Ok(w.deadline.is_some() && (w.queued || o.count() & MAX_VALUE == 0))
    }
    /// Pending waiters own the lifetime of the original inline object. Equal
    /// bytes at a recycled address do not transfer that ownership.
    pub fn has_pending_overlap(&self, address: u64, length: usize) -> bool {
        let end = address.saturating_add(length as u64);
        length != 0
            && self.objects.iter().any(|o| {
                !o.waiters.is_empty() && address < o.address + SIZE as u64 && o.address < end
            })
    }
    fn bytes(memory: &Memory<'_>, address: u64) -> Result<[u8; SIZE]> {
        memory
            .read(address, SIZE)?
            .try_into()
            .map_err(|_| "Invalid semaphore object size".into())
    }
    fn plan(&self, address: u64, bytes: [u8; SIZE], outcome: Outcome) -> Plan {
        Plan {
            owner: self.owner.clone(),
            revision: self.revision,
            address,
            observed: Some(bytes),
            write: None,
            change: Change::None,
            next: self.next,
            outcome,
        }
    }
    fn changed(&self, mut plan: Plan, object: Object) -> Plan {
        plan.write = Some(object.bytes);
        plan.change = Change::Put(object);
        plan
    }
    fn lookup(&self, address: u64, bytes: &[u8; SIZE]) -> Result<Option<&Object>> {
        if u32::from_le_bytes(bytes[..4].try_into().unwrap()) != MAGIC {
            return Ok(None);
        }
        let o = self
            .objects
            .iter()
            .find(|o| o.address == address)
            .ok_or("Unowned or copied semaphore object")?;
        if o.bytes != *bytes {
            return Err("Unsupported semaphore metadata mutation".into());
        }
        Ok(Some(o))
    }
    fn ticket<'a>(&'a self, ticket: &WaitTicket, thread: u64) -> Result<(&'a Object, usize)> {
        if !Rc::ptr_eq(&self.owner, &ticket.owner) || thread != ticket.thread {
            return Err("Foreign semaphore wait ticket".into());
        }
        let o = self
            .objects
            .iter()
            .find(|o| o.address == ticket.address && o.generation == ticket.generation)
            .ok_or("Retired semaphore wait generation")?;
        let index = o
            .waiters
            .iter()
            .position(|w| w.identity == ticket.identity && w.thread == thread)
            .ok_or("Consumed or abandoned semaphore wait ticket")?;
        Ok((o, index))
    }
    /// Errors (including invalid value before pointer access) are returned as a
    /// prepared status; no errno is written here. Shared synchronization needs a
    /// process-shared address/identity registry and is not silently emulated.
    pub fn init(
        &self,
        memory: &Memory<'_>,
        address: u64,
        pshared: i32,
        value: u32,
    ) -> Result<Plan> {
        if value > MAX_VALUE {
            // Invalid value does not dereference sem in the reference.
            let mut p = self.plan(address, [0; SIZE], Outcome::Errno(EINVAL));
            p.observed = None;
            return Ok(p);
        }
        if pshared != 0 {
            return Err("Process-shared semaphores unsupported".into());
        }
        let observed = Self::bytes(memory, address)?;
        if self.objects.iter().any(|o| o.address == address) {
            return Err("Reinitializing a live semaphore unsupported".into());
        }
        if self.objects.len() == OBJECT_LIMIT {
            return Err("Guest semaphore object budget exhausted".into());
        }
        let end = address
            .checked_add(SIZE as u64)
            .ok_or("Semaphore object address overflow")?;
        if self
            .objects
            .iter()
            .any(|o| address < o.address + SIZE as u64 && o.address < end)
        {
            return Err("Overlapping semaphore object storage".into());
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or("Semaphore identity exhausted")?;
        let mut bytes = [0; SIZE];
        bytes[..4].copy_from_slice(&MAGIC.to_le_bytes());
        bytes[4..8].copy_from_slice(&value.to_le_bytes());
        let mut p = self.changed(
            self.plan(address, observed, Outcome::Success),
            Object {
                address,
                generation: self.next,
                bytes,
                waiters: Vec::new(),
            },
        );
        p.next = next;
        Ok(p)
    }
    pub fn destroy(&self, memory: &Memory<'_>, address: u64) -> Result<Plan> {
        let bytes = Self::bytes(memory, address)?;
        let Some(o) = self.lookup(address, &bytes)? else {
            return Ok(self.plan(address, bytes, Outcome::Errno(EINVAL)));
        };
        if !o.waiters.is_empty() {
            return Err("Destroying semaphore with pending waiters unsupported".into());
        }
        let mut p = self.plan(address, bytes, Outcome::Success);
        let mut after = bytes;
        after[..4].fill(0);
        p.write = Some(after);
        p.change = Change::Remove(o.generation);
        Ok(p)
    }
    pub fn getvalue(&self, memory: &Memory<'_>, address: u64) -> Result<Plan> {
        let b = Self::bytes(memory, address)?;
        Ok(self.plan(
            address,
            b,
            match self.lookup(address, &b)? {
                Some(o) => Outcome::Value(o.count() & MAX_VALUE),
                None => Outcome::Errno(EINVAL),
            },
        ))
    }
    pub fn trywait(&self, memory: &Memory<'_>, address: u64) -> Result<Plan> {
        let b = Self::bytes(memory, address)?;
        let Some(o) = self.lookup(address, &b)? else {
            return Ok(self.plan(address, b, Outcome::Errno(EINVAL)));
        };
        if o.count() & MAX_VALUE == 0 {
            return Ok(self.plan(address, b, Outcome::Errno(EAGAIN)));
        }
        let mut after = o.clone();
        after.set_count(o.count() - 1);
        Ok(self.changed(self.plan(address, b, Outcome::Success), after))
    }
    pub fn post(&self, memory: &Memory<'_>, address: u64) -> Result<Plan> {
        let b = Self::bytes(memory, address)?;
        let Some(o) = self.lookup(address, &b)? else {
            return Ok(self.plan(address, b, Outcome::Errno(EINVAL)));
        };
        if o.count() & MAX_VALUE == MAX_VALUE {
            return Ok(self.plan(address, b, Outcome::Errno(EOVERFLOW)));
        }
        let mut after = o.clone();
        let mut count = o.count() + 1;
        if count & HAS_WAITERS != 0 {
            let queued = after.waiters.iter().filter(|w| w.queued).count();
            if queued == 1 {
                count &= MAX_VALUE
            }
            if let Some(w) = after.waiters.iter_mut().find(|w| w.queued) {
                w.queued = false;
            }
        }
        after.set_count(count);
        Ok(self.changed(self.plan(address, b, Outcome::Success), after))
    }
    /// Zero timeout pointer means an untimed wait. Read a non-NULL deadline only
    /// when count==0, preserving sem_timedwait's immediate-success ordering.
    /// Realtime comes from the guest clock provider, never host wallclock here.
    pub fn wait(
        &self,
        memory: &Memory<'_>,
        address: u64,
        thread: u64,
        timeout: u64,
        now: impl FnOnce() -> Result<Realtime>,
    ) -> Result<Plan> {
        if thread == 0 {
            return Err("Semaphore wait requires guest thread identity".into());
        }
        if self
            .objects
            .iter()
            .any(|o| o.waiters.iter().any(|w| w.thread == thread))
        {
            return Err("Guest thread already owns semaphore wait".into());
        }
        let b = Self::bytes(memory, address)?;
        let Some(o) = self.lookup(address, &b)? else {
            return Ok(self.plan(address, b, Outcome::Errno(EINVAL)));
        };
        if o.count() & MAX_VALUE != 0 {
            return self.trywait(memory, address);
        }
        let mut after = o.clone();
        let mut deadline = None;
        if timeout != 0 {
            let t = Realtime::from_memory(memory, timeout)?;
            if !t.valid() || t.seconds < 0 {
                return Ok(self.plan(address, b, Outcome::Errno(EINVAL)));
            }
            let current = now()?;
            if !current.valid() {
                return Err("Invalid guest realtime clock snapshot".into());
            }
            after.set_count(HAS_WAITERS);
            if t <= current {
                return Ok(self.changed(self.plan(address, b, Outcome::Errno(ETIMEDOUT)), after));
            }
            deadline = Some(t);
        }
        if self.pending_count() == WAIT_LIMIT {
            return Err("Guest semaphore wait budget exhausted".into());
        }
        let next = self
            .next
            .checked_add(1)
            .ok_or("Semaphore wait identity exhausted")?;
        let ticket = WaitTicket {
            owner: self.owner.clone(),
            address,
            generation: o.generation,
            identity: self.next,
            thread,
        };
        after.waiters.push(Waiter {
            identity: self.next,
            thread,
            queued: true,
            deadline,
        });
        after.set_count(HAS_WAITERS);
        let mut p = self.changed(self.plan(address, b, Outcome::Pending(ticket)), after);
        p.next = next;
        Ok(p)
    }
    /// Scheduler hint. A permit is not reserved by post or a readiness check.
    /// A realtime jump can alter readiness; recheck against fresh time on resume.
    pub fn ready(&self, ticket: &WaitTicket, thread: u64, now: Option<Realtime>) -> Result<bool> {
        let (o, index) = self.ticket(ticket, thread)?;
        let w = &o.waiters[index];
        Ok(!w.queued || Self::expired(w, now)?)
    }
    fn expired(w: &Waiter, now: Option<Realtime>) -> Result<bool> {
        match w.deadline {
            None => Ok(false),
            Some(t) => {
                let now = now
                    .filter(|t| t.valid())
                    .ok_or("Timed semaphore wait requires current realtime")?;
                Ok(t <= now)
            }
        }
    }
    /// A monotonic scheduling delay is a hint only. Completion must still call
    /// resume with fresh realtime; backward jumps extend the wait, forward jumps
    /// expire it. Values are u128 nanoseconds to avoid timestamp subtraction wrap.
    pub fn delay_ns(
        &self,
        ticket: &WaitTicket,
        thread: u64,
        now: Realtime,
    ) -> Result<Option<u128>> {
        if !now.valid() {
            return Err("Invalid realtime snapshot".into());
        }
        let (o, i) = self.ticket(ticket, thread)?;
        Ok(o.waiters[i]
            .deadline
            .map(|t| (t.nanos() - now.nanos()).max(0) as u128))
    }
    pub fn resume(
        &self,
        memory: &Memory<'_>,
        ticket: &WaitTicket,
        thread: u64,
        now: Option<Realtime>,
    ) -> Result<Plan> {
        let (o, index) = self.ticket(ticket, thread)?;
        let b = Self::bytes(memory, o.address)?;
        self.lookup(o.address, &b)?
            .ok_or("Semaphore magic changed during wait")?;
        let w = &o.waiters[index];
        // The libc retry checks count before inspecting a timeout/error. A
        // notified waiter with a permit needs no further wallclock read.
        let expired = if !w.queued && o.count() & MAX_VALUE != 0 {
            false
        } else {
            Self::expired(w, now)?
        };
        if w.queued && !expired {
            return Ok(self.plan(o.address, b, Outcome::Pending(ticket.clone())));
        }
        let mut after = o.clone();
        if o.count() & MAX_VALUE != 0 {
            after.set_count(o.count() - 1);
            after.waiters.remove(index);
            return Ok(self.changed(self.plan(o.address, b, Outcome::Success), after));
        }
        if expired {
            after.waiters.remove(index);
            return Ok(self.changed(self.plan(o.address, b, Outcome::Errno(ETIMEDOUT)), after));
        }
        // Woken, but another guest consumed the posted permit before this dispatch.
        after.waiters[index].queued = true;
        after.set_count(HAS_WAITERS);
        Ok(self.changed(
            self.plan(o.address, b, Outcome::Pending(ticket.clone())),
            after,
        ))
    }
    /// Process/thread teardown only: remove ownership without fabricating a guest
    /// EINTR or cancellation result. Source timeout removal also leaves waiters bit.
    pub fn abandon(&mut self, ticket: &WaitTicket, thread: u64) -> Result<()> {
        let (o, index) = self.ticket(ticket, thread)?;
        let address = o.address;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Semaphore revision exhausted")?;
        self.objects
            .iter_mut()
            .find(|o| o.address == address)
            .unwrap()
            .waiters
            .remove(index);
        self.revision = revision;
        Ok(())
    }
    pub fn commit(&mut self, memory: &mut Memory<'_>, plan: Plan) -> Result<Outcome> {
        if !Rc::ptr_eq(&self.owner, &plan.owner) || plan.revision != self.revision {
            return Err("Foreign or stale semaphore plan".into());
        }
        if let Some(observed) = plan.observed {
            if Self::bytes(memory, plan.address)? != observed {
                return Err("Semaphore object changed before commit".into());
            }
        }
        let mutation = !matches!(plan.change, Change::None);
        let revision = if mutation {
            self.revision
                .checked_add(1)
                .ok_or("Semaphore revision exhausted")?
        } else {
            self.revision
        };
        if let Some(bytes) = plan.write {
            memory.validate_write(plan.address, SIZE)?;
            if let Change::Put(o) = &plan.change {
                if !self.objects.iter().any(|v| v.address == o.address) {
                    self.objects
                        .try_reserve(1)
                        .map_err(|_| "Cannot reserve semaphore state")?
                }
            }
            memory.write(plan.address, &bytes)?;
        }
        match plan.change {
            Change::None => {}
            Change::Remove(g) => self.objects.retain(|o| o.generation != g),
            Change::Put(o) => {
                if let Some(old) = self.objects.iter_mut().find(|old| old.address == o.address) {
                    *old = o
                } else {
                    self.objects.push(o)
                }
            }
        }
        self.next = plan.next;
        self.revision = revision;
        Ok(plan.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: u64 = 0x1000;
    fn success(outcome: Outcome) {
        assert!(matches!(outcome, Outcome::Success));
    }
    fn errno(outcome: Outcome, n: u32) {
        assert!(matches!(outcome,Outcome::Errno(e)if e==n));
    }
    fn pending(outcome: Outcome) -> WaitTicket {
        match outcome {
            Outcome::Pending(t) => t,
            _ => panic!("expected pending semaphore"),
        }
    }
    fn init(s: &mut GuestSemaphore, m: &mut Memory<'_>, value: u32) {
        let p = s.init(m, A, 0, value).unwrap();
        success(s.commit(m, p).unwrap());
    }
    fn never() -> Result<Realtime> {
        panic!("untimed/immediate wait read clock")
    }
    #[test]
    fn inline_layout_counts_errno_and_destroy() {
        let mut bytes = [0xa5; 64];
        let mut memory = Memory::default();
        memory.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore::default();
        init(&mut s, &mut memory, 2);
        assert_eq!(
            memory.read(A, SIZE).unwrap(),
            [0x32, 0x6d, 0x65, 0x73, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        for _ in 0..2 {
            let p = s.trywait(&memory, A).unwrap();
            success(s.commit(&mut memory, p).unwrap());
        }
        let p = s.trywait(&memory, A).unwrap();
        errno(s.commit(&mut memory, p).unwrap(), EAGAIN);
        let p = s.post(&memory, A).unwrap();
        success(s.commit(&mut memory, p).unwrap());
        let p = s.getvalue(&memory, A).unwrap();
        assert!(matches!(
            s.commit(&mut memory, p).unwrap(),
            Outcome::Value(1)
        ));
        let p = s.destroy(&memory, A).unwrap();
        success(s.commit(&mut memory, p).unwrap());
        assert_eq!(memory.load(A, 4).unwrap(), 0);
        assert_eq!(memory.load(A + 4, 4).unwrap(), 1);
        assert_eq!(s.object_count(), 0);
        let p = s.destroy(&memory, A).unwrap();
        errno(s.commit(&mut memory, p).unwrap(), EINVAL);
        init(&mut s, &mut memory, MAX_VALUE);
        let p = s.post(&memory, A).unwrap();
        errno(s.commit(&mut memory, p).unwrap(), EOVERFLOW);
        assert_eq!(memory.load(A + 4, 4).unwrap(), u64::from(MAX_VALUE));
    }
    #[test]
    fn guards_and_plans_are_atomic() {
        let mut bytes = [0xa5; 16];
        let mut memory = Memory::default();
        memory.add(A, &mut bytes, false, false).unwrap();
        let mut s = GuestSemaphore::default();
        let p = s.init(&memory, A, 0, 1).unwrap();
        assert_eq!(p.write_range(), Some((A, 16)));
        assert!(s.commit(&mut memory, p).is_err());
        assert_eq!(s.object_count(), 0);
        assert_eq!(memory.read(A, 16).unwrap(), [0xa5; 16]);
        let p = s.init(&memory, u64::MAX, 1, u32::MAX).unwrap();
        assert!(matches!(p.outcome(), Outcome::Errno(EINVAL)));
        assert_eq!(p.write_range(), None);
        assert_eq!(p.object_range(), None);
        errno(s.commit(&mut memory, p).unwrap(), EINVAL);
        assert!(s.init(&memory, A, 1, 0).unwrap_err().contains("shared"));
    }
    #[test]
    fn wake_race_requeue_foreign_and_replay() {
        let mut bytes = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore::default();
        init(&mut s, &mut m, 0);
        let p = s.wait(&m, A, 7, 0, never).unwrap();
        let ticket = pending(s.commit(&mut m, p).unwrap());
        assert_eq!(ticket.object_range(), (A, 16));
        assert_eq!(ticket.thread_id(), 7);
        assert!(!s.ready(&ticket, 7, None).unwrap());
        assert_eq!(m.load(A + 4, 4).unwrap(), u64::from(HAS_WAITERS));
        assert!(s.destroy(&m, A).is_err());
        assert!(s.init(&m, A, 0, 1).is_err());
        assert!(s.wait(&m, A, 7, 0, never).is_err());
        assert!(GuestSemaphore::default().ready(&ticket, 7, None).is_err());
        assert!(s.ready(&ticket, 8, None).is_err());
        let p = s.post(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert!(s.ready(&ticket, 7, None).unwrap());
        assert_eq!(m.load(A + 4, 4).unwrap(), 1);
        // Another thread steals the posted permit before the notified waiter runs.
        let p = s.trywait(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        let p = s.resume(&m, &ticket, 7, None).unwrap();
        let again = pending(s.commit(&mut m, p).unwrap());
        assert!(!s.ready(&again, 7, None).unwrap());
        let p = s.post(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        let p = s.resume(&m, &ticket, 7, None).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert_eq!(s.pending_count(), 0);
        assert!(s.resume(&m, &ticket, 7, None).is_err());
    }
    #[test]
    fn multiple_waiters_and_stale_plans() {
        let mut bytes = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore::default();
        init(&mut s, &mut m, 0);
        let p = s.wait(&m, A, 1, 0, never).unwrap();
        let a = pending(s.commit(&mut m, p).unwrap());
        let p = s.wait(&m, A, 2, 0, never).unwrap();
        let b = pending(s.commit(&mut m, p).unwrap());
        let stale = s.post(&m, A).unwrap();
        let p = s.post(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert!(s.commit(&mut m, stale).is_err());
        assert!(s.ready(&a, 1, None).unwrap());
        assert!(!s.ready(&b, 2, None).unwrap());
        assert_eq!(m.load(A + 4, 4).unwrap(), u64::from(HAS_WAITERS | 1));
        let p = s.post(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert!(s.ready(&b, 2, None).unwrap());
        assert_eq!(m.load(A + 4, 4).unwrap(), 2);
        let p = s.resume(&m, &b, 2, None).unwrap();
        success(s.commit(&mut m, p).unwrap());
        let p = s.resume(&m, &a, 1, None).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert_eq!(m.load(A + 4, 4).unwrap(), 0);
        let p = s.destroy(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        init(&mut s, &mut m, 0);
        assert!(s.ready(&a, 1, None).is_err());
    }
    #[test]
    fn timedwait_lazy_deadline_and_wallclock_jumps() {
        let mut bytes = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore::default();
        init(&mut s, &mut m, 1);
        let p = s.wait(&m, A, 1, u64::MAX, never).unwrap();
        success(s.commit(&mut m, p).unwrap()); // invalid deadline is untouched when permit exists
        m.store(A + 32, 10, 8).unwrap();
        m.store(A + 40, 1_000_000_000, 8).unwrap();
        let p = s.wait(&m, A, 1, A + 32, never).unwrap();
        errno(s.commit(&mut m, p).unwrap(), EINVAL);
        m.store(A + 40, 0, 8).unwrap();
        m.store(A + 32, u64::MAX, 8).unwrap();
        let p = s.wait(&m, A, 1, A + 32, never).unwrap();
        errno(s.commit(&mut m, p).unwrap(), EINVAL);
        assert_eq!(m.load(A + 4, 4).unwrap(), 0);
        assert_eq!(s.pending_count(), 0);
        m.store(A + 32, 10, 8).unwrap();
        let now = Realtime {
            seconds: 9,
            nanoseconds: 0,
        };
        let p = s.wait(&m, A, 1, A + 32, || Ok(now)).unwrap();
        let t = pending(s.commit(&mut m, p).unwrap());
        assert_eq!(s.delay_ns(&t, 1, now).unwrap(), Some(1_000_000_000));
        let backwards = Realtime {
            seconds: 7,
            nanoseconds: 0,
        };
        assert_eq!(s.delay_ns(&t, 1, backwards).unwrap(), Some(3_000_000_000));
        assert!(!s.ready(&t, 1, Some(backwards)).unwrap());
        let forward = Realtime {
            seconds: 11,
            nanoseconds: 0,
        };
        assert!(s.ready(&t, 1, Some(forward)).unwrap());
        let p = s.resume(&m, &t, 1, Some(forward)).unwrap();
        errno(s.commit(&mut m, p).unwrap(), ETIMEDOUT);
        assert!(s.ready(&t, 1, Some(forward)).is_err());
        assert_eq!(m.load(A + 4, 4).unwrap(), u64::from(HAS_WAITERS));
        let p = s.wait(&m, A, 2, A + 32, || Ok(forward)).unwrap();
        errno(s.commit(&mut m, p).unwrap(), ETIMEDOUT);
        assert_eq!(s.pending_count(), 0);
        // A wake carrying a permit wins before another deadline check.
        let p = s.wait(&m, A, 3, A + 32, || Ok(now)).unwrap();
        let t = pending(s.commit(&mut m, p).unwrap());
        let p = s.post(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        let p = s.resume(&m, &t, 3, None).unwrap();
        success(s.commit(&mut m, p).unwrap());
    }
    #[test]
    fn metadata_copy_mutation_and_abandon_are_detected() {
        let mut bytes = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore::default();
        init(&mut s, &mut m, 0);
        let copy = m.read(A, 16).unwrap().to_vec();
        m.write(A + 16, &copy).unwrap();
        assert!(s.trywait(&m, A + 16).is_err());
        let p = s.wait(&m, A, 1, 0, never).unwrap();
        let t = pending(s.commit(&mut m, p).unwrap());
        m.store(A + 12, 1, 4).unwrap();
        assert!(s.resume(&m, &t, 1, None).is_err());
        m.store(A + 12, 0, 4).unwrap();
        let p = s.post(&m, A).unwrap();
        m.store(A + 8, 1, 4).unwrap();
        assert!(s.commit(&mut m, p).is_err());
        assert_eq!(s.pending_count(), 1);
        m.store(A + 8, 0, 4).unwrap();
        s.abandon(&t, 1).unwrap();
        assert!(s.abandon(&t, 1).is_err());
        let p = s.destroy(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        for _ in 0..OBJECT_LIMIT + 10 {
            init(&mut s, &mut m, 0);
            let p = s.destroy(&m, A).unwrap();
            success(s.commit(&mut m, p).unwrap());
        }
        assert_eq!(s.object_count(), 0);
    }
    #[test]
    fn identity_budget_overlap_and_uncommitted_wait_preserve_state() {
        let mut bytes = vec![0; SIZE * (OBJECT_LIMIT + 1)];
        let mut m = Memory::default();
        m.add(A, &mut bytes, true, false).unwrap();
        let mut s = GuestSemaphore {
            next: u64::MAX,
            ..GuestSemaphore::default()
        };
        assert!(s.init(&m, A, 0, 0).is_err());
        assert_eq!(m.read(A, SIZE).unwrap(), [0; SIZE]);
        s.next = 1;
        init(&mut s, &mut m, 0);
        assert!(s.init(&m, A + 8, 0, 0).is_err());
        let p = s.wait(&m, A, 1, 0, never).unwrap();
        let ticket = match p.outcome() {
            Outcome::Pending(t) => t.clone(),
            _ => unreachable!(),
        };
        assert_eq!(s.pending_count(), 0);
        assert!(s.ready(&ticket, 1, None).is_err());
        drop(p);
        assert_eq!(m.load(A + 4, 4).unwrap(), 0);
        for i in 1..OBJECT_LIMIT {
            let p = s.init(&m, A + (i * SIZE) as u64, 0, 0).unwrap();
            success(s.commit(&mut m, p).unwrap());
        }
        let spare = A + (SIZE * OBJECT_LIMIT) as u64;
        assert!(s.init(&m, spare, 0, 0).is_err());
        assert_eq!(m.read(spare, SIZE).unwrap(), [0; SIZE]);
        let p = s.destroy(&m, A).unwrap();
        success(s.commit(&mut m, p).unwrap());
        let p = s.init(&m, spare, 0, 0).unwrap();
        success(s.commit(&mut m, p).unwrap());
        assert_eq!(s.object_count(), OBJECT_LIMIT);
    }
}
