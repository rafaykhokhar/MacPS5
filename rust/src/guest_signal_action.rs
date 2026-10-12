//! Process-owned signal dispositions under an explicit FreeBSD libthr-visible
//! contract, not a measurement of console firmware behavior.
//!
//! Sources: SDK10b539de include/freebsd/sys/{signal,_sigset}.h;
//! FreeBSD releng/11.4 lib/libthr/thread/thr_sig.c (__thr_sigaction),
//! lib/libc/gen/signal.c, sys/kern/kern_sig.c (kern_sigaction, sigacts_alloc).
//! Unlike raw sys_sigaction, libthr rejects invalid/reserved signals before
//! touching act and returns its cached user action for caught handlers. That
//! cache removes SIGCANCEL32 but retains KILL/STOP mask bits and user padding.
//! Default/ignore queries instead expose the canonical kernel representation.
//!
//! No signal sources or delivery are implemented here. Future delivery needs
//! owned pending events, CPU/mask/stack/module continuations, real guest frames,
//! and explicit blocked-call interruption/restart policy. Merely registering a
//! handler does not invoke it or validate its executable mapping. No host signal
//! registration, callback invocation, or fabricated sigreturn frame is used.
use crate::{Result, guest_memory::Memory};
use std::rc::Rc;
pub const ACTION_SIZE: usize = 32;
pub const EINVAL: u32 = 22;
const RESTART: u32 = 2;
const KNOWN_FLAGS: u32 = 0x7f;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Action([u8; ACTION_SIZE]);
impl Action {
    fn initial() -> Self {
        let mut bytes = [0; ACTION_SIZE];
        bytes[8..12].copy_from_slice(&RESTART.to_le_bytes());
        Self(bytes)
    }
    fn handler(self) -> u64 {
        u64::from_le_bytes(self.0[..8].try_into().expect("handler"))
    }
    fn flags(self) -> u32 {
        u32::from_le_bytes(self.0[8..12].try_into().expect("flags"))
    }
    fn remove_mask(&mut self, signal: usize) {
        let bit = signal - 1;
        self.0[12 + bit / 8] &= !(1 << (bit % 8));
    }
    fn normalize(mut self, signal: i32) -> std::result::Result<Self, u32> {
        if matches!(signal, 9 | 17) && self.handler() != 0 {
            return Err(EINVAL);
        }
        if self.handler() > 1 {
            if self.flags() & !KNOWN_FLAGS != 0 {
                return Err(EINVAL);
            }
            self.remove_mask(32);
        } else {
            self.remove_mask(9);
            self.remove_mask(17);
            let mut flags = self.flags() & (KNOWN_FLAGS & !0x40);
            if signal != 20 {
                flags &= !(0x08 | 0x20);
            }
            self.0[8..12].copy_from_slice(&flags.to_le_bytes());
            self.0[28..32].fill(0);
        }
        Ok(self)
    }
}

/// Process/revision-bound disposition snapshot for synchronous delivery setup.
#[derive(Clone)]
pub struct Snapshot {
    owner: Rc<()>,
    revision: u64,
    signal: i32,
    action: Action,
}
impl Snapshot {
    pub fn signal(&self) -> i32 {
        self.signal
    }
    pub fn handler(&self) -> u64 {
        self.action.handler()
    }
    pub fn flags(&self) -> u32 {
        self.action.flags()
    }
    /// libthr installs caught handlers through a SIGINFO trampoline with
    /// NODEFER cleared and an all-signals kernel mask. kern_sig.c::sigdflt
    /// changes handler to DFL and clears SIGINFO, retaining other kernel flags.
    /// This differs from abort's later explicit flags-zero sigaction.
    pub fn reset_for_delivery(&self) -> Disposition {
        Disposition {
            handler: 0,
            flags: self.flags() & !(0x10 | 0x40),
            mask: [255; 16],
        }
    }
    pub fn mask(&self) -> [u8; 16] {
        self.action.0[12..28].try_into().expect("signal mask")
    }
}
#[derive(Clone, Copy)]
pub struct Disposition {
    pub handler: u64,
    pub flags: u32,
    pub mask: [u8; 16],
}
/// All checks precede this exclusive permit. Dropping it changes nothing;
/// commit cannot allocate or fail, for atomic publication with owned delivery.
pub struct SnapshotCommit<'a> {
    store: &'a mut Store,
    replacement: Option<(usize, Action)>,
    next: u64,
}
impl SnapshotCommit<'_> {
    pub fn commit(self) {
        if let Some((index, action)) = self.replacement {
            self.store.actions[index] = action;
        }
        self.store.revision = self.next;
    }
}
pub struct Store {
    owner: Rc<()>,
    actions: [Action; 128],
    revision: u64,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            owner: Rc::new(()),
            actions: [Action::initial(); 128],
            revision: 0,
        }
    }
}
/// A one-shot checked transaction. The executor must additionally protect the
/// captured return frame and preguard errno before committing this plan.
/// Guard failures leave disposition state unchanged, intentionally stronger than
/// a real kernel installing state before a failing user old-action copyout.
pub struct Plan {
    owner: Rc<()>,
    revision: u64,
    replacement: Option<(usize, Action)>,
    output: Option<(u64, [u8; ACTION_SIZE])>,
    errno: Option<u32>,
    value: u64,
}
impl Plan {
    pub fn output_range(&self) -> Option<(u64, usize)> {
        self.output
            .as_ref()
            .map(|(address, _)| (*address, ACTION_SIZE))
    }
    pub fn errno(&self) -> Option<u32> {
        self.errno
    }
    pub fn return_value(&self) -> u64 {
        self.value
    }
}
impl Store {
    pub fn snapshot(&self, signal: i32) -> Result<Snapshot> {
        if !(1..=128).contains(&signal) || signal == 32 {
            return Err("Invalid signal disposition snapshot".into());
        }
        Ok(Snapshot {
            owner: self.owner.clone(),
            revision: self.revision,
            signal,
            action: self.actions[(signal - 1) as usize],
        })
    }
    pub fn prepare_snapshot<'a>(
        &'a mut self,
        snapshot: &Snapshot,
        replacement: Option<Disposition>,
    ) -> Result<SnapshotCommit<'a>> {
        if !Rc::ptr_eq(&self.owner, &snapshot.owner) || self.revision != snapshot.revision {
            return Err("Foreign or stale signal disposition snapshot".into());
        }
        let replacement = if let Some(d) = replacement {
            let mut bytes = [0u8; ACTION_SIZE];
            bytes[..8].copy_from_slice(&d.handler.to_le_bytes());
            bytes[8..12].copy_from_slice(&d.flags.to_le_bytes());
            bytes[12..28].copy_from_slice(&d.mask);
            Some((
                (snapshot.signal - 1) as usize,
                Action(bytes)
                    .normalize(snapshot.signal)
                    .map_err(|e| format!("Invalid replacement signal disposition: {e}"))?,
            ))
        } else {
            None
        };
        let next = if replacement.is_some() {
            self.revision
                .checked_add(1)
                .ok_or("Signal disposition revision exhausted")?
        } else {
            self.revision
        };
        Ok(SnapshotCommit {
            store: self,
            replacement,
            next,
        })
    }
    fn base(&self) -> Plan {
        Plan {
            owner: self.owner.clone(),
            revision: self.revision,
            replacement: None,
            output: None,
            errno: None,
            value: 0,
        }
    }
    fn prepare(&self, signal: i32, input: Option<Action>, old: u64, signal_wrapper: bool) -> Plan {
        let mut plan = self.base();
        let failure = |mut plan: Plan, error| {
            plan.errno = Some(error);
            plan.value = if signal_wrapper {
                u64::MAX
            } else {
                u32::MAX as u64
            };
            plan
        };
        if !(1..=128).contains(&signal) || signal == 32 {
            return failure(plan, EINVAL);
        }
        let index = (signal - 1) as usize;
        if let Some(input) = input {
            match input.normalize(signal) {
                Ok(action) => plan.replacement = Some((index, action)),
                Err(error) => return failure(plan, error),
            }
        }
        if old != 0 {
            plan.output = Some((old, self.actions[index].0));
        }
        if signal_wrapper {
            plan.value = self.actions[index].handler();
        }
        plan
    }
    pub fn plan(&self, memory: &Memory<'_>, signal: i32, act: u64, old: u64) -> Result<Plan> {
        if !(1..=128).contains(&signal) || signal == 32 {
            return Ok(self.prepare(signal, None, 0, false));
        }
        let input = if act == 0 {
            None
        } else {
            Some(Action(
                memory
                    .read(act, ACTION_SIZE)?
                    .try_into()
                    .expect("action size"),
            ))
        };
        Ok(self.prepare(signal, input, old, false))
    }
    /// signal() uses the initially empty libc _sigintr set. siginterrupt is not
    /// exposed, so this wrapper always requests SA_RESTART regardless of a
    /// previous sigaction's flags, and returns the old handler at full width.
    pub fn plan_signal(&self, signal: i32, handler: u64) -> Plan {
        let mut action = Action::initial();
        action.0[..8].copy_from_slice(&handler.to_le_bytes());
        self.prepare(signal, Some(action), 0, true)
    }
    pub fn commit(&mut self, memory: &mut Memory<'_>, plan: Plan) -> Result<u64> {
        if !Rc::ptr_eq(&plan.owner, &self.owner) {
            return Err("Foreign process signal action transaction".into());
        }
        if plan.revision != self.revision {
            return Err("Stale signal action transaction".into());
        }
        let next = self
            .revision
            .checked_add(1)
            .ok_or("Signal action transaction identity exhausted")?;
        if let Some((address, bytes)) = &plan.output {
            memory.validate_write(*address, bytes.len())?;
        }
        if let Some((address, bytes)) = &plan.output {
            memory.write(*address, bytes)?;
        }
        if let Some((index, action)) = plan.replacement {
            self.actions[index] = action;
        }
        self.revision = next;
        Ok(plan.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn action(handler: u64, flags: u32) -> [u8; 32] {
        let mut bytes = [0xff; 32];
        bytes[..8].copy_from_slice(&handler.to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes
    }
    #[test]
    fn caught_cache_and_default_canonicalization() {
        let mut bytes = [0u8; 128];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut store = Store::default();
        let query = store.plan(&memory, 2, 0, 0x1040).unwrap();
        store.commit(&mut memory, query).unwrap();
        assert_eq!(memory.load(0x1040, 8).unwrap(), 0);
        assert_eq!(memory.load(0x1048, 4).unwrap(), 2);
        let raw = action(0x1234_5678_9abc_def0, 0x7f);
        memory.write(0x1000, &raw).unwrap();
        let plan = store.plan(&memory, 2, 0x1000, 0x1000).unwrap();
        store.commit(&mut memory, plan).unwrap();
        assert_eq!(memory.load(0x1000, 8).unwrap(), 0); // input snapshotted before alias output
        let query = store.plan(&memory, 2, 0, 0x1040).unwrap();
        store.commit(&mut memory, query).unwrap();
        let mut expected = raw;
        expected[15] &= 0x7f;
        assert_eq!(memory.read(0x1040, 32).unwrap(), expected);
        memory.write(0x1000, &action(1, u32::MAX)).unwrap();
        let plan = store.plan(&memory, 2, 0x1000, 0).unwrap();
        store.commit(&mut memory, plan).unwrap();
        let query = store.plan(&memory, 2, 0, 0x1040).unwrap();
        store.commit(&mut memory, query).unwrap();
        assert_eq!(memory.load(0x1048, 4).unwrap(), 0x17); // ONSTACK RESTART RESETHAND NODEFER
        assert_eq!(memory.read(0x105c, 4).unwrap(), [0; 4]);
        assert_eq!(memory.load(0x104d, 1).unwrap() & 1, 0); // KILL9
        assert_eq!(memory.load(0x104e, 1).unwrap() & 1, 0); // STOP17
        assert_eq!(memory.load(0x104f, 1).unwrap() & 128, 128); // default/ignore retains32
    }
    #[test]
    fn invalid_ordering_rollback_and_wrapper_width() {
        let mut bytes = [0u8; 64];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut store = Store::default();
        for sig in [0, 32, 129, i32::MIN] {
            assert_eq!(
                store
                    .plan(&memory, sig, u64::MAX, u64::MAX)
                    .unwrap()
                    .errno(),
                Some(22)
            );
        }
        memory.write(0x1000, &action(0x1234, 0x80)).unwrap();
        assert_eq!(
            store.plan(&memory, 2, 0x1000, u64::MAX).unwrap().errno(),
            Some(22)
        );
        for sig in [9, 17] {
            assert_eq!(store.plan_signal(sig, 1).errno(), Some(22));
        }
        memory.write(0x1000, &action(0x1234, 0)).unwrap();
        let plan = store.plan(&memory, 2, 0x1000, 0x1030).unwrap();
        assert!(store.commit(&mut memory, plan).is_err());
        let p = store.plan_signal(2, u64::MAX);
        assert_eq!(p.errno(), None);
        assert_eq!(store.commit(&mut memory, p).unwrap(), 0);
        let p = store.plan_signal(2, 0x100_0000_0000);
        assert_eq!(store.commit(&mut memory, p).unwrap(), u64::MAX);
        let p = store.plan(&memory, 2, 0, 0x1000).unwrap();
        store.commit(&mut memory, p).unwrap();
        assert_eq!(memory.load(0x1008, 4).unwrap(), 2);
        assert_eq!(memory.read(0x100c, 16).unwrap(), [0; 16]);
        let foreign = Store::default().plan_signal(2, 0);
        assert!(store.commit(&mut memory, foreign).is_err());
        let first = store.plan_signal(15, 1);
        let stale = store.plan_signal(15, 0);
        store.commit(&mut memory, first).unwrap();
        assert!(store.commit(&mut memory, stale).is_err());
    }
}
