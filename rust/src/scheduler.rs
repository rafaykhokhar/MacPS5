//! Bounded cooperative execution of explicitly enqueued guest workers.
//! This is not pthread_create/join: callers retain images and parent stack data.
use crate::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor},
};
type Result<T> = std::result::Result<T, String>;
use std::rc::Rc;
const WORKER_LIMIT: usize = 63;
enum State {
    Ready { entry: u64, argument: u64 },
    Suspended(Execution),
    Completed(u64),
    Dispatching,
}
struct Worker {
    thread: u64,
    state: State,
}
/// One scheduling action. Service boundaries remain retained for future runtime
/// orchestration; they are never replaced with a fabricated success result.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Yielded(u64),
    MutexPending(u64),
    Completed { thread: u64, value: u64 },
    ServiceBoundary(u64),
    Finished,
}
pub struct Scheduler {
    failure: Option<String>,
    owner: Option<Rc<()>>,
    workers: Vec<Worker>,
    cursor: usize,
    quantum: u64,
    remaining_dispatches: u64,
    remaining_instructions: u64,
}
impl Scheduler {
    /// Every dispatch has a bounded native instruction quantum and retains its
    /// cumulative per-worker fuel/transition limits. The process-wide dispatch
    /// budget is never replenished by enqueueing, returning or switching workers.
    pub fn new(quantum: u64, dispatch_budget: u64, instruction_budget: u64) -> Result<Self> {
        if quantum == 0 || dispatch_budget == 0 || instruction_budget == 0 {
            return Err("Scheduler budgets must be positive".into());
        }
        Ok(Self {
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
        if self.workers.len() == 4096
            || self
                .workers
                .iter()
                .filter(|w| !matches!(w.state, State::Completed(_)))
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
        native.tls.register_thread(thread)?;
        self.workers.push(Worker {
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
        self.require_owner(native)?;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let len = self.workers.len();
        let Some(index) = (0..len)
            .map(|n| (self.cursor + n) % len)
            .find(|&i| !matches!(self.workers[i].state, State::Completed(_)))
        else {
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
            if let State::Suspended(execution) = &mut worker.state {
                execution.stack_region()?;
            }
        }
        self.cursor = (index + 1) % len;
        self.remaining_dispatches -= 1;
        let thread = self.workers[index].thread;
        native.select_guest_thread(thread)?;
        native.set_worker_quantum(Some(self.quantum))?;
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
                if let State::Suspended(execution) = &mut worker.state {
                    if let Some(region) = execution
                        .stack_region()
                        .expect("Suspended stack validated before dispatch")
                    {
                        mappings.push(region);
                    }
                }
            }
        }
        if let State::Suspended(execution) = &mut state {
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
        match outcome {
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
                self.workers[index].state = State::Completed(value);
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
