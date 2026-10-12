//! Owned abort sequencing, not signal delivery or resource teardown.
//! Policy: https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdlib/abort.c
//! raise.c uses thr_self/thr_kill; stdio/findfp.c::_cleanup walks __sflush.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a sys/signal.h fixes SIGABRT=6.
//! Executor must validate all owned FILE metadata (including configured stdin),
//! reserve full CPU/FP/mask/stack/code continuations, and commit signal Store/TLS
//! changes together with these phase permits. No handler is invoked here.
use crate::{
    Result, guest_signal,
    guest_signal_action::{Disposition, Snapshot},
};
use std::rc::Rc;
pub const SIGABRT: i32 = 6;
pub const SA_RESTART: u32 = 2;
pub const SA_RESETHAND: u32 = 4;
pub const SA_NODEFER: u32 = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    CleanupPending,
    FirstRaise,
    HandlerRunning,
    ForceDefault,
    SecondRaise,
    Terminated,
}
#[derive(Debug)]
pub struct HandlerTicket {
    owner: Rc<()>,
    thread: u64,
}
impl HandlerTicket {
    pub fn thread(&self) -> u64 {
        self.thread
    }
}
pub enum Decision {
    Terminate {
        signal: i32,
    },
    ForceDefault,
    Handler {
        address: u64,
        mask: [u8; 16],
        restore_mask: [u8; 16],
        reset_before_entry: bool,
        ticket: HandlerTicket,
    },
}
pub struct Abort {
    owner: Rc<()>,
    thread: u64,
    phase: Phase,
}
pub struct RaisePermit<'a> {
    abort: &'a mut Abort,
    phase: Phase,
    decision: Decision,
}
impl RaisePermit<'_> {
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
    pub fn commit(self) -> Decision {
        self.abort.phase = self.phase;
        self.decision
    }
}
pub struct ReturnPermit<'a> {
    abort: &'a mut Abort,
}
impl ReturnPermit<'_> {
    pub fn commit(self) {
        self.abort.phase = Phase::ForceDefault;
    }
}
/// Abort's deliberate SETMASK, with libthr/kernel unmaskable bits filtered.
/// Returning from a handler restores THIS mask, not the pre-abort thread mask.
pub fn abort_mask() -> [u8; 16] {
    let mut mask = [255; 16];
    mask[0] &= !(1 << (SIGABRT - 1));
    guest_signal::transition([0; 16], 3, Some(mask)).expect("valid SETMASK")
}
pub fn forced_default() -> Disposition {
    Disposition {
        handler: 0,
        flags: 0,
        mask: [255; 16],
    }
}
impl Abort {
    pub fn new(thread: u64) -> Result<Self> {
        if thread == 0 {
            return Err("Invalid abort thread identity".into());
        }
        Ok(Self {
            owner: Rc::new(()),
            thread,
            phase: Phase::CleanupPending,
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// Caller confirms flush obligations before publishing this transition.
    pub fn cleanup_validated(&mut self) -> Result<()> {
        if self.phase != Phase::CleanupPending {
            return Err("Abort cleanup phase replay".into());
        }
        self.phase = Phase::FirstRaise;
        Ok(())
    }
    /// Executor must also validate the snapshot with its owning Store and hold
    /// that Store's permit until frame/mask/state publication completes.
    pub fn prepare_raise(&mut self, action: &Snapshot) -> Result<RaisePermit<'_>> {
        if action.signal() != SIGABRT
            || !matches!(self.phase, Phase::FirstRaise | Phase::SecondRaise)
        {
            return Err("Invalid abort raise phase or signal".into());
        }
        if self.phase == Phase::SecondRaise && action.handler() != 0 {
            return Err("Abort second raise requires committed default disposition".into());
        }
        let (phase, decision) = match action.handler() {
            0 => (Phase::Terminated, Decision::Terminate { signal: SIGABRT }),
            1 => (Phase::ForceDefault, Decision::ForceDefault),
            address => {
                if action.flags() & !(SA_RESTART | SA_RESETHAND | SA_NODEFER) != 0 {
                    return Err("Abort handler flags require unimplemented signal-frame/alternate-stack ABI".into());
                }
                let restore_mask = abort_mask();
                let mut added = action.mask();
                if action.flags() & SA_NODEFER == 0 {
                    added[0] |= 1 << (SIGABRT - 1);
                }
                let mask =
                    guest_signal::transition(restore_mask, 1, Some(added)).expect("valid BLOCK");
                (
                    Phase::HandlerRunning,
                    Decision::Handler {
                        address,
                        mask,
                        restore_mask,
                        reset_before_entry: action.flags() & SA_RESETHAND != 0,
                        ticket: HandlerTicket {
                            owner: self.owner.clone(),
                            thread: self.thread,
                        },
                    },
                )
            }
        };
        Ok(RaisePermit {
            abort: self,
            phase,
            decision,
        })
    }
    /// Authenticate the callback's frame separately before using this permit.
    /// This ticket authenticates the model/thread, not guest stack bytes.
    pub fn prepare_handler_return(
        &mut self,
        ticket: &HandlerTicket,
        thread: u64,
    ) -> Result<ReturnPermit<'_>> {
        if self.phase != Phase::HandlerRunning
            || !Rc::ptr_eq(&self.owner, &ticket.owner)
            || self.thread != thread
            || ticket.thread != thread
        {
            return Err("Foreign, stale or wrong-thread abort handler return".into());
        }
        Ok(ReturnPermit { abort: self })
    }
    /// Caller commits forced_default() to Store and the abort mask to TLS before
    /// the second raise. Its default snapshot is checked again by prepare_raise.
    pub fn default_installed(&mut self) -> Result<()> {
        if self.phase != Phase::ForceDefault {
            return Err("Abort forced-default phase replay".into());
        }
        self.phase = Phase::SecondRaise;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_signal_action::Store;
    fn disposition(store: &mut Store, handler: u64, flags: u32, mask: [u8; 16]) {
        let old = store.snapshot(SIGABRT).unwrap();
        store
            .prepare_snapshot(
                &old,
                Some(Disposition {
                    handler,
                    flags,
                    mask,
                }),
            )
            .unwrap()
            .commit();
    }
    #[test]
    fn default_ignore_and_mask_filtering() {
        let mut s = Store::default();
        let mut a = Abort::new(1).unwrap();
        assert!(a.prepare_raise(&s.snapshot(6).unwrap()).is_err());
        a.cleanup_validated().unwrap();
        let permit = a.prepare_raise(&s.snapshot(6).unwrap()).unwrap();
        assert!(matches!(
            permit.decision(),
            Decision::Terminate { signal: 6 }
        ));
        drop(permit);
        assert_eq!(a.phase(), Phase::FirstRaise);
        assert!(matches!(
            a.prepare_raise(&s.snapshot(6).unwrap()).unwrap().commit(),
            Decision::Terminate { signal: 6 }
        ));
        assert!(a.prepare_raise(&s.snapshot(6).unwrap()).is_err());
        disposition(&mut s, 1, 0, [0; 16]);
        let mut a = Abort::new(2).unwrap();
        a.cleanup_validated().unwrap();
        assert!(matches!(
            a.prepare_raise(&s.snapshot(6).unwrap()).unwrap().commit(),
            Decision::ForceDefault
        ));
        a.default_installed().unwrap();
        assert!(a.prepare_raise(&s.snapshot(6).unwrap()).is_err());
        let snap = s.snapshot(6).unwrap();
        s.prepare_snapshot(&snap, Some(forced_default()))
            .unwrap()
            .commit();
        assert!(matches!(
            a.prepare_raise(&s.snapshot(6).unwrap()).unwrap().commit(),
            Decision::Terminate { signal: 6 }
        ));
        let m = abort_mask();
        for sig in 1..=128 {
            let b = sig - 1;
            assert_eq!(
                m[b / 8] & (1 << (b % 8)) != 0,
                !matches!(sig, 6 | 9 | 17 | 32)
            );
        }
    }
    #[test]
    fn real_handler_decision_return_replay_and_reset_policy() {
        let mut s = Store::default();
        disposition(&mut s, 0x1000, SA_RESETHAND, [0; 16]);
        let mut a = Abort::new(3).unwrap();
        a.cleanup_validated().unwrap();
        let Decision::Handler {
            address,
            mask,
            restore_mask,
            reset_before_entry,
            ticket,
        } = a.prepare_raise(&s.snapshot(6).unwrap()).unwrap().commit()
        else {
            panic!("handler")
        };
        assert_eq!(address, 0x1000);
        assert!(reset_before_entry);
        let snapshot = s.snapshot(6).unwrap();
        s.prepare_snapshot(&snapshot, Some(snapshot.reset_for_delivery()))
            .unwrap()
            .commit();
        let reset = s.snapshot(6).unwrap();
        assert_eq!(reset.handler(), 0);
        assert_eq!(reset.flags(), SA_RESETHAND);
        assert_ne!(reset.mask()[3] & 128, 0); // kernel catchmask retains reserved32
        assert_eq!(reset.mask()[1] & 1, 0);
        assert_eq!(reset.mask()[2] & 1, 0);
        disposition(&mut s, 0x1000, SA_RESETHAND, [0; 16]);

        assert_ne!(mask[0] & 32, 0);
        assert_eq!(restore_mask[0] & 32, 0);
        let mut foreign = Abort::new(3).unwrap();
        foreign.cleanup_validated().unwrap();
        foreign
            .prepare_raise(&s.snapshot(6).unwrap())
            .unwrap()
            .commit();
        assert!(foreign.prepare_handler_return(&ticket, 3).is_err());
        assert!(a.prepare_handler_return(&ticket, 4).is_err());
        {
            let _permit = a.prepare_handler_return(&ticket, 3).unwrap();
        }
        assert_eq!(a.phase(), Phase::HandlerRunning);
        a.prepare_handler_return(&ticket, 3).unwrap().commit();
        assert!(a.prepare_handler_return(&ticket, 3).is_err());
        disposition(&mut s, 0x2000, SA_NODEFER, [0; 16]);
        let mut b = Abort::new(4).unwrap();
        b.cleanup_validated().unwrap();
        let Decision::Handler { mask, .. } =
            b.prepare_raise(&s.snapshot(6).unwrap()).unwrap().commit()
        else {
            panic!("handler")
        };
        assert_eq!(mask[0] & 32, 0);
    }
    #[test]
    fn unsupported_delivery_flags_and_snapshot_transactions() {
        let mut s = Store::default();
        let mut other = Store::default();
        let snap = s.snapshot(6).unwrap();
        assert!(other.prepare_snapshot(&snap, None).is_err());
        disposition(&mut s, 0x1000, 0x40, [0; 16]);
        assert!(s.prepare_snapshot(&snap, None).is_err());
        let mut a = Abort::new(1).unwrap();
        a.cleanup_validated().unwrap();
        assert!(a.prepare_raise(&s.snapshot(6).unwrap()).is_err());
        assert_eq!(a.phase(), Phase::FirstRaise);
        let snapshot = s.snapshot(6).unwrap();
        {
            let _permit = s
                .prepare_snapshot(&snapshot, Some(forced_default()))
                .unwrap();
        }
        assert_eq!(s.snapshot(6).unwrap().handler(), 0x1000);
        disposition(&mut s, 0x1000, 1, [0; 16]);
        assert!(a.prepare_raise(&s.snapshot(6).unwrap()).is_err());
    }
}
