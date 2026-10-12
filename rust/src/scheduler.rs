//! Bounded cooperative execution of explicitly enqueued guest workers.
//! Guest create/join/detach requests use retained continuations; callers keep
//! images and externally supplied parent data alive throughout scheduling.
use crate::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, ExitRequest, NativeExecutor, ThreadOperation},
};
type Result<T> = std::result::Result<T, String>;
use std::collections::BTreeSet;
use std::rc::Rc;
#[derive(Clone)]
pub(crate) struct ImageLease {
    pub bias: u64,
    pub handles: BTreeSet<u64>,
}
const WORKER_LIMIT: usize = 63;
enum State {
    Ready { entry: u64, argument: u64 },
    Suspended(Execution),
    Completed(u64),
    Reaped,
    ExternalBoundary(Execution),
    Dispatching,
}
struct Worker {
    lease: Option<ImageLease>,
    external: bool,
    detached: bool,
    joiner: Option<u64>,
    thread: u64,
    state: State,
}
/// One scheduling action. Service boundaries remain retained for future runtime
/// orchestration; they are never replaced with a fabricated success result.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    ExitRequested(ExitRequest),
    Yielded(u64),
    MutexPending(u64),
    JoinPending(u64),
    ThreadService(u64),
    Completed { thread: u64, value: u64 },
    ServiceBoundary(u64),
    Finished,
}
pub struct Scheduler {
    exit_request: Option<ExitRequest>,
    next_thread: u64,
    failure: Option<String>,
    owner: Option<Rc<()>>,
    workers: Vec<Worker>,
    cursor: usize,
    quantum: u64,
    remaining_dispatches: u64,
    remaining_instructions: u64,
}
impl Scheduler {
    pub(crate) fn live_image_pins(&self) -> BTreeSet<u64> {
        self.workers
            .iter()
            .filter(|w| !matches!(w.state, State::Completed(_) | State::Reaped))
            .filter_map(|w| w.lease.as_ref())
            .flat_map(|lease| lease.handles.iter().copied())
            .collect()
    }
    pub(crate) fn retained_stacks(&mut self) -> Result<Vec<DataRegion<'_>>> {
        let mut regions = Vec::new();
        for worker in &mut self.workers {
            if let State::Suspended(execution) | State::ExternalBoundary(execution) =
                &mut worker.state
            {
                if let Some(region) = execution.stack_region()? {
                    regions.push(region);
                }
            }
        }
        Ok(regions)
    }
    pub(crate) fn exit_request(&self) -> Option<ExitRequest> {
        self.exit_request
    }
    pub(crate) fn adopt_parent(
        &mut self,
        native: &NativeExecutor,
        request: &mut Option<Box<crate::module_executor::ThreadRequest>>,
        lease: ImageLease,
    ) -> Result<u64> {
        self.require_owner(native)?;
        let pending = request.as_ref().ok_or("Missing parent thread request")?;
        let thread = pending.guest_thread_id().ok_or("Consumed parent request")?;
        if !pending.belongs_to(native)
            || thread != native.guest_thread_id()
            || !native.tls.contains_thread(thread)
            || self
                .workers
                .iter()
                .any(|w| w.thread == thread && !matches!(w.state, State::Reaped))
        {
            return Err("Invalid parent thread identity".into());
        }
        self.workers
            .try_reserve(1)
            .map_err(|_| "Cannot reserve parent record")?;
        self.next_thread = self
            .next_thread
            .max(thread.checked_add(1).ok_or("Thread identity overflow")?);
        request.as_mut().unwrap().set_quantum(self.quantum)?;
        self.insert_worker(Worker {
            lease: Some(lease),
            external: true,
            thread,
            detached: false,
            joiner: None,
            state: State::Suspended(Execution::ThreadRequest(request.take().unwrap())),
        });
        Ok(thread)
    }
    /// Hand the parent back while independent worker leases remain owned here.
    pub(crate) fn take_parent_boundary(&mut self, thread: u64) -> Option<Execution> {
        let worker = self.workers.iter_mut().find(|w| {
            w.thread == thread && w.external && matches!(w.state, State::ExternalBoundary(_))
        })?;
        if !matches!(worker.state, State::ExternalBoundary(_)) {
            return None;
        }
        let State::ExternalBoundary(outcome) = std::mem::replace(&mut worker.state, State::Reaped)
        else {
            unreachable!()
        };
        Some(outcome)
    }
    fn insert_worker(&mut self, worker: Worker) {
        if let Some(slot) = self
            .workers
            .iter_mut()
            .find(|w| matches!(w.state, State::Reaped))
        {
            *slot = worker;
        } else {
            self.workers.push(worker);
        }
    }
    fn service_thread(
        &mut self,
        index: usize,
        native: &mut NativeExecutor,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        parent_data: &mut [DataRegion<'_>],
    ) -> Result<Step> {
        let parent = self.workers[index].thread;
        let lease = self.workers[index].lease.clone();
        self.workers
            .try_reserve(1)
            .map_err(|_| "Cannot reserve thread record")?;
        let capacity = (self.workers.len() < 4096
            || self
                .workers
                .iter()
                .any(|w| matches!(w.state, State::Reaped)))
            && self
                .workers
                .iter()
                .filter(|w| !matches!(w.state, State::Completed(_) | State::Reaped))
                .count()
                < WORKER_LIMIT;
        let State::Suspended(Execution::ThreadRequest(mut request)) =
            std::mem::replace(&mut self.workers[index].state, State::Dispatching)
        else {
            return Err("Missing thread request".into());
        };
        let operation = request.operation();
        let target_id = match operation {
            ThreadOperation::Join { thread, .. } | ThreadOperation::Detach { thread } => {
                Some(thread)
            }
            _ => None,
        };
        let target = target_id.and_then(|id| {
            self.workers
                .iter()
                .enumerate()
                .find(|(_, w)| w.thread == id)
                .map(|(i, w)| {
                    (
                        i,
                        w.detached,
                        w.joiner,
                        match w.state {
                            State::Completed(v) => Some(v),
                            _ => None,
                        },
                        matches!(w.state, State::Reaped),
                    )
                })
        });
        let mut mappings = Vec::new();
        for region in parent_data.iter_mut() {
            mappings.push(DataRegion {
                address: region.address,
                bytes: &mut *region.bytes,
                writable: region.writable,
            });
        }
        for (other, worker) in self.workers.iter_mut().enumerate() {
            if other != index {
                if let State::Suspended(execution) | State::ExternalBoundary(execution) =
                    &mut worker.state
                {
                    if let Some(region) = execution
                        .stack_region()
                        .expect("Prevalidated suspended stack")
                    {
                        mappings.push(region);
                    }
                }
            }
        }
        let mut child = None;
        let mut reap = None;
        let mut detach = None;
        let mut joiner = None;
        let result = (|| -> Result<Option<u64>> {
            match operation {
                ThreadOperation::Create {
                    output,
                    entry,
                    argument,
                    options,
                } => {
                    if !capacity {
                        return Ok(Some(35));
                    }
                    native.with_thread_request_memory(
                        &mut request,
                        image,
                        dependencies,
                        &mut mappings,
                        |memory| memory.validate_write(output, 8),
                    )?;
                    let mxcsr = request.mxcsr()?;
                    let id = native.tls.next_thread_id()?.max(self.next_thread);
                    let next = id.checked_add(1).ok_or("Thread identity overflow")?;
                    let mut child_mappings: Vec<_> = mappings
                        .iter_mut()
                        .map(|region| DataRegion {
                            address: region.address,
                            bytes: &mut *region.bytes,
                            writable: region.writable,
                        })
                        .collect();
                    child_mappings.push(request.stack_region()?);
                    let old_fuel = native.fuel;
                    native.fuel = native.fuel.min(self.remaining_instructions);
                    let prepared = native.prepare_worker(
                        image,
                        dependencies,
                        (id, entry, argument, mxcsr, options.stack_size),
                        &mut child_mappings,
                    );
                    native.fuel = old_fuel;
                    drop(child_mappings);
                    let prepared = prepared?;
                    native.tls.register_thread(id)?;
                    if let Err(error) = native.with_thread_request_memory(
                        &mut request,
                        image,
                        dependencies,
                        &mut mappings,
                        |memory| memory.store(output, id, 8),
                    ) {
                        native.tls.retire_thread(id)?;
                        return Err(error);
                    }
                    self.next_thread = next;
                    child = Some(Worker {
                        lease: lease.clone(),
                        external: false,
                        thread: id,
                        detached: options.detached,
                        joiner: None,
                        state: State::Suspended(Execution::QuantumYield(Box::new(prepared))),
                    });
                    Ok(Some(0))
                }
                ThreadOperation::Join { thread, output } => {
                    if thread == parent {
                        return Ok(Some(11));
                    }
                    let Some((target, detached, owner, value, reaped)) = target else {
                        return Ok(Some(3));
                    };
                    if detached || reaped || owner.is_some_and(|owner| owner != parent) {
                        return Ok(Some(22));
                    }
                    if let Some(value) = value {
                        if output != 0 {
                            native.with_thread_request_memory(
                                &mut request,
                                image,
                                dependencies,
                                &mut mappings,
                                |memory| memory.store(output, value, 8),
                            )?;
                        }
                        reap = Some(target);
                        Ok(Some(0))
                    } else {
                        joiner = Some(target);
                        Ok(None)
                    }
                }
                ThreadOperation::Detach { .. } => {
                    let Some((target, detached, owner, value, reaped)) = target else {
                        return Ok(Some(3));
                    };
                    if detached || reaped || owner.is_some() {
                        return Ok(Some(22));
                    }
                    if value.is_some() {
                        reap = Some(target);
                    } else {
                        detach = Some(target);
                    }
                    Ok(Some(0))
                }
                ThreadOperation::Completed(_) => Err("Already processed thread request".into()),
            }
        })();
        drop(mappings);
        match result {
            Ok(result) => {
                if let Some(value) = result {
                    request.complete(value);
                }
                self.workers[index].state = State::Suspended(Execution::ThreadRequest(request));
                if let Some(child) = child {
                    self.insert_worker(child);
                }
                if let Some(target) = reap {
                    self.workers[target].state = State::Reaped;
                }
                if let Some(target) = detach {
                    self.workers[target].detached = true;
                }
                if let Some(target) = joiner {
                    self.workers[target].joiner = Some(parent);
                }
                Ok(if result.is_some() {
                    Step::ThreadService(parent)
                } else {
                    Step::JoinPending(parent)
                })
            }
            Err(error) => {
                self.workers[index].state = State::Suspended(Execution::ThreadRequest(request));
                self.failure = Some(error.clone());
                Err(error)
            }
        }
    }
    /// Every dispatch has a bounded native instruction quantum and retains its
    /// cumulative per-worker fuel/transition limits. The process-wide dispatch
    /// budget is never replenished by enqueueing, returning or switching workers.
    pub fn new(quantum: u64, dispatch_budget: u64, instruction_budget: u64) -> Result<Self> {
        if quantum == 0 || dispatch_budget == 0 || instruction_budget == 0 {
            return Err("Scheduler budgets must be positive".into());
        }
        Ok(Self {
            exit_request: None,
            next_thread: 2,
            failure: None,
            owner: None,
            workers: Vec::new(),
            cursor: 0,
            quantum,
            remaining_dispatches: dispatch_budget,
            remaining_instructions: instruction_budget,
        })
    }
    /// Register an explicit fresh TLS identity and enqueue actual guest code.
    /// Successful enqueue is not a completed guest pthread_create operation.
    pub fn enqueue(
        &mut self,
        native: &mut NativeExecutor,
        thread: u64,
        entry: u64,
        argument: u64,
    ) -> Result<()> {
        self.require_owner(native)?;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if (self.workers.len() == 4096
            && !self
                .workers
                .iter()
                .any(|w| matches!(w.state, State::Reaped)))
            || self
                .workers
                .iter()
                .filter(|w| !matches!(w.state, State::Completed(_) | State::Reaped))
                .count()
                == WORKER_LIMIT
        {
            return Err("Scheduler worker budget exhausted".into());
        }
        if entry == 0 {
            return Err("Worker entry must be nonzero".into());
        }
        self.workers
            .try_reserve(1)
            .map_err(|_| "Cannot reserve worker record")?;
        let next = thread.checked_add(1).ok_or("Thread identity overflow")?;
        native.tls.register_thread(thread)?;
        self.next_thread = self.next_thread.max(next);
        self.insert_worker(Worker {
            lease: None,
            external: false,
            detached: false,
            joiner: None,
            thread,
            state: State::Ready { entry, argument },
        });
        Ok(())
    }
    pub fn result(&self, thread: u64) -> Option<u64> {
        self.workers
            .iter()
            .find(|w| w.thread == thread)
            .and_then(|w| {
                if let State::Completed(v) = w.state {
                    Some(v)
                } else {
                    None
                }
            })
    }
    pub fn remaining_dispatches(&self) -> u64 {
        self.remaining_dispatches
    }
    pub fn remaining_instructions(&self) -> u64 {
        self.remaining_instructions
    }
    fn require_owner(&mut self, native: &NativeExecutor) -> Result<()> {
        let identity = native.execution_identity();
        if let Some(owner) = &self.owner {
            if !Rc::ptr_eq(owner, &identity) {
                return Err("Scheduler belongs to another executor".into());
            }
        } else {
            self.owner = Some(identity);
        }
        Ok(())
    }
    /// All supplied data, including suspended parent stacks, must remain alive
    /// for every dispatch. Other workers' owned stacks are mapped automatically.
    /// Select the parent identity explicitly before resuming parent code; the
    /// executor retains the dispatched identity, including after its retirement.
    pub fn step(
        &mut self,
        native: &mut NativeExecutor,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        parent_data: &mut [DataRegion<'_>],
    ) -> Result<Step> {
        let len = self.workers.len();
        let bias = (0..len)
            .map(|n| (self.cursor + n) % len)
            .find(|&i| {
                !matches!(
                    self.workers[i].state,
                    State::Completed(_) | State::Reaped | State::ExternalBoundary(_)
                )
            })
            .and_then(|i| self.workers[i].lease.as_ref().map(|lease| lease.bias));
        if let Some(bias) = bias.filter(|bias| *bias != image.bias) {
            let index = dependencies
                .iter()
                .position(|mapped| mapped.bias == bias)
                .ok_or("Live worker primary image is not mapped")?;
            let (before, tail) = dependencies.split_at_mut(index);
            let (selected, after) = tail.split_first_mut().unwrap();
            let mut others: Vec<&mut Image> = before
                .iter_mut()
                .chain(after.iter_mut())
                .map(|mapped| &mut **mapped)
                .collect();
            others.push(image);
            self.step_mapped(native, selected, &mut others, parent_data)
        } else {
            self.step_mapped(native, image, dependencies, parent_data)
        }
    }
    fn step_mapped(
        &mut self,
        native: &mut NativeExecutor,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        parent_data: &mut [DataRegion<'_>],
    ) -> Result<Step> {
        self.require_owner(native)?;
        if let Some(request) = self.exit_request {
            return Ok(Step::ExitRequested(request));
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let len = self.workers.len();
        let Some(index) = (0..len).map(|n| (self.cursor + n) % len).find(|&i| {
            !matches!(
                self.workers[i].state,
                State::Completed(_) | State::Reaped | State::ExternalBoundary(_)
            )
        }) else {
            return Ok(Step::Finished);
        };
        if self.remaining_dispatches == 0 {
            return Err("Scheduler process dispatch budget exhausted".into());
        }
        if self.remaining_instructions == 0 {
            return Err("Scheduler process instruction budget exhausted".into());
        }
        // Validate before removing any owned continuation. No mutable external
        // code runs between this pass and the scoped stack mapping below.
        for worker in &mut self.workers {
            if let State::Suspended(execution) | State::ExternalBoundary(execution) =
                &mut worker.state
            {
                execution.stack_region()?;
            }
        }
        self.cursor = (index + 1) % len;
        self.remaining_dispatches -= 1;
        let thread = self.workers[index].thread;
        native.select_guest_thread(thread)?;
        native.set_worker_quantum(Some(self.quantum))?;
        if matches!(&self.workers[index].state, State::Suspended(Execution::ThreadRequest(request)) if !matches!(request.operation(),ThreadOperation::Completed(_)))
        {
            return self.service_thread(index, native, image, dependencies, parent_data);
        }
        let mut state = std::mem::replace(&mut self.workers[index].state, State::Dispatching);
        let mut mappings = Vec::new();
        for region in parent_data.iter_mut() {
            mappings.push(DataRegion {
                address: region.address,
                bytes: &mut *region.bytes,
                writable: region.writable,
            });
        }
        for (other, worker) in self.workers.iter_mut().enumerate() {
            if other != index {
                if let State::Suspended(execution) | State::ExternalBoundary(execution) =
                    &mut worker.state
                {
                    if let Some(region) = execution
                        .stack_region()
                        .expect("Suspended stack validated before dispatch")
                    {
                        mappings.push(region);
                    }
                }
            }
        }
        if let State::Suspended(execution) | State::ExternalBoundary(execution) = &mut state {
            execution.cap_instruction_fuel(self.remaining_instructions);
        }
        let original_fuel = native.fuel;
        native.fuel = native.fuel.min(self.remaining_instructions);
        let before = native.consumed_instruction_fuel();
        let outcome = match &mut state {
            State::Ready { entry, argument } => native
                .run_worker_with_data(image, *entry, *argument, dependencies, &mut mappings)
                .map(Some),
            State::Suspended(Execution::QuantumYield(wait)) => native
                .resume_quantum(image, dependencies, wait, &mut mappings)
                .map(Some),
            State::Suspended(Execution::MutexWait(wait)) => {
                native.resume_mutex_with_data(image, dependencies, wait, &mut mappings)
            }
            State::Suspended(Execution::ThreadRequest(request)) => match request.operation() {
                ThreadOperation::Completed(result) => native
                    .finish_thread_request(request, image, dependencies, &mut mappings, result)
                    .map(Some),
                _ => Err("Unprocessed thread request".into()),
            },
            State::Suspended(_) => Ok(None),
            _ => Err("Invalid scheduler dispatch state".into()),
        };
        native.fuel = original_fuel;
        let spent = native.consumed_instruction_fuel() - before;
        drop(mappings);
        let Some(remaining) = u64::try_from(spent)
            .ok()
            .and_then(|spent| self.remaining_instructions.checked_sub(spent))
        else {
            let error = "Scheduler instruction accounting exceeded cap".to_string();
            self.failure = Some(error.clone());
            self.workers[index].state = state;
            return Err(error);
        };
        self.remaining_instructions = remaining;
        if let Ok(Some(Execution::ExitRequested(request))) = &outcome {
            self.exit_request = Some(*request);
            self.workers[index].state = State::Suspended(Execution::ExitRequested(*request));
            return Ok(Step::ExitRequested(*request));
        }
        match outcome {
            Ok(Some(execution))
                if self.workers[index].external
                    && !matches!(
                        execution,
                        Execution::QuantumYield(_)
                            | Execution::MutexWait(_)
                            | Execution::ThreadRequest(_)
                    ) =>
            {
                self.workers[index].state = State::ExternalBoundary(execution);
                Ok(Step::ServiceBoundary(thread))
            }
            Err(error) => {
                self.failure = Some(error.clone());
                self.workers[index].state = state;
                Err(error)
            }
            Ok(None) => {
                let step = if matches!(state, State::Suspended(Execution::MutexWait(_))) {
                    Step::MutexPending(thread)
                } else {
                    Step::ServiceBoundary(thread)
                };
                self.workers[index].state = state;
                Ok(step)
            }
            Ok(Some(Execution::WorkerReturned(value))) => {
                self.workers[index].state = if self.workers[index].detached {
                    State::Reaped
                } else {
                    State::Completed(value)
                };
                if let Err(error) = native.tls.retire_thread(thread) {
                    self.failure = Some(error.clone());
                    return Err(error);
                }
                Ok(Step::Completed { thread, value })
            }
            Ok(Some(execution)) => {
                let step = match execution {
                    Execution::QuantumYield(_) => Step::Yielded(thread),
                    Execution::MutexWait(_) => Step::MutexPending(thread),
                    _ => Step::ServiceBoundary(thread),
                };
                self.workers[index].state = State::Suspended(execution);
                Ok(step)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_reaped_external_record_does_not_shadow_live_boundary() {
        let mut scheduler = Scheduler::new(1, 10, 10).unwrap();
        scheduler.workers.push(Worker {
            lease: None,
            external: true,
            thread: 1,
            detached: false,
            joiner: None,
            state: State::Reaped,
        });
        scheduler.workers.push(Worker {
            lease: None,
            external: true,
            thread: 1,
            detached: false,
            joiner: None,
            state: State::ExternalBoundary(Execution::Returned(17)),
        });
        assert!(matches!(
            scheduler.take_parent_boundary(1),
            Some(Execution::Returned(17))
        ));
        assert!(scheduler.take_parent_boundary(1).is_none());
    }
    #[test]
    fn overflowing_enqueue_identity_does_not_register_tls() {
        let mut scheduler = Scheduler::new(1, 10, 10).unwrap();
        let mut native = NativeExecutor::new(0x900000, 4096, 10, 10).unwrap();
        assert!(
            scheduler
                .enqueue(&mut native, u64::MAX, 0x100100, 0)
                .is_err()
        );
        assert_eq!(native.tls.live_threads(), 1);
        assert_eq!(native.tls.next_thread_id().unwrap(), 2);
        assert!(scheduler.workers.is_empty());
    }
    #[test]
    fn reaped_records_are_reused_without_reusing_thread_identity() {
        let mut scheduler = Scheduler::new(1, 20000, 20000).unwrap();
        for thread in 2..10002 {
            scheduler.insert_worker(Worker {
                lease: None,
                external: false,
                thread,
                detached: false,
                joiner: None,
                state: State::Completed(thread),
            });
            assert_eq!(scheduler.workers.len(), 1);
            assert_eq!(scheduler.result(thread), Some(thread));
            if thread > 2 {
                assert_eq!(scheduler.result(thread - 1), None);
            }
            scheduler.workers[0].state = State::Reaped;
        }
    }
}
