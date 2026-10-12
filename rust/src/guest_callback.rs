//! Internal owned state for nested, resumable comparator calls.
use crate::{
    Result,
    guest_compare::{Event, Request},
    guest_search::Search,
    guest_sort::Sort,
};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};
pub(crate) enum Engine {
    Sort(Sort),
    Search(Search),
}
impl Engine {
    pub fn next(&mut self) -> Result<Event> {
        match self {
            Self::Sort(v) => v.next(),
            Self::Search(v) => v.next(),
        }
    }
    pub fn compare(&mut self, request: Request, value: i32) -> Result<()> {
        match self {
            Self::Sort(v) => v.compare(request, value),
            Self::Search(v) => v.compare(request, value),
        }
    }
    pub fn validate_swap(&self, request: &Request) -> Result<()> {
        match self {
            Self::Sort(v) => v.validate_swap(request),
            Self::Search(_) => Err("Search cannot swap".into()),
        }
    }
    pub fn swapped(&mut self, request: Request) -> Result<()> {
        match self {
            Self::Sort(v) => v.swapped(request),
            Self::Search(_) => Err("Search cannot swap".into()),
        }
    }
}
struct Lease {
    owner: Weak<()>,
    ranges: Vec<(u64, usize)>,
    frames: [u64; 2],
}
#[derive(Clone, Default)]
pub(crate) struct Registry(Rc<RefCell<Vec<Lease>>>);
impl Registry {
    pub fn acquire(&self, ranges: Vec<(u64, usize)>, frames: [u64; 2]) -> Result<Rc<()>> {
        let mut entries = self.0.borrow_mut();
        entries.retain(|entry| entry.owner.strong_count() != 0);
        if entries.len() >= 4096 {
            return Err("Comparator backing lease limit".into());
        }
        entries
            .try_reserve(1)
            .map_err(|_| "Cannot reserve comparator backing lease")?;
        let owner = Rc::new(());
        entries.push(Lease {
            owner: Rc::downgrade(&owner),
            ranges,
            frames,
        });
        Ok(owner)
    }
    pub fn overlaps(&self, address: u64, length: usize) -> bool {
        if length == 0 {
            return false;
        }
        let end = address.saturating_add(length as u64);
        self.0
            .borrow()
            .iter()
            .filter(|e| e.owner.strong_count() != 0)
            .any(|e| {
                e.ranges
                    .iter()
                    .any(|&(base, len)| address < base.saturating_add(len as u64) && base < end)
            })
    }
    pub fn barriers(&self) -> Vec<(u64, usize)> {
        self.0
            .borrow()
            .iter()
            .filter(|e| e.owner.strong_count() != 0)
            .flat_map(|e| e.frames.map(|address| (address, 8)))
            .collect()
    }
}
pub(crate) struct Frame {
    pub engine: Engine,
    pub pending: Option<Event>,
    pub waiting: bool,
    pub comparator: u64,
    pub registers: [u64; 16],
    pub return_sp: u64,
    pub return_address: u64,
    pub return_bias: u64,
    pub return_offset: u64,
    pub terminal: bool,
    pub callback_sp: u64,
    pub _lease: Rc<()>,
}
/// An ordinary synchronous abort handler. Full CPU state is owned, not rebuilt
/// from ABI callee-saved registers; restoring it must keep live remaining fuel.
pub(crate) struct AbortFrame {
    pub model: crate::guest_abort::Abort,
    pub ticket: crate::guest_abort::HandlerTicket,
    pub cpu: crate::legacy_cpu::Cpu,
    pub restore_mask: [u8; 16],
    pub thread: u64,
    pub comparator_depth: usize,
    pub callback_sp: u64,
    pub _lease: Rc<()>,
}
impl AbortFrame {
    pub fn restore_cpu(&self, cpu: &mut crate::legacy_cpu::Cpu) {
        let live_fuel = cpu.fuel;
        *cpu = self.cpu.clone();
        cpu.fuel = live_fuel;
        cpu.memory = 0;
    }
}
pub(crate) type AbortFaults = Rc<RefCell<Vec<AbortFrame>>>;
pub(crate) type RetainedStacks = Rc<RefCell<Vec<(u64, Vec<u8>, Rc<()>)>>>;
pub(crate) type Faults = Rc<RefCell<Vec<Frame>>>;
pub(crate) struct Context {
    pub frames: Vec<Frame>,
    pub abort_frames: Vec<AbortFrame>,
    abort_faults: AbortFaults,
    abort_stack_faults: RetainedStacks,
    pub registry: Registry,
    faults: Faults,
}
impl Context {
    pub fn new(registry: Registry, faults: Faults) -> Self {
        Self {
            frames: Vec::new(),
            abort_frames: Vec::new(),
            abort_faults: Rc::new(RefCell::new(Vec::new())),
            abort_stack_faults: Rc::new(RefCell::new(Vec::new())),
            registry,
            faults,
        }
    }
    pub fn with_abort_faults(mut self, faults: AbortFaults, stacks: RetainedStacks) -> Self {
        self.abort_faults = faults;
        self.abort_stack_faults = stacks;
        self
    }
    pub fn retain_abort_stack(&self, address: u64, bytes: Vec<u8>, lease: Rc<()>) {
        self.abort_stack_faults
            .borrow_mut()
            .push((address, bytes, lease));
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty() && self.abort_frames.is_empty()
    }
    pub fn authentic_abort_return(&self, sp: u64, target: u64) -> bool {
        target == u64::MAX
            && self.abort_frames.last().is_some_and(|frame| {
                frame.comparator_depth == self.frames.len() && frame.callback_sp == sp
            })
    }
    pub fn authentic_return(&self, sp: u64, target: u64) -> bool {
        if self
            .abort_frames
            .last()
            .is_some_and(|frame| frame.comparator_depth == self.frames.len())
        {
            return self.authentic_abort_return(sp, target);
        }
        target == u64::MAX
            && self
                .frames
                .last()
                .is_some_and(|frame| frame.waiting && frame.callback_sp == sp)
    }
    pub fn accept_return(&mut self, value: u64) -> Result<()> {
        let frame = self
            .frames
            .last_mut()
            .ok_or("No active comparator return")?;
        if !frame.waiting {
            return Err("Comparator return replay".into());
        }
        let Some(Event::Compare { request, .. }) = frame.pending.take() else {
            return Err("Missing comparator request".into());
        };
        frame.engine.compare(request, value as u32 as i32)?;
        frame.waiting = false;
        Ok(())
    }
}
impl Drop for Context {
    fn drop(&mut self) {
        if !self.abort_frames.is_empty() {
            self.abort_faults
                .borrow_mut()
                .append(&mut self.abort_frames);
        }
        if !self.frames.is_empty() {
            self.faults.borrow_mut().append(&mut self.frames);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(registry: &Registry, sp: u64) -> Frame {
        let mut engine = Engine::Search(Search::new(0x4000, 0x3000, 1, 8).unwrap());
        let pending = Some(engine.next().unwrap());
        Frame {
            engine,
            pending,
            waiting: true,
            comparator: 0x1000,
            registers: [0; 16],
            return_sp: sp + 256,
            return_address: 0x2000,
            return_bias: 0,
            return_offset: 0x2000,
            terminal: false,
            callback_sp: sp,
            _lease: registry
                .acquire(vec![(0x3000, 16)], [sp + 256, sp])
                .unwrap(),
        }
    }
    #[test]
    fn nested_return_authentication_and_signed_once_only_completion() {
        let registry = Registry::default();
        let faults = Rc::new(RefCell::new(Vec::new()));
        let mut context = Context::new(registry.clone(), faults.clone());
        context.frames.push(frame(&registry, 0x8000));
        context.frames.push(frame(&registry, 0x7000));
        assert!(!context.authentic_return(0x8000, u64::MAX));
        assert!(!context.authentic_return(0x7008, u64::MAX));
        assert!(!context.authentic_return(0x7000, 0x2000));
        assert!(context.authentic_return(0x7000, u64::MAX));
        context.accept_return(0x1234_5678_ffff_ffff).unwrap();
        assert!(!context.authentic_return(0x7000, u64::MAX));
        assert!(context.accept_return(0).is_err());
        assert!(matches!(
            context.frames.last_mut().unwrap().engine.next().unwrap(),
            Event::Done(0)
        ));
        context.frames.pop();
        assert!(context.authentic_return(0x8000, u64::MAX));
        context.accept_return(0x1234_5678_0000_0000).unwrap();
        assert!(matches!(
            context.frames.last_mut().unwrap().engine.next().unwrap(),
            Event::Done(0x3000)
        ));
        context.frames.pop();
        drop(context);
        assert!(faults.borrow().is_empty());
        assert!(!registry.overlaps(0x3000, 1));
        assert!(registry.barriers().is_empty());
    }
    #[test]
    fn dropped_contexts_accumulate_fault_owned_leases_until_teardown() {
        let registry = Registry::default();
        let faults = Rc::new(RefCell::new(Vec::new()));
        for sp in [0x8000, 0x7000] {
            let mut context = Context::new(registry.clone(), faults.clone());
            context.frames.push(frame(&registry, sp));
            drop(context);
        }
        assert_eq!(faults.borrow().len(), 2);
        assert!(registry.overlaps(0x3000, 1));
        assert!(!registry.overlaps(0x3000, 0));
        assert!(!registry.overlaps(0x3010, 1));
        assert_eq!(registry.barriers().len(), 4);
        faults.borrow_mut().clear();
        assert!(!registry.overlaps(0x3000, 1));
        assert!(registry.barriers().is_empty());
    }
}
