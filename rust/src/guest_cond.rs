//! Owned process-private condition variables using eight-byte pointer slots.
//! Policy follows the documented FreeBSD condition contract; this module alone
//! does not release mutexes, retain guest mappings, or implement a scheduler.
//! Callers must retain condition/mutex backing until `finish` commits and must
//! publish a prepared wait together with a prevalidated full-depth mutex detach.
use crate::{Result, guest_memory::Memory};
use std::rc::Rc;

pub const OBJECT_LIMIT: usize = 256;
pub const WAIT_LIMIT: usize = 4096;
pub const EINVAL: i32 = 22;
pub const EBUSY: i32 = 16;
pub const ETIMEDOUT: i32 = 60;
pub const SLOT_SIZE: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Realtime {
    pub seconds: i64,
    pub nanoseconds: i64,
}
impl Realtime {
    pub fn valid(self) -> bool {
        (0..1_000_000_000).contains(&self.nanoseconds)
    }
    fn nanos(self) -> i128 {
        i128::from(self.seconds) * 1_000_000_000 + i128::from(self.nanoseconds)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Reacquiring(i32),
}
#[derive(Clone, Debug)]
pub struct WaitTicket {
    owner: Rc<()>,
    slot: u64,
    object: u64,
    identity: u64,
    thread: u64,
    mutex: u64,
}
impl WaitTicket {
    pub fn thread_id(&self) -> u64 {
        self.thread
    }
    pub fn object_range(&self) -> (u64, usize) {
        (self.slot, SLOT_SIZE)
    }
    pub fn mutex_address(&self) -> u64 {
        self.mutex
    }
}
#[derive(Clone, Debug)]
pub enum Outcome {
    Status(i32),
    Pending(WaitTicket),
    Reacquiring(i32),
}
#[derive(Clone, Debug)]
struct Waiter {
    identity: u64,
    thread: u64,
    mutex: u64,
    deadline: Option<Realtime>,
    phase: Phase,
}
#[derive(Debug)]
struct Object {
    slot: u64,
    identity: u64,
    waiters: Vec<Waiter>,
}
#[derive(Debug)]
pub struct Plan {
    owner: Rc<()>,
    revision: u64,
    slot: u64,
    observed: Option<u64>,
    write: Option<u64>,
    objects: Vec<Object>,
    next: u64,
    outcome: Outcome,
}
impl Plan {
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }
    pub fn write_range(&self) -> Option<(u64, usize)> {
        self.write.map(|_| (self.slot, SLOT_SIZE))
    }
    pub fn object_range(&self) -> Option<(u64, usize)> {
        self.observed.map(|_| (self.slot, SLOT_SIZE))
    }
}
#[derive(Debug)]
pub struct GuestCond {
    owner: Rc<()>,
    revision: u64,
    next: u64,
    objects: Vec<Object>,
}
impl Default for GuestCond {
    fn default() -> Self {
        Self {
            owner: Rc::new(()),
            revision: 0,
            next: 0x20000,
            objects: Vec::new(),
        }
    }
}
impl GuestCond {
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
    pub fn pending_count(&self) -> usize {
        self.objects.iter().map(|o| o.waiters.len()).sum()
    }
    pub fn has_pending_overlap(&self, address: u64, length: usize) -> bool {
        let end = address.saturating_add(length as u64);
        length != 0
            && self.objects.iter().any(|o| {
                !o.waiters.is_empty() && address < o.slot.saturating_add(8) && o.slot < end
            })
    }
    fn plan(&self, memory: &Memory<'_>, slot: u64) -> Result<Plan> {
        let observed = memory.load(slot, 8)?;
        let mut objects = Vec::new();
        objects
            .try_reserve(self.objects.len() + 1)
            .map_err(|_| "Cannot reserve condition objects")?;
        for o in &self.objects {
            let mut waiters = Vec::new();
            waiters
                .try_reserve(o.waiters.len() + 1)
                .map_err(|_| "Cannot reserve condition waiters")?;
            waiters.extend(o.waiters.iter().cloned());
            objects.push(Object {
                slot: o.slot,
                identity: o.identity,
                waiters,
            });
        }
        Ok(Plan {
            owner: self.owner.clone(),
            revision: self.revision,
            slot,
            observed: Some(observed),
            write: None,
            objects,
            next: self.next,
            outcome: Outcome::Status(0),
        })
    }
    fn allocate(plan: &mut Plan) -> Result<usize> {
        if plan.objects.len() >= OBJECT_LIMIT {
            return Err("Guest condition object budget exhausted".into());
        }
        plan.next = plan
            .next
            .checked_add(1)
            .ok_or("Condition identity exhausted")?;
        plan.objects.push(Object {
            slot: plan.slot,
            identity: plan.next,
            waiters: Vec::new(),
        });
        plan.write = Some(plan.next);
        Ok(plan.objects.len() - 1)
    }
    fn resolve(plan: &mut Plan, lazy: bool) -> Result<Option<usize>> {
        if plan.observed == Some(0) && lazy {
            // Resetting an owned slot cannot replace an object with retained waiters.
            if plan.objects.iter().any(|o| o.slot == plan.slot) {
                plan.outcome = Outcome::Status(EINVAL);
                return Ok(None);
            }
            return Self::allocate(plan).map(Some);
        }
        let i = plan
            .objects
            .iter()
            .position(|o| o.slot == plan.slot && Some(o.identity) == plan.observed);
        if i.is_none() {
            plan.outcome = Outcome::Status(EINVAL);
        }
        Ok(i)
    }
    /// Explicit initialization. Nondefault attributes are an explicit boundary
    /// until attribute objects/clocks are implemented, never silently ignored.
    pub fn init(&self, memory: &Memory<'_>, slot: u64, attr_slot: u64) -> Result<Plan> {
        if attr_slot != 0 {
            return Err("Condition attributes unsupported".into());
        }
        let mut p = self.plan(memory, slot)?;
        if p.objects.iter().any(|o| o.slot == slot) {
            p.outcome = Outcome::Status(EBUSY);
        } else {
            Self::allocate(&mut p)?;
        }
        Ok(p)
    }
    pub fn destroy(&self, memory: &Memory<'_>, slot: u64) -> Result<Plan> {
        let mut p = self.plan(memory, slot)?;
        if p.observed == Some(0) && !p.objects.iter().any(|o| o.slot == slot) {
            // Destroying the static initializer succeeds without changing it.
        } else if let Some(i) = Self::resolve(&mut p, false)? {
            if !p.objects[i].waiters.is_empty() {
                p.outcome = Outcome::Status(EBUSY);
            } else {
                p.objects.remove(i);
                p.write = Some(1);
            }
        }
        Ok(p)
    }
    pub fn signal(&self, memory: &Memory<'_>, slot: u64, broadcast: bool) -> Result<Plan> {
        let mut p = self.plan(memory, slot)?;
        if let Some(i) = Self::resolve(&mut p, true)? {
            for w in &mut p.objects[i].waiters {
                if w.phase == Phase::Waiting {
                    w.phase = Phase::Reacquiring(0);
                    if !broadcast {
                        break;
                    }
                }
            }
        }
        Ok(p)
    }
    /// Validate timespec before touching condition storage. The caller must also
    /// do this before resolving the mutex, matching timed-wait error precedence.
    pub fn wait(
        &self,
        memory: &Memory<'_>,
        slot: u64,
        thread: u64,
        mutex: u64,
        deadline: Option<Realtime>,
    ) -> Result<Plan> {
        if deadline.is_some_and(|t| !t.valid()) {
            // No condition memory access for an invalid timespec.
            return Ok(Plan {
                owner: self.owner.clone(),
                revision: self.revision,
                slot,
                observed: None,
                write: None,
                objects: Vec::new(),
                next: self.next,
                outcome: Outcome::Status(EINVAL),
            });
        }
        if thread == 0 || mutex == 0 {
            return Err("Invalid condition wait thread/mutex identity".into());
        }
        if slot < mutex.saturating_add(SLOT_SIZE as u64)
            && mutex < slot.saturating_add(SLOT_SIZE as u64)
        {
            return Err("Condition and mutex slots overlap".into());
        }
        if self.pending_count() >= WAIT_LIMIT {
            return Err("Guest condition waiter budget exhausted".into());
        }
        if self
            .objects
            .iter()
            .any(|o| o.waiters.iter().any(|w| w.thread == thread))
        {
            return Err("Thread already has a condition wait".into());
        }
        let mut p = self.plan(memory, slot)?;
        if let Some(i) = Self::resolve(&mut p, true)? {
            if p.objects[i]
                .waiters
                .iter()
                .any(|w| w.phase == Phase::Waiting && w.mutex != mutex)
            {
                return Err("Concurrent condition waits use different mutexes".into());
            }
            p.next = p
                .next
                .checked_add(1)
                .ok_or("Condition wait identity exhausted")?;
            p.objects[i]
                .waiters
                .try_reserve(1)
                .map_err(|_| "Cannot reserve condition waiter")?;
            p.objects[i].waiters.push(Waiter {
                identity: p.next,
                thread,
                mutex,
                deadline,
                phase: Phase::Waiting,
            });
            p.outcome = Outcome::Pending(WaitTicket {
                owner: self.owner.clone(),
                slot,
                object: p.objects[i].identity,
                identity: p.next,
                thread,
                mutex,
            });
        }
        Ok(p)
    }
    fn ticket(&self, ticket: &WaitTicket, thread: u64) -> Result<(&Object, usize)> {
        if !Rc::ptr_eq(&self.owner, &ticket.owner) || thread != ticket.thread {
            return Err("Foreign condition wait ticket".into());
        }
        let o = self
            .objects
            .iter()
            .find(|o| o.slot == ticket.slot && o.identity == ticket.object)
            .ok_or("Stale condition object")?;
        let i = o
            .waiters
            .iter()
            .position(|w| {
                w.identity == ticket.identity && w.thread == thread && w.mutex == ticket.mutex
            })
            .ok_or("Consumed condition wait ticket")?;
        Ok((o, i))
    }
    pub fn phase(&self, ticket: &WaitTicket, thread: u64) -> Result<Phase> {
        let (o, i) = self.ticket(ticket, thread)?;
        Ok(o.waiters[i].phase)
    }
    pub fn has_realtime_waits(&self) -> bool {
        self.objects.iter().any(|o| {
            o.waiters
                .iter()
                .any(|w| w.phase == Phase::Waiting && w.deadline.is_some())
        })
    }
    pub fn delay_ns(&self, ticket: &WaitTicket, thread: u64, now: Realtime) -> Result<Option<u64>> {
        if !now.valid() {
            return Err("Invalid current condition clock".into());
        }
        let (o, i) = self.ticket(ticket, thread)?;
        let w = &o.waiters[i];
        if w.phase != Phase::Waiting {
            return Ok(Some(0));
        }
        Ok(w.deadline
            .map(|d| (d.nanos() - now.nanos()).clamp(0, i128::from(u64::MAX)) as u64))
    }
    /// Select timeout only while still waiting. Once signalled, timeout cannot
    /// replace success while mutex reacquisition is blocked (and vice versa).
    pub fn select(
        &self,
        memory: &Memory<'_>,
        ticket: &WaitTicket,
        thread: u64,
        now: Option<Realtime>,
    ) -> Result<Plan> {
        let (o, i) = self.ticket(ticket, thread)?;
        if now.is_some_and(|t| !t.valid()) {
            return Err("Invalid current condition clock".into());
        }
        let mut phase = o.waiters[i].phase;
        if phase == Phase::Waiting {
            if let Some(deadline) = o.waiters[i].deadline {
                if now.ok_or("Condition realtime clock required")? >= deadline {
                    phase = Phase::Reacquiring(ETIMEDOUT);
                }
            }
        }
        let mut p = self.plan(memory, ticket.slot)?;
        let index = Self::resolve(&mut p, false)?.ok_or("Condition slot changed during wait")?;
        p.objects[index].waiters[i].phase = phase;
        p.outcome = match phase {
            Phase::Waiting => Outcome::Pending(ticket.clone()),
            Phase::Reacquiring(s) => Outcome::Reacquiring(s),
        };
        Ok(p)
    }
    /// Prepare removal only AFTER executor has prepared successful mutex
    /// reacquisition. Commit paired with that reacquisition, never on a timeout
    /// merely becoming eligible. There is intentionally no abandon/cancel API.
    pub fn finish(&self, memory: &Memory<'_>, ticket: &WaitTicket, thread: u64) -> Result<Plan> {
        let (o, i) = self.ticket(ticket, thread)?;
        let Phase::Reacquiring(status) = o.waiters[i].phase else {
            return Err("Condition is still waiting".into());
        };
        let mut p = self.plan(memory, ticket.slot)?;
        let index =
            Self::resolve(&mut p, false)?.ok_or("Condition slot changed during reacquisition")?;
        p.objects[index].waiters.remove(i);
        p.outcome = Outcome::Status(status);
        Ok(p)
    }
    pub fn validate(&self, memory: &Memory<'_>, p: &Plan) -> Result<()> {
        if !Rc::ptr_eq(&self.owner, &p.owner) || self.revision != p.revision {
            return Err("Foreign or stale condition plan".into());
        }
        self.revision
            .checked_add(1)
            .ok_or("Condition revision exhausted")?;
        if let Some(observed) = p.observed {
            if memory.load(p.slot, 8)? != observed {
                return Err("Condition slot changed before commit".into());
            }
        }
        if p.write.is_some() {
            memory.validate_write(p.slot, 8)?;
        }
        Ok(())
    }
    /// All fallible checks precede publication. After the optional guest slot
    /// write, replacement is allocation-free. Pair with an infallible mutex
    /// detach/reacquire permit to keep the two models atomic.
    pub fn commit(&mut self, memory: &mut Memory<'_>, p: Plan) -> Result<Outcome> {
        self.validate(memory, &p)?;
        if let Some(value) = p.write {
            memory.write(p.slot, &value.to_le_bytes())?;
        }
        if p.observed.is_some() {
            self.objects = p.objects;
            self.next = p.next;
            self.revision += 1;
        }
        Ok(p.outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: u64 = 0x1000;
    fn status(o: Outcome, n: i32) {
        assert!(matches!(o, Outcome::Status(s) if s==n));
    }
    fn pending(o: Outcome) -> WaitTicket {
        match o {
            Outcome::Pending(t) => t,
            _ => panic!("expected pending"),
        }
    }
    fn wait(
        c: &mut GuestCond,
        m: &mut Memory<'_>,
        thread: u64,
        deadline: Option<Realtime>,
    ) -> WaitTicket {
        let p = c.wait(m, A, thread, A + 32, deadline).unwrap();
        pending(c.commit(m, p).unwrap())
    }
    #[test]
    fn nonbanked_signal_broadcast_and_single_use() {
        let mut b = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        let mut c = GuestCond::default();
        let p = c.signal(&m, A, false).unwrap();
        assert_eq!(p.write_range(), Some((A, 8)));
        status(c.commit(&mut m, p).unwrap(), 0);
        let t1 = wait(&mut c, &mut m, 1, None);
        let t2 = wait(&mut c, &mut m, 2, None);
        assert_eq!(c.phase(&t1, 1).unwrap(), Phase::Waiting);
        assert!(c.finish(&m, &t1, 1).is_err());
        let p = c.signal(&m, A, false).unwrap();
        c.commit(&mut m, p).unwrap();
        assert_eq!(c.phase(&t1, 1).unwrap(), Phase::Reacquiring(0));
        assert_eq!(c.phase(&t2, 2).unwrap(), Phase::Waiting);
        let p = c.destroy(&m, A).unwrap();
        status(c.commit(&mut m, p).unwrap(), EBUSY);
        assert!(c.has_pending_overlap(A + 7, 1));
        let p = c.signal(&m, A, true).unwrap();
        c.commit(&mut m, p).unwrap();
        assert_eq!(c.phase(&t2, 2).unwrap(), Phase::Reacquiring(0));
        for (t, id) in [(&t1, 1), (&t2, 2)] {
            let p = c.finish(&m, t, id).unwrap();
            status(c.commit(&mut m, p).unwrap(), 0);
            assert!(c.finish(&m, t, id).is_err());
        }
        assert_eq!(c.pending_count(), 0);
        let p = c.destroy(&m, A).unwrap();
        status(c.commit(&mut m, p).unwrap(), 0);
        assert_eq!(m.load(A, 8).unwrap(), 1);
        let p = c.signal(&m, A, false).unwrap();
        status(c.commit(&mut m, p).unwrap(), EINVAL);
    }
    #[test]
    fn timeout_selection_is_sticky_during_reacquisition() {
        let mut b = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        let mut c = GuestCond::default();
        let d = Realtime {
            seconds: 3,
            nanoseconds: 1,
        };
        let t = wait(&mut c, &mut m, 1, Some(d));
        assert_eq!(
            c.delay_ns(
                &t,
                1,
                Realtime {
                    seconds: 3,
                    nanoseconds: 0
                }
            )
            .unwrap(),
            Some(1)
        );
        let p = c.select(&m, &t, 1, Some(d)).unwrap();
        assert!(matches!(
            c.commit(&mut m, p).unwrap(),
            Outcome::Reacquiring(ETIMEDOUT)
        ));
        let p = c.signal(&m, A, false).unwrap();
        c.commit(&mut m, p).unwrap();
        assert_eq!(c.phase(&t, 1).unwrap(), Phase::Reacquiring(ETIMEDOUT));
        assert_eq!(c.pending_count(), 1);
        assert!(!c.has_realtime_waits());
        let p = c.finish(&m, &t, 1).unwrap();
        status(c.commit(&mut m, p).unwrap(), ETIMEDOUT);
        let t = wait(&mut c, &mut m, 1, Some(d));
        let p = c.signal(&m, A, false).unwrap();
        c.commit(&mut m, p).unwrap();
        let p = c.select(&m, &t, 1, Some(d)).unwrap();
        assert!(matches!(
            c.commit(&mut m, p).unwrap(),
            Outcome::Reacquiring(0)
        ));
    }
    #[test]
    fn preflight_failure_foreign_stale_and_reused_slots() {
        let mut b = [0; 64];
        let mut ro = [0; 8];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        m.add(A + 128, &mut ro, false, false).unwrap();
        let mut c = GuestCond::default();
        let mut foreign = GuestCond::default();
        let p = c.signal(&m, A + 128, false).unwrap();
        assert!(c.commit(&mut m, p).is_err());
        assert_eq!(c.object_count(), 0);
        let p = c.init(&m, A, 0).unwrap();
        assert!(foreign.commit(&mut m, p).is_err());
        assert_eq!(m.load(A, 8).unwrap(), 0);
        let stale = c.init(&m, A, 0).unwrap();
        let p = c.init(&m, A, 0).unwrap();
        c.commit(&mut m, p).unwrap();
        assert!(c.commit(&mut m, stale).is_err());
        let t = wait(&mut c, &mut m, 1, None);
        assert!(foreign.phase(&t, 1).is_err());
        assert!(c.phase(&t, 2).is_err());
        let p = c.signal(&m, A, false).unwrap();
        m.store(A, 0, 8).unwrap();
        assert!(c.commit(&mut m, p).is_err());
        assert_eq!(c.phase(&t, 1).unwrap(), Phase::Waiting);
        let p = c.signal(&m, A, false).unwrap();
        status(c.commit(&mut m, p).unwrap(), EINVAL);
        assert_eq!(c.pending_count(), 1);
    }
    #[test]
    fn invalid_timespec_precedes_memory_and_preserves_existing_state() {
        let mut b = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        let mut c = GuestCond::default();
        let t = wait(&mut c, &mut m, 1, None);
        let p = c
            .wait(
                &m,
                u64::MAX,
                0,
                0,
                Some(Realtime {
                    seconds: 0,
                    nanoseconds: 1_000_000_000,
                }),
            )
            .unwrap();
        assert_eq!(p.object_range(), None);
        status(c.commit(&mut m, p).unwrap(), EINVAL);
        assert_eq!(c.phase(&t, 1).unwrap(), Phase::Waiting);
    }
    #[test]
    fn static_destroy_and_slot_alias_rejection() {
        let mut b = [0; 64];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        let mut c = GuestCond::default();
        let p = c.destroy(&m, A).unwrap();
        status(c.commit(&mut m, p).unwrap(), 0);
        assert_eq!(m.load(A, 8).unwrap(), 0);
        let t = wait(&mut c, &mut m, 1, None);
        let identity = m.load(A, 8).unwrap();
        m.store(A + 8, identity, 8).unwrap();
        let p = c.signal(&m, A + 8, false).unwrap();
        status(c.commit(&mut m, p).unwrap(), EINVAL);
        assert_eq!(c.phase(&t, 1).unwrap(), Phase::Waiting);
        assert!(c.wait(&m, A, 2, A + 40, None).is_err());
    }
    #[test]
    fn budget_and_abandoned_preparation_do_not_publish() {
        let mut b = vec![0; OBJECT_LIMIT * 8 + 64];
        let mut m = Memory::default();
        m.add(A, &mut b, true, false).unwrap();
        let mut c = GuestCond::default();
        let p = c.wait(&m, A, 1, A + 32, None).unwrap();
        assert!(matches!(p.outcome(), Outcome::Pending(_)));
        drop(p);
        assert_eq!(c.pending_count(), 0);
        assert_eq!(m.load(A, 8).unwrap(), 0);
        assert!(c.wait(&m, A, 1, A + 7, None).is_err());
        for n in 0..OBJECT_LIMIT {
            let p = c.init(&m, A + n as u64 * 8, 0).unwrap();
            c.commit(&mut m, p).unwrap();
        }
        let extra = A + OBJECT_LIMIT as u64 * 8;
        assert!(c.init(&m, extra, 0).is_err());
        assert_eq!(m.load(extra, 8).unwrap(), 0);
        for thread in 1..=WAIT_LIMIT as u64 {
            let _ = wait(&mut c, &mut m, thread, None);
        }
        assert!(c.wait(&m, A, WAIT_LIMIT as u64 + 1, A + 32, None).is_err());
        assert_eq!(c.pending_count(), WAIT_LIMIT);
    }
}
