//! Resumable native initialization; pending calls remain fault-pinned until complete.
#![allow(unsafe_code)]
use super::*;
use crate::module_executor::{
    Continuation, Execution, ExitRequest, MutexWait, NativeExecutor, ThreadRequest,
};
use std::collections::VecDeque;
pub struct ProcessData {
    pub address: u64,
    pub bytes: Vec<u8>,
    pub writable: bool,
}
/// An already executing root at a load-service boundary. This explicitly keeps
/// its unresolved imports; it is not the normal fully linked module-load API.
pub struct Bootstrap {
    pub image: Image,
    pub continuation: Box<Continuation>,
    pub arguments: [u64; 6],
    pub data: Vec<ProcessData>,
}

pub enum StartEvent {
    ThreadRequest { thread: u64 },
    MutexWait { thread: u64, slot: u64 },
    ExitRequested(ExitRequest),
    SysmoduleLoadInternal { arguments: [u64; 6] },
    Dlsym { arguments: [u64; 6] },
    Started,
    LoadStartModule { arguments: [u64; 6] },
    StopUnloadModule { arguments: [u64; 6] },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartOutcome {
    ThreadRequest { handle: u64, thread: u64 },
    MutexWait { handle: u64, thread: u64, slot: u64 },
    Started,
    ExitRequested(ExitRequest),
}
pub enum LoadProgress {
    Child { handle: u64, event: StartEvent },
    Caller { event: StartEvent },
}
pub struct LoadConfig<'a> {
    pub bias: u64,
    pub instruction_limit: usize,
    pub archive: &'a std::path::Path,
    pub directory: &'a std::path::Path,
}
pub struct LoadPlan<'a> {
    /// Explicit candidate load biases from the host; overlaps still reject.
    pub biases: &'a mut dyn Iterator<Item = u64>,
    pub max_requests: usize,
    pub instruction_limit: usize,
    pub archive: &'a std::path::Path,
    pub directory: &'a std::path::Path,
}
pub(super) struct StartState {
    scheduled_thread: Option<u64>,
    thread_request: Option<Box<ThreadRequest>>,
    calls: VecDeque<(u64, bool)>,
    arguments: Arguments,
    status_return: bool,
    continuation: Option<Box<Continuation>>,
    mutex_wait: Option<Box<MutexWait>>,
    request: Option<[u64; 6]>,
    loaded: Option<(u64, u64)>,
    stop_request: bool,
    dlsym_request: bool,
    sysmodule_request: bool,
    stopping: Option<u64>,
}
impl Modules {
    /// A deadline only after scheduling observed all guest work blocked.
    pub fn native_blocked_sleep_deadline(&self) -> Option<u128> {
        self.thread_scheduler
            .as_ref()
            .and_then(|s| s.blocked_sleep_deadline())
    }
    /// Earliest retained sleeper in the executor's monotonic clock domain.
    /// The host driver may wait only after scheduling reports no runnable work.
    pub fn native_sleep_deadline(&self) -> Option<u128> {
        self.thread_scheduler
            .as_ref()
            .and_then(|s| s.next_sleep_deadline())
    }
    /// Validate first, then revoke native code and remove the image without any
    /// remaining fallible operation. Failed preflight leaves both registrations.
    pub fn unload_native(&mut self, handle: u64, executor: &mut NativeExecutor) -> Result<()> {
        self.validate_unload(handle)?;
        let bias = self
            .modules
            .get(&handle)
            .and_then(|module| module.image.as_ref())
            .map(|image| image.bias);
        if let Some(bias) = bias {
            executor.remove_image_translations(bias);
        }
        self.commit_unload(handle);
        Ok(())
    }
    /// Run one background scheduling action while preserving the caller's thread
    /// identity. Worker leases choose and pin each original module mapping.
    pub fn step_native_workers(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
    ) -> Result<crate::scheduler::Step> {
        self.require_native_running(executor)?;
        let caller = executor.guest_thread_id();
        let mut scheduler = self
            .thread_scheduler
            .take()
            .ok_or("No process thread scheduler")?;
        self.thread_image_pins = scheduler.live_image_pins();
        let result = self.with_native_images(handle, |image, dependencies, data| {
            scheduler.step(executor, image, dependencies, data)
        });
        self.thread_image_pins = scheduler.live_image_pins();
        let exit = scheduler.exit_request();
        self.thread_scheduler = Some(scheduler);
        if let Some(request) = exit {
            self.exit_request = Some(request);
            return Ok(crate::scheduler::Step::ExitRequested(request));
        }
        executor.select_guest_thread(caller)?;
        result
    }
    pub(super) fn check_thread_pin(&self, handle: u64) -> Result<()> {
        if self.thread_image_pins.contains(&handle) {
            return Err("Live guest worker pins module image".into());
        }
        Ok(())
    }
    fn thread_image_lease(&self, handle: u64) -> Result<crate::scheduler::ImageLease> {
        let bias = self
            .modules
            .get(&handle)
            .and_then(|module| module.image.as_ref())
            .ok_or("Missing thread owner image")?
            .bias;
        let mut handles = BTreeSet::new();
        let mut pending = vec![handle];
        while let Some(id) = pending.pop() {
            if handles.insert(id) {
                let module = self
                    .modules
                    .get(&id)
                    .ok_or("Missing worker image provider")?;
                pending.extend(module.dependencies.iter().copied());
                for (&parent, state) in &self.pending_starts {
                    if state.loaded.is_some_and(|(child, _)| child == id)
                        || state.stopping == Some(id)
                    {
                        pending.push(parent);
                    }
                }
            }
        }
        Ok(crate::scheduler::ImageLease { bias, handles })
    }
    /// Advance a retained initializer through real worker scheduling. The
    /// process-wide (quantum, dispatch, instruction) budgets are installed only
    /// on first use and never reset by another module or another call. This
    /// Background workers retain their image/provider leases after parent handback.
    pub fn resume_native_threads(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        budgets: (u64, u64, u64),
        max_steps: usize,
    ) -> Result<StartEvent> {
        if max_steps == 0 || max_steps > 10000 {
            return Err("Invalid thread scheduling step limit".into());
        }
        self.require_native_running(executor)?;
        self.require_pending_thread(handle, executor)?;
        let lease = self.thread_image_lease(handle)?;
        let mut state = self
            .pending_starts
            .remove(&handle)
            .ok_or("No pending module start")?;
        let thread = state.scheduled_thread.or_else(|| {
            state
                .thread_request
                .as_ref()
                .and_then(|request| request.guest_thread_id())
        });
        let Some(thread) = thread else {
            self.pending_starts.insert(handle, state);
            return Err("No pending module thread request".into());
        };
        let mut scheduler = match self.thread_scheduler.take() {
            Some(scheduler) => scheduler,
            None => match crate::scheduler::Scheduler::new(budgets.0, budgets.1, budgets.2) {
                Ok(scheduler) => scheduler,
                Err(error) => {
                    self.pending_starts.insert(handle, state);
                    return Err(error);
                }
            },
        };
        let result = (|| -> Result<Option<Execution>> {
            if state.scheduled_thread.is_none() {
                scheduler.adopt_parent(executor, &mut state.thread_request, lease)?;
                state.scheduled_thread = Some(thread);
            }
            for _ in 0..max_steps {
                self.thread_image_pins = scheduler.live_image_pins();
                let step = self.with_native_images(handle, |image, dependencies, data| {
                    scheduler.step(executor, image, dependencies, data)
                })?;
                if matches!(step, crate::scheduler::Step::Sleeping { .. }) {
                    break;
                }
                if let Some(request) = scheduler.exit_request() {
                    return Ok(Some(Execution::ExitRequested(request)));
                }
                if let Some(outcome) = scheduler.take_parent_boundary(thread) {
                    return Ok(Some(outcome));
                }
            }
            Ok(None)
        })();
        self.thread_image_pins = scheduler.live_image_pins();
        self.thread_scheduler = Some(scheduler);
        // Parent TLS is externally owned; scheduling must never retire it.
        let selected = executor.select_guest_thread(thread);
        match result {
            Ok(Some(outcome)) => {
                state.scheduled_thread = None;
                if !matches!(outcome, Execution::ExitRequested(_)) {
                    if let Err(error) = selected {
                        self.pending_starts.insert(handle, state);
                        return Err(error);
                    }
                }
                self.advance_native(handle, executor, state, Some(outcome))
            }
            other => {
                self.pending_starts.insert(handle, state);
                selected?;
                match other {
                    Ok(None) => Ok(StartEvent::ThreadRequest { thread }),
                    Err(error) => Err(error),
                    _ => unreachable!(),
                }
            }
        }
    }
    /// Until a scheduler supplies per-request thread contexts, reject dispatch
    /// under a different selected guest identity before loading, writing results,
    /// acquiring pins, or taking the caller's continuation. A caller may select
    /// the originating identity and retry the unchanged pending request.
    fn require_pending_thread(&self, caller: u64, executor: &NativeExecutor) -> Result<()> {
        if let Some(state) = self.pending_starts.get(&caller) {
            let thread = if let Some(thread) = state.scheduled_thread {
                thread
            } else if let Some(request) = &state.thread_request {
                request.guest_thread_id().ok_or("Thread request consumed")?
            } else if let Some(wait) = &state.mutex_wait {
                wait.guest_thread_id()
            } else {
                state
                    .continuation
                    .as_ref()
                    .ok_or("Missing pending guest thread context")?
                    .guest_thread_id()
            };
            if thread != executor.guest_thread_id() {
                return Err("Pending module request belongs to a different guest thread; select its originating context before dispatch".into());
            }
        }
        Ok(())
    }
    pub fn exit_request(&self) -> Option<ExitRequest> {
        self.exit_request
    }
    fn require_native_running(&mut self, executor: &NativeExecutor) -> Result<()> {
        if self.exit_request.is_none() {
            self.exit_request = executor.exit_request();
        }
        if self.exit_request.is_some() {
            return Err(
                "Guest exit requested; module execution prohibited while cleanup is pending".into(),
            );
        }
        executor.require_running()
    }
    /// Acquire an explicitly bound, started provider. A missing provider is an
    /// unsupported runtime dependency, never a success-only sysmodule stub.
    pub fn dispatch_native_sysmodule(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let state = self
            .pending_starts
            .get(&caller)
            .ok_or("No pending module start")?;
        if !state.sysmodule_request {
            return Err("Pending request is not sysmodule load".into());
        }
        let arguments = state.request.ok_or("Missing sysmodule request")?;
        // The API argument is uint32_t; upper register bits are not part of it.
        let id = arguments[0] as u32;
        if id & 0x7fff_ffff == 0 {
            return Err("Invalid internal sysmodule ID".into());
        }
        self.retain_sysmodule(id)?;
        self.resume_native(caller, executor, 0)
    }

    pub fn adopt_bootstrap(
        &mut self,
        name: &str,
        mut bootstrap: Bootstrap,
        executor: &mut NativeExecutor,
    ) -> Result<u64> {
        self.require_native_running(executor)?;
        if bootstrap.continuation.guest_thread_id() != executor.guest_thread_id() {
            return Err("Bootstrap belongs to a different guest thread".into());
        }
        if !self.process_data.is_empty() || self.modules.values().any(|m| m.image.is_some()) {
            return Err("Bootstrap requires a manager without guest images".into());
        }
        let handle = self.available(name)?;
        let bytes = bootstrap
            .image
            .file
            .len()
            .checked_add(
                bootstrap
                    .image
                    .segments
                    .iter()
                    .map(|s| s.bytes.len())
                    .sum::<usize>(),
            )
            .and_then(|n| {
                bootstrap
                    .data
                    .iter()
                    .try_fold(n, |n, r| n.checked_add(r.bytes.len()))
            })
            .ok_or("Bootstrap byte count overflow")?;
        if bytes > self.budget - self.bytes {
            return Err("Bootstrap exceeds module budget".into());
        }
        let mut registry = self.registry.clone();
        registry.register_module(name, &[])?;
        let mut data: Vec<_> = bootstrap
            .data
            .iter_mut()
            .map(|r| crate::guest_memory::DataRegion {
                address: r.address,
                bytes: &mut r.bytes,
                writable: r.writable,
            })
            .collect();
        executor.with_suspended_memory(
            &mut bootstrap.continuation,
            &mut bootstrap.image,
            &mut [],
            &mut data,
            |memory| {
                crate::module_executor::LoadRequest::read(memory, bootstrap.arguments).map(|_| ())
            },
        )?;
        let dependencies = self.modules.keys().copied().collect();
        self.commit(
            handle,
            Module {
                name: name.into(),
                image: Some(bootstrap.image),
                entries: EntryPoints::default(),
                lifecycle: crate::lifecycle::Lifecycle::default(),
                dependencies,
                phase: Phase::Faulted,
                bytes,
            },
            registry,
        );
        self.process_data = bootstrap.data;
        self.bootstrap = Some(handle);
        self.pending_starts.insert(
            handle,
            StartState {
                calls: VecDeque::new(),
                arguments: Arguments::default(),
                status_return: true,
                continuation: Some(bootstrap.continuation),
                scheduled_thread: None,
                thread_request: None,
                mutex_wait: None,
                request: Some(bootstrap.arguments),
                loaded: None,
                stop_request: false,
                dlsym_request: false,
                sysmodule_request: false,
                stopping: None,
            },
        );
        Ok(handle)
    }
    pub fn dispatch_native_dlsym(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let state = self
            .pending_starts
            .get(&caller)
            .ok_or("No pending module start")?;
        if !state.dlsym_request {
            return Err("Pending request is not dlsym".into());
        }
        let [handle, name, output, ..] = state.request.ok_or("Missing dlsym request")?;
        let symbol = self.with_pending_memory(caller, executor, |memory| {
            if name == 0 || output == 0 {
                return Err("Null dlsym pointer".into());
            }
            memory.validate_write(output, 8)?;
            let mut bytes = Vec::new();
            for offset in 0..4096 {
                let byte = memory.load(
                    name.checked_add(offset).ok_or("Symbol address overflow")?,
                    1,
                )? as u8;
                if byte == 0 {
                    return String::from_utf8(bytes).map_err(|_| "Invalid symbol encoding".into());
                }
                bytes.push(byte);
            }
            Err("Unterminated dlsym symbol".into())
        });
        let Ok(symbol) = symbol else {
            return self.resume_native(caller, executor, 0x8002000e);
        };
        let address = self
            .modules
            .get(&handle)
            .filter(|module| module.phase == Phase::Started || handle == caller)
            .and_then(|module| self.registry.resolve_dynamic(&module.name, &symbol));
        let Some(address) = address else {
            return self.resume_native(caller, executor, 0x80020003);
        };
        self.with_pending_memory(caller, executor, |memory| memory.store(output, address, 8))?;
        if handle != caller {
            self.modules
                .get_mut(&caller)
                .unwrap()
                .dependencies
                .insert(handle);
        }
        self.resume_native(caller, executor, 0)
    }
    pub fn dispatch_native_stop(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let state = self
            .pending_starts
            .get(&caller)
            .ok_or("No pending module start")?;
        if !state.stop_request {
            return Err("Pending request is not stop/unload".into());
        }
        let [child, size, pointer, flags, options, result] =
            state.request.ok_or("Missing stop request")?;
        if flags != 0 || options != 0 {
            return Err("Stop flags/options are unsupported".into());
        }
        if size != 0 {
            self.with_pending_memory(caller, executor, |memory| {
                memory.read(
                    pointer,
                    usize::try_from(size).map_err(|_| "Stop argument size overflow")?,
                )?;
                Ok(())
            })?;
        }
        if result != 0 {
            if self.image(child).is_some_and(|image| {
                image.segments.iter().any(|segment| {
                    result >= segment.address
                        && result - segment.address < segment.bytes.len() as u64
                })
            }) {
                return Err("Stop result points into module being unloaded".into());
            }
            self.with_pending_memory(caller, executor, |memory| memory.validate_write(result, 4))?;
        }
        if let Err(error) =
            self.stop_dynamic_module(caller, child, executor, Arguments { size, pointer })
        {
            if let Some(request) = executor.exit_request() {
                self.exit_request = Some(request);
                return Ok(StartEvent::ExitRequested(request));
            }
            return Err(error);
        }
        if result != 0 {
            self.with_pending_memory(caller, executor, |memory| memory.store(result, 0, 4))?;
        }
        self.resume_native(caller, executor, 0)
    }
    /// Release a module loaded by this caller. Static import dependencies cannot
    /// be discarded this way. The caller's pin stays in place throughout guest
    /// stop/finalizers, including on fault or panic; code is removed only on success.
    pub fn stop_dynamic_module(
        &mut self,
        caller: u64,
        child: u64,
        executor: &mut NativeExecutor,
        arguments: Arguments,
    ) -> Result<()> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        self.check_thread_pin(child)?;
        self.check_sysmodule_pin(child)?;
        if !self.dynamic_loads.contains(&(caller, child)) {
            return Err("Caller does not own this dynamic module load".into());
        }
        if self.phase(caller) != Some(Phase::Started)
            && !self
                .pending_starts
                .get(&caller)
                .is_some_and(|state| state.stop_request)
        {
            return Err("Caller is not active at a stop/unload boundary".into());
        }
        if self
            .pending_starts
            .get(&caller)
            .is_some_and(|state| state.loaded.is_some_and(|(loaded, _)| loaded == child))
        {
            return Err("Child load has not returned to caller".into());
        }
        let module = self.modules.get(&child).ok_or("Unknown child module")?;
        if module.phase != Phase::Started || module.image.is_none() {
            return Err("Dynamic module cannot stop in its current state".into());
        }
        if self
            .modules
            .iter()
            .any(|(&id, module)| id != caller && module.dependencies.contains(&child))
        {
            return Err("Another module pins dynamic load".into());
        }
        if let Some(state) = self.pending_starts.get_mut(&caller) {
            state.stopping = Some(child);
            let outcome = self.stop_native_shared(child, executor, arguments);
            self.pending_starts.get_mut(&caller).unwrap().stopping = None;
            outcome?;
        } else {
            if arguments.size != 0 {
                return Err("Stop arguments require a suspended caller".into());
            }
            self.transition(child, false, executor, arguments)?;
        }
        self.modules
            .get_mut(&caller)
            .ok_or("Missing dynamic caller")?
            .dependencies
            .remove(&child);
        if let Err(error) = self.unload_native(child, executor) {
            self.modules
                .get_mut(&caller)
                .unwrap()
                .dependencies
                .insert(child);
            return Err(error);
        }
        Ok(())
    }
    fn stop_native_shared(
        &mut self,
        child: u64,
        executor: &mut NativeExecutor,
        arguments: Arguments,
    ) -> Result<()> {
        self.check_thread_pin(child)?;
        let module = self.modules.get_mut(&child).ok_or("Missing stop image")?;
        let calls: Vec<_> = module
            .entries
            .stop
            .into_iter()
            .map(|address| (address, true))
            .chain(
                module
                    .lifecycle
                    .finalizers
                    .iter()
                    .map(|&address| (address, false)),
            )
            .collect();
        module.phase = Phase::Faulted;
        for (address, status) in calls {
            let arguments = if status {
                arguments
            } else {
                Arguments::default()
            };
            let result = self.with_native_images(child, |image, dependencies, data| {
                executor.run_with_data(image, address, arguments, dependencies, data)
            })?;
            match result {
                Execution::ExitRequested(request) => {
                    self.exit_request = Some(request);
                    return Err("Guest exit requested during stop; cleanup remains pending".into());
                }
                Execution::Returned(value) => {
                    if status && value != 0 {
                        return Err(format!("Guest module returned status {value}"));
                    }
                }
                _ => return Err("Service yields during stop/finalizers are unsupported".into()),
            }
        }
        self.modules.get_mut(&child).unwrap().phase = Phase::Stopped;
        Ok(())
    }
    /// Drive nested guest module loads until the requested module actually starts.
    /// Request limits also bound repeated missing-file probes. On failure, pending
    /// and faulted images remain owned and pinned; no successful start is invented.
    /// # Safety
    /// The archive must match this exact trusted native runtime and CPU/Memory ABI.
    pub unsafe fn start_native_with_files(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        arguments: Arguments,
        files: &mut crate::module_files::ModuleFiles,
        plan: LoadPlan<'_>,
    ) -> Result<StartOutcome> {
        let mut active = handle;
        let mut event = self.start_native(handle, executor, arguments)?;
        let mut parents = Vec::new();
        let mut requests = 0usize;
        loop {
            match event {
                StartEvent::ThreadRequest { thread } => {
                    return Ok(StartOutcome::ThreadRequest {
                        handle: active,
                        thread,
                    });
                }
                StartEvent::MutexWait { thread, slot } => {
                    return Ok(StartOutcome::MutexWait {
                        handle: active,
                        thread,
                        slot,
                    });
                }
                StartEvent::ExitRequested(request) => {
                    return Ok(StartOutcome::ExitRequested(request));
                }
                StartEvent::SysmoduleLoadInternal { .. } => {
                    if requests >= plan.max_requests {
                        return Err("Native module load request budget exhausted".into());
                    }
                    requests += 1;
                    event = self.dispatch_native_sysmodule(active, executor)?;
                }
                StartEvent::Dlsym { .. } => {
                    if requests >= plan.max_requests {
                        return Err("Native module load request budget exhausted".into());
                    }
                    requests += 1;
                    event = self.dispatch_native_dlsym(active, executor)?;
                }
                StartEvent::StopUnloadModule { .. } => {
                    if requests >= plan.max_requests {
                        return Err("Native module load request budget exhausted".into());
                    }
                    requests += 1;
                    event = self.dispatch_native_stop(active, executor)?;
                }
                StartEvent::Started => {
                    let Some(parent) = parents.pop() else {
                        return Ok(StartOutcome::Started);
                    };
                    active = parent;
                    event = self.finish_native_load(parent, executor)?;
                }
                StartEvent::LoadStartModule { .. } => {
                    if requests >= plan.max_requests {
                        return Err("Native module load request budget exhausted".into());
                    }
                    requests += 1;
                    let bias = plan
                        .biases
                        .next()
                        .ok_or("Native module load addresses exhausted")?;
                    // SAFETY: matching archive contract forwarded from this caller.
                    let progress = unsafe {
                        self.dispatch_native_load(
                            active,
                            executor,
                            files,
                            LoadConfig {
                                bias,
                                instruction_limit: plan.instruction_limit,
                                archive: plan.archive,
                                directory: plan.directory,
                            },
                        )?
                    };
                    match progress {
                        LoadProgress::Caller { event: next } => event = next,
                        LoadProgress::Child {
                            handle: child,
                            event: next,
                        } => {
                            parents.push(active);
                            active = child;
                            event = next;
                        }
                    }
                }
            }
        }
    }
    /// Dispatch a supported catalog load, returning ENOENT to guest code for a
    /// valid but absent catalog path. Unsupported execution/host failures remain
    /// explicit errors rather than being disguised as guest filesystem failures.
    /// # Safety
    /// Same exact-runtime archive contract as begin_native_load.
    pub unsafe fn dispatch_native_load(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
        files: &mut crate::module_files::ModuleFiles,
        config: LoadConfig<'_>,
    ) -> Result<LoadProgress> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let state = self
            .pending_starts
            .get(&caller)
            .ok_or("No pending module start")?;
        if state.stop_request || state.dlsym_request || state.sysmodule_request {
            return Err("Pending request is not load/start".into());
        }
        if state.loaded.is_some() {
            return Err("Pending request already loaded a module".into());
        }
        let arguments = state.request.ok_or("Missing loader request")?;
        let request = self.with_pending_memory(caller, executor, |memory| {
            crate::module_executor::LoadRequest::read(memory, arguments)
        })?;
        if let Some(&provider) = self.native_paths.get(&request.path) {
            if files.contains(&request.path) {
                return Err("Ambiguous native and file module path".into());
            }
            if self.phase(provider) != Some(Phase::Started)
                || request.arguments.size != 0
                || request.arguments.pointer != 0
            {
                return Err(
                    "Native provider must be started and accepts no start arguments".into(),
                );
            }
            if request.result_address != 0 {
                self.with_pending_memory(caller, executor, |memory| {
                    memory.store(request.result_address, 0, 4)
                })?;
            }
            // Shared native services stay owned by the host. The caller pins
            // their exports but cannot dynamically stop/unload the provider.
            self.modules
                .get_mut(&caller)
                .unwrap()
                .dependencies
                .insert(provider);
            let event = self.resume_native(caller, executor, provider)?;
            return Ok(LoadProgress::Caller { event });
        }
        if !files.contains(&request.path) {
            // AnyPS5/core/libs/prx/libkernel/KernelErrors.hpp: SceKernelError(2).
            let event = self.resume_native(caller, executor, 0x80020002)?;
            return Ok(LoadProgress::Caller { event });
        }
        // SAFETY: forwarded matching-runtime archive contract.
        let (handle, event) = unsafe { self.begin_native_load(caller, executor, files, config)? };
        Ok(LoadProgress::Child { handle, event })
    }
    /// Load and compile the file requested by a suspended guest call, then start
    /// it. The returned event can itself request a nested load. Finish the parent
    /// only once the child has actually started. Caller images and suspended
    /// stacks are borrowed during initialization, preserving argument pointers.
    /// # Safety
    /// The archive must be the exact trusted runtime build for this CPU/Memory ABI.
    pub unsafe fn begin_native_load(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
        files: &mut crate::module_files::ModuleFiles,
        config: LoadConfig<'_>,
    ) -> Result<(u64, StartEvent)> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let state = self
            .pending_starts
            .get(&caller)
            .ok_or("No pending module start")?;
        if state.stop_request || state.dlsym_request || state.sysmodule_request {
            return Err("Pending request is not load/start".into());
        }
        if state.loaded.is_some() {
            return Err("Pending request already loaded a module".into());
        }
        let arguments = state.request.ok_or("Missing loader request")?;
        let request = self.with_pending_memory(caller, executor, |memory| {
            crate::module_executor::LoadRequest::read(memory, arguments)
        })?;
        let input = files.read(&request.path, self.budget - self.bytes)?;
        let child = self.load_dynamic(&input.name, &input.bytes, config.bias, input.entries)?;
        self.pending_starts.get_mut(&caller).unwrap().loaded =
            Some((child, request.result_address));
        self.modules
            .get_mut(&caller)
            .unwrap()
            .dependencies
            .insert(child);
        self.dynamic_loads.insert((caller, child));
        let module = self.modules.get_mut(&child).unwrap();
        // Compilation failure retains the published image and dependency pin.
        module.phase = Phase::Faulted;
        let image = module.image.as_ref().ok_or("Missing loaded image")?;
        let mut roots = BTreeSet::new();
        for address in module
            .lifecycle
            .initializers
            .iter()
            .chain(module.lifecycle.finalizers.iter())
            .copied()
            .chain(module.entries.start)
            .chain(module.entries.stop)
        {
            roots.insert(
                address
                    .checked_sub(image.bias)
                    .ok_or("Entry below loaded image")?,
            );
        }
        for export in image.exported_symbols()? {
            if export.function {
                roots.insert(
                    export
                        .address
                        .checked_sub(image.bias)
                        .ok_or("Export below image")?,
                );
            }
        }
        if !roots.is_empty() {
            // SAFETY: forwarded caller contract for the matching trusted archive.
            unsafe {
                executor.compile_image(
                    &input.bytes,
                    config.bias,
                    roots,
                    config.instruction_limit,
                    config.archive,
                    config.directory,
                )?;
            }
        }
        self.modules.get_mut(&child).unwrap().phase = Phase::Loaded;
        let event = self.start_native(child, executor, request.arguments)?;
        Ok((child, event))
    }
    /// Write successful start status and return the real owned module handle.
    /// Failed/incomplete children cannot be converted into successful guest loads.
    pub fn finish_native_load(
        &mut self,
        caller: u64,
        executor: &mut NativeExecutor,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(caller, executor)?;
        let (child, result_address) = self
            .pending_starts
            .get(&caller)
            .and_then(|s| s.loaded)
            .ok_or("No loaded child for pending request")?;
        if self.phase(child) != Some(Phase::Started) {
            return Err("Loaded child has not started".into());
        }
        if result_address != 0 {
            self.with_pending_memory(caller, executor, |memory| {
                memory.store(result_address, 0, 4)
            })?;
        }
        self.resume_native(caller, executor, child)
    }
    fn with_native_images<T>(
        &mut self,
        handle: u64,
        operation: impl FnOnce(
            &mut Image,
            &mut [&mut Image],
            &mut [crate::guest_memory::DataRegion<'_>],
        ) -> Result<T>,
    ) -> Result<T> {
        // A pending load owns its child until start completes. Borrow each live
        // ancestor's image and stack so argp remains the original guest pointer.
        let mut ancestors = BTreeSet::new();
        let mut child = handle;
        while let Some((&parent, _)) = self.pending_starts.iter().find(|(_, state)| {
            state.loaded.is_some_and(|(loaded, _)| loaded == child) || state.stopping == Some(child)
        }) {
            if parent == handle || !ancestors.insert(parent) {
                return Err("Cyclic pending module load".into());
            }
            child = parent;
        }
        let mut required = BTreeSet::new();
        let mut pending: Vec<_> = self
            .modules
            .get(&handle)
            .ok_or("Unknown module handle")?
            .dependencies
            .iter()
            .copied()
            .collect();
        for ancestor in &ancestors {
            let module = self
                .modules
                .get(ancestor)
                .ok_or("Missing suspended caller")?;
            pending.extend(
                module
                    .dependencies
                    .iter()
                    .copied()
                    .filter(|id| *id != handle && !ancestors.contains(id)),
            );
        }
        while let Some(id) = pending.pop() {
            if id == handle {
                return Err("Cyclic module execution dependency".into());
            }
            if ancestors.contains(&id) {
                continue;
            }
            if required.insert(id) {
                let provider = self
                    .modules
                    .get(&id)
                    .ok_or("Missing execution dependency")?;
                if provider.phase != Phase::Started {
                    return Err("Execution dependency has not started".into());
                }
                pending.extend(provider.dependencies.iter().copied());
            }
        }
        required.extend(ancestors.iter().copied());
        required.extend(
            self.thread_image_pins
                .iter()
                .copied()
                .filter(|id| *id != handle),
        );
        let mut selected = None;
        let mut dependencies = Vec::new();
        for (&id, module) in &mut self.modules {
            if id == handle {
                selected = module.image.as_mut();
            } else if required.contains(&id) {
                if let Some(image) = &mut module.image {
                    dependencies.push(image);
                }
            }
        }
        let mut data = Vec::new();
        if let Some(scheduler) = self.thread_scheduler.as_mut() {
            data.extend(scheduler.retained_stacks()?);
        }
        for region in &mut self.process_data {
            data.push(crate::guest_memory::DataRegion {
                address: region.address,
                bytes: &mut region.bytes,
                writable: region.writable,
            });
        }
        for state in self.pending_starts.values_mut() {
            if let Some(continuation) = state.continuation.as_mut() {
                data.push(continuation.stack_region());
            } else if let Some(request) = state.thread_request.as_mut() {
                data.push(request.stack_region()?);
            } else if let Some(wait) = state.mutex_wait.as_mut() {
                data.push(wait.stack_region()?);
            }
        }
        operation(
            selected.ok_or("Missing module image")?,
            &mut dependencies,
            &mut data,
        )
    }
    /// Starts constructors and the explicit status entry. A yielded request keeps
    /// its continuation owned by this manager, preventing duplicate/replayed resumes.
    pub fn start_native(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        arguments: Arguments,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        let module = self.modules.get(&handle).ok_or("Unknown module handle")?;
        if module.phase != Phase::Loaded || module.image.is_none() {
            return Err("Module cannot be started in its current state".into());
        }
        self.with_native_images(handle, |_, _, _| Ok(()))?;
        let module = self.modules.get_mut(&handle).unwrap();
        let mut calls: VecDeque<_> = module
            .lifecycle
            .initializers
            .iter()
            .map(|&address| (address, false))
            .collect();
        if let Some(address) = module.entries.start {
            calls.push_back((address, true));
        }
        // On errors or panic this phase pins the image and all its providers.
        module.phase = Phase::Faulted;
        self.advance_native(
            handle,
            executor,
            StartState {
                calls,
                arguments,
                status_return: false,
                continuation: None,
                scheduled_thread: None,
                thread_request: None,
                mutex_wait: None,
                request: None,
                loaded: None,
                stop_request: false,
                dlsym_request: false,
                sysmodule_request: false,
                stopping: None,
            },
            None,
        )
    }
    /// Continue exactly one pending service with the supplied guest return value.
    pub fn resume_native(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        result: u64,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(handle, executor)?;
        if self.pending_starts.get(&handle).is_some_and(|state| {
            state.mutex_wait.is_some()
                || state.thread_request.is_some()
                || state.scheduled_thread.is_some()
        }) {
            return Err("Mutex wait requires dedicated checked resume".into());
        }
        let mut state = self
            .pending_starts
            .remove(&handle)
            .ok_or("No pending module start")?;
        let continuation = state
            .continuation
            .take()
            .ok_or("Missing native continuation")?;
        state.request = None;
        state.loaded = None;
        state.stop_request = false;
        state.dlsym_request = false;
        state.sysmodule_request = false;
        let outcome = self.with_native_images(handle, |image, dependencies, data| {
            executor.resume_with_data(image, dependencies, *continuation, result, data)
        })?;
        self.advance_native(handle, executor, state, Some(outcome))
    }
    /// Recheck a blocked initializer's mutex; pending/error retains the request.
    pub fn resume_native_mutex(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
    ) -> Result<StartEvent> {
        self.require_native_running(executor)?;
        self.require_pending_thread(handle, executor)?;
        let mut state = self
            .pending_starts
            .remove(&handle)
            .ok_or("No pending module start")?;
        let result = self.with_native_images(handle, |image, dependencies, data| {
            executor.resume_mutex_with_data(
                image,
                dependencies,
                state.mutex_wait.as_mut().ok_or("No pending mutex wait")?,
                data,
            )
        });
        match result {
            Ok(Some(outcome)) => {
                state.mutex_wait = None;
                self.advance_native(handle, executor, state, Some(outcome))
            }
            other => {
                let event = state.mutex_wait.as_ref().map(|wait| StartEvent::MutexWait {
                    thread: wait.guest_thread_id(),
                    slot: wait.slot_address(),
                });
                self.pending_starts.insert(handle, state);
                match other {
                    Err(error) => Err(error),
                    Ok(None) => event.ok_or("No pending mutex wait".into()),
                    _ => unreachable!(),
                }
            }
        }
    }
    /// Checked host access while a service is pending. Failure retains the pending
    /// call so a runtime can report an error result, or explicitly abandon it.
    pub fn with_pending_memory<T>(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        operation: impl FnOnce(&mut crate::guest_memory::Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        let mut state = self
            .pending_starts
            .remove(&handle)
            .ok_or("No pending module start")?;
        let result = self.with_native_images(handle, |image, dependencies, data| {
            if let Some(request) = state.thread_request.as_mut() {
                executor.with_thread_request_memory(request, image, dependencies, data, operation)
            } else if let Some(wait) = state.mutex_wait.as_mut() {
                executor.with_mutex_wait_memory(wait, image, dependencies, data, operation)
            } else {
                let continuation = state
                    .continuation
                    .as_mut()
                    .ok_or("Missing native continuation")?;
                executor.with_suspended_memory(continuation, image, dependencies, data, operation)
            }
        });
        self.pending_starts.insert(handle, state);
        result
    }
    /// Discard suspended execution without making the partially initialized image
    /// unloadable or retryable. It remains fault-pinned until process teardown.
    pub fn abandon_native_start(&mut self, handle: u64) -> Result<()> {
        self.check_thread_pin(handle)?;
        self.pending_starts
            .remove(&handle)
            .ok_or("No pending module start")?;
        Ok(())
    }
    fn advance_native(
        &mut self,
        handle: u64,
        executor: &mut NativeExecutor,
        mut state: StartState,
        mut outcome: Option<Execution>,
    ) -> Result<StartEvent> {
        loop {
            if let Some(execution) = outcome.take() {
                match execution {
                    Execution::ThreadRequest(request) => {
                        let thread = request.guest_thread_id().ok_or("Thread request consumed")?;
                        state.thread_request = Some(request);
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::ThreadRequest { thread });
                    }
                    Execution::QuantumYield(_) => {
                        return Err("Worker quantum cannot complete module initialization".into());
                    }
                    Execution::WorkerReturned(_) => {
                        return Err("Worker return cannot complete module initialization".into());
                    }
                    Execution::MutexWait(wait) => {
                        let event = StartEvent::MutexWait {
                            thread: wait.guest_thread_id(),
                            slot: wait.slot_address(),
                        };
                        state.mutex_wait = Some(wait);
                        self.pending_starts.insert(handle, state);
                        return Ok(event);
                    }
                    Execution::ExitRequested(request) => {
                        self.exit_request = Some(request);
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::ExitRequested(request));
                    }
                    Execution::SysmoduleLoadInternal {
                        arguments,
                        continuation,
                    } => {
                        state.continuation = Some(continuation);
                        state.request = Some(arguments);
                        state.sysmodule_request = true;
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::SysmoduleLoadInternal { arguments });
                    }
                    Execution::Dlsym {
                        arguments,
                        continuation,
                    } => {
                        state.continuation = Some(continuation);
                        state.request = Some(arguments);
                        state.dlsym_request = true;
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::Dlsym { arguments });
                    }
                    Execution::StopUnloadModule {
                        arguments,
                        continuation,
                    } => {
                        state.continuation = Some(continuation);
                        state.request = Some(arguments);
                        state.stop_request = true;
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::StopUnloadModule { arguments });
                    }
                    Execution::Returned(status) => {
                        if state.status_return && status != 0 {
                            return Err(format!("Guest module returned status {status}"));
                        }
                    }
                    Execution::LoadStartModule {
                        arguments,
                        continuation,
                    } => {
                        state.continuation = Some(continuation);
                        state.request = Some(arguments);
                        self.pending_starts.insert(handle, state);
                        return Ok(StartEvent::LoadStartModule { arguments });
                    }
                }
            }
            let Some((address, status_return)) = state.calls.pop_front() else {
                self.modules
                    .get_mut(&handle)
                    .ok_or("Unknown module handle")?
                    .phase = Phase::Started;
                return Ok(StartEvent::Started);
            };
            state.status_return = status_return;
            let arguments = if status_return {
                state.arguments
            } else {
                Arguments::default()
            };
            outcome = Some(
                self.with_native_images(handle, |image, dependencies, data| {
                    executor.run_with_data(image, address, arguments, dependencies, data)
                })?,
            );
        }
    }
}

#[cfg(test)]
mod exit_request_tests {
    use super::*;
    fn state() -> StartState {
        StartState {
            calls: VecDeque::from([(0x200110, true)]),
            arguments: Arguments::default(),
            status_return: false,
            continuation: None,
            scheduled_thread: None,
            thread_request: None,
            mutex_wait: None,
            request: None,
            loaded: None,
            stop_request: false,
            dlsym_request: false,
            sysmodule_request: false,
            stopping: None,
        }
    }
    #[test]
    fn nested_initializer_exit_never_starts_child_or_resumes_parent() {
        let mut modules = Modules::new(65536);
        let parent = modules
            .load_dynamic("parent.prx", &fixture(), 0x100000, EntryPoints::default())
            .unwrap();
        let child = modules
            .load_dynamic("child.prx", &fixture(), 0x200000, EntryPoints::default())
            .unwrap();
        modules.modules.get_mut(&parent).unwrap().phase = Phase::Faulted;
        modules.modules.get_mut(&child).unwrap().phase = Phase::Faulted;
        let mut parent_state = state();
        parent_state.loaded = Some((child, 0));
        modules.pending_starts.insert(parent, parent_state);
        let mut executor = NativeExecutor::new(0x900000, 4096, 10, 10).unwrap();
        let request = ExitRequest {
            status: -1,
            kind: crate::module_executor::ExitKind::Normal,
        };
        // Simulate the typed outcome of an initializer. Its remaining start call
        // is deliberately unregistered: it must never be attempted after exit.
        let event = modules
            .advance_native(
                child,
                &mut executor,
                state(),
                Some(Execution::ExitRequested(request)),
            )
            .unwrap();
        assert!(matches!(event,StartEvent::ExitRequested(value) if value==request));
        assert_eq!(modules.exit_request(), Some(request));
        assert_eq!(modules.phase(child), Some(Phase::Faulted));
        assert_eq!(modules.phase(parent), Some(Phase::Faulted));
        assert!(modules.pending_starts.contains_key(&parent));
        assert!(
            modules
                .finish_native_load(parent, &mut executor)
                .err()
                .unwrap()
                .contains("exit requested")
        );
        assert!(
            modules
                .resume_native(parent, &mut executor, 0)
                .err()
                .unwrap()
                .contains("exit requested")
        );
        let other = modules
            .load_dynamic("other.prx", &fixture(), 0x300000, EntryPoints::default())
            .unwrap();
        assert!(
            modules
                .start_native(other, &mut executor, Arguments::default())
                .err()
                .unwrap()
                .contains("exit requested")
        );
        assert_eq!(modules.phase(other), Some(Phase::Loaded));
    }
}
