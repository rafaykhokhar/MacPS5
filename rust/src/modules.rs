//! Owned module images, dependency pins and serialized start/stop transitions.
//! The guest-call executor supplies actual CPU execution; no success is fabricated.
use crate::{Result, loader::Image, registry::Registry};
use std::collections::{BTreeMap, BTreeSet};
#[path = "modules_native.rs"]
mod native;
#[path = "sysmodule.rs"]
mod sysmodule;
pub use native::{
    Bootstrap, LoadConfig, LoadPlan, LoadProgress, ProcessData, StartEvent, StartOutcome,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Loaded,
    Started,
    Stopped,
    Faulted,
}
#[derive(Clone, Copy, Default)]
pub struct EntryPoints {
    pub start: Option<u64>,
    pub stop: Option<u64>,
}
#[derive(Clone, Copy, Default)]
pub struct Arguments {
    pub size: u64,
    pub pointer: u64,
}
#[derive(Debug)]
pub struct LoadStartError {
    /// Present when loading committed; callers must retain the module even if
    /// initialization failed, because guest code may have retained pointers.
    pub handle: Option<u64>,
    pub message: String,
}
pub trait Executor {
    /// Reject stop/unload while executor-owned waits borrow this image's data.
    /// Synchronous executors that retain no guest state need no extra checks.
    fn validate_image_retirement(&self, _image: &Image) -> Result<()> {
        Ok(())
    }
    /// Legacy synchronous adapters expose termination separately from their
    /// i32 status result, so process exit cannot become module-start success.
    fn exit_request(&self) -> Option<crate::module_executor::ExitRequest> {
        None
    }
    fn call_with_dependencies(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
    ) -> Result<i32> {
        if !dependencies.is_empty() {
            return Err("Executor cannot map guest dependencies".into());
        }
        self.call(image, address, arguments)
    }
    fn initialize_with_dependencies(
        &mut self,
        image: &mut Image,
        address: u64,
        dependencies: &mut [&mut Image],
    ) -> Result<()> {
        if !dependencies.is_empty() {
            return Err("Executor cannot map guest dependencies".into());
        }
        self.initialize(image, address)
    }
    /// Run the guest entry until its matching return. Faults, budget exhaustion,
    /// unsupported code and incomplete calls must return Err, never a guessed status.
    /// Must not retain references to image storage after returning.
    fn call(&mut self, image: &mut Image, address: u64, arguments: Arguments) -> Result<i32>;
    /// Run a void constructor/finalizer with the executor's process startup context.
    /// Its return register is not a module status. Unsupported execution must fail.
    fn initialize(&mut self, _image: &mut Image, _address: u64) -> Result<()> {
        Err("Constructor execution is not configured".into())
    }
}
struct Module {
    name: String,
    image: Option<Image>,
    entries: EntryPoints,
    lifecycle: crate::lifecycle::Lifecycle,
    dependencies: BTreeSet<u64>,
    phase: Phase,
    bytes: usize,
}
pub struct Modules {
    thread_image_pins: BTreeSet<u64>,
    thread_scheduler: Option<crate::scheduler::Scheduler>,
    exit_request: Option<crate::module_executor::ExitRequest>,
    native_paths: BTreeMap<String, u64>,
    sysmodules: BTreeMap<u32, (u64, u32)>,
    bootstrap: Option<u64>,
    process_data: Vec<ProcessData>,
    dynamic_loads: BTreeSet<(u64, u64)>,
    pending_starts: BTreeMap<u64, native::StartState>,
    registry: Registry,
    modules: BTreeMap<u64, Module>,
    names: BTreeMap<String, u64>,
    next: u64,
    bytes: usize,
    budget: usize,
}
impl Modules {
    pub fn load_file(
        &mut self,
        files: &mut crate::module_files::ModuleFiles,
        path: &str,
        bias: u64,
    ) -> Result<u64> {
        let input = files.read(path, self.budget - self.bytes)?;
        self.load_dynamic(&input.name, &input.bytes, bias, input.entries)
    }
    pub fn load_start_file(
        &mut self,
        files: &mut crate::module_files::ModuleFiles,
        path: &str,
        bias: u64,
        executor: &mut impl Executor,
        arguments: Arguments,
    ) -> std::result::Result<u64, LoadStartError> {
        let handle = self
            .load_file(files, path, bias)
            .map_err(|message| LoadStartError {
                handle: None,
                message,
            })?;
        self.start(handle, executor, arguments)
            .map_err(|message| LoadStartError {
                handle: Some(handle),
                message,
            })?;
        Ok(handle)
    }
    pub fn new(budget: usize) -> Self {
        Self {
            thread_image_pins: BTreeSet::new(),
            thread_scheduler: None,
            exit_request: None,
            native_paths: BTreeMap::new(),
            sysmodules: BTreeMap::new(),
            bootstrap: None,
            process_data: Vec::new(),
            dynamic_loads: BTreeSet::new(),
            pending_starts: BTreeMap::new(),
            registry: Registry::default(),
            modules: BTreeMap::new(),
            names: BTreeMap::new(),
            next: 1,
            bytes: 0,
            budget,
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn phase(&self, handle: u64) -> Option<Phase> {
        self.modules.get(&handle).map(|m| m.phase)
    }
    pub fn image(&self, handle: u64) -> Option<&Image> {
        self.modules.get(&handle)?.image.as_ref()
    }
    pub fn resolve(&self, identity: &crate::imports::Identity) -> Option<u64> {
        self.registry.resolve(identity)
    }
    fn available(&self, name: &str) -> Result<u64> {
        if self.names.contains_key(name) {
            return Err("Module is already loaded".into());
        }
        if self.modules.len() >= 128 {
            return Err("Module count budget exhausted".into());
        }
        self.next
            .checked_add(1)
            .ok_or("Module handle space exhausted")?;
        Ok(self.next)
    }
    fn commit(&mut self, handle: u64, module: Module, registry: Registry) {
        self.bytes += module.bytes;
        self.names.insert(module.name.clone(), handle);
        self.modules.insert(handle, module);
        self.registry = registry;
        self.next = handle + 1;
    }
    /// Native bridge modules must already have working services behind their exports.
    /// Associate a guest pathname with an already initialized native provider.
    /// This is host configuration, never a host-filesystem lookup.
    pub fn bind_native_path(&mut self, path: &str, handle: u64) -> Result<()> {
        if !crate::module_files::valid_path(path)
            || self.native_paths.contains_key(path)
            || self.native_paths.len() >= 128
            || !self
                .modules
                .get(&handle)
                .is_some_and(|m| m.image.is_none() && m.phase == Phase::Started)
        {
            return Err("Invalid native module path binding".into());
        }
        self.native_paths.insert(path.into(), handle);
        Ok(())
    }
    /// Native bridge modules must already have working services behind their exports.
    pub fn register_native(&mut self, name: &str, exports: &[(&str, u64)]) -> Result<u64> {
        let handle = self.available(name)?;
        let mut registry = self.registry.clone();
        registry.register_module(name, exports)?;
        self.commit(
            handle,
            Module {
                name: name.into(),
                image: None,
                entries: EntryPoints::default(),
                lifecycle: crate::lifecycle::Lifecycle::default(),
                dependencies: BTreeSet::new(),
                phase: Phase::Started,
                bytes: 0,
            },
            registry,
        );
        Ok(handle)
    }
    /// Export offsets and entry offsets are explicit loader descriptors for now.
    /// Automatic extraction of ELF lifecycle/export tables remains a separate loader step.
    pub fn load(
        &mut self,
        name: &str,
        file: &[u8],
        bias: u64,
        exports: &[(&str, u64)],
        entries: EntryPoints,
    ) -> Result<u64> {
        self.load_impl(name, file, bias, Some(exports), entries)
    }
    /// Discover exports from bounded dynamic symbol metadata instead of explicit descriptors.
    pub fn load_dynamic(
        &mut self,
        name: &str,
        file: &[u8],
        bias: u64,
        entries: EntryPoints,
    ) -> Result<u64> {
        self.load_impl(name, file, bias, None, entries)
    }
    fn load_impl(
        &mut self,
        name: &str,
        file: &[u8],
        bias: u64,
        exports: Option<&[(&str, u64)]>,
        entries: EntryPoints,
    ) -> Result<u64> {
        let handle = self.available(name)?;
        let mut registry = self.registry.clone();
        registry.register_module(name, &[])?;
        let mut image = Image::load(file, bias, self.budget - self.bytes)?;
        for new in &image.segments {
            for old in self
                .modules
                .values()
                .filter_map(|m| m.image.as_ref())
                .flat_map(|i| &i.segments)
            {
                if new.address < old.address + old.bytes.len() as u64
                    && old.address < new.address + new.bytes.len() as u64
                {
                    return Err("Module guest address ranges overlap".into());
                }
            }
        }
        let mut owners = BTreeSet::new();
        image.relocate_scoped(|identity| {
            self.registry
                .resolve_owned(identity)
                .map(|(owner, address)| {
                    owners.insert(owner.to_owned());
                    address
                })
        })?;
        if !image.unresolved.is_empty() {
            return Err("Module imports remain unresolved".into());
        }
        let lifecycle = image.lifecycle()?;
        let dependencies = image
            .dependencies
            .iter()
            .chain(owners.iter())
            .map(|name| {
                self.names
                    .get(name)
                    .copied()
                    .ok_or_else(|| format!("Missing module dependency: {name}"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        let entry = |offset: Option<u64>| -> Result<Option<u64>> {
            offset
                .map(|offset| {
                    let address = bias
                        .checked_add(offset)
                        .ok_or("Module entry address overflow")?;
                    if !image.segments.iter().any(|s| {
                        s.flags & 1 != 0
                            && address >= s.address
                            && address - s.address < s.bytes.len() as u64
                    }) {
                        return Err("Module entry is not executable guest memory".into());
                    }
                    Ok(address)
                })
                .transpose()
        };
        let entries = EntryPoints {
            start: entry(entries.start)?,
            stop: entry(entries.stop)?,
        };
        if let Some(exports) = exports {
            let exports =
                exports
                    .iter()
                    .map(|&(name, offset)| {
                        let address = bias
                            .checked_add(offset)
                            .ok_or("Module export address overflow")?;
                        if !image.segments.iter().any(|s| {
                            address >= s.address && address - s.address < s.bytes.len() as u64
                        }) {
                            return Err("Module export outside image".into());
                        }
                        Ok((name, address))
                    })
                    .collect::<Result<Vec<_>>>()?;
            registry.register_module(name, &exports)?;
        } else {
            registry.register_image(name, &image.exported_symbols()?)?;
        }
        let bytes = file.len() + image.segments.iter().map(|s| s.bytes.len()).sum::<usize>();
        self.commit(
            handle,
            Module {
                name: name.into(),
                image: Some(image),
                entries,
                lifecycle,
                dependencies,
                phase: Phase::Loaded,
                bytes,
            },
            registry,
        );
        Ok(handle)
    }
    pub fn start(
        &mut self,
        handle: u64,
        executor: &mut impl Executor,
        arguments: Arguments,
    ) -> Result<()> {
        let module = self.modules.get(&handle).ok_or("Unknown module handle")?;
        if module.phase != Phase::Loaded {
            return Err("Module cannot be started in its current state".into());
        }
        for dependency in &module.dependencies {
            if self.phase(*dependency) != Some(Phase::Started) {
                return Err("Module dependency has not started".into());
            }
        }
        self.transition(handle, true, executor, arguments)
    }
    pub fn stop(
        &mut self,
        handle: u64,
        executor: &mut impl Executor,
        arguments: Arguments,
    ) -> Result<()> {
        self.check_thread_pin(handle)?;
        self.check_sysmodule_pin(handle)?;
        let module = self.modules.get(&handle).ok_or("Unknown module handle")?;
        if module.phase != Phase::Started || module.image.is_none() {
            return Err("Module cannot be stopped in its current state".into());
        }
        executor.validate_image_retirement(module.image.as_ref().unwrap())?;
        if self.modules.values().any(|m| {
            matches!(m.phase, Phase::Started | Phase::Faulted) && m.dependencies.contains(&handle)
        }) {
            return Err("Active or faulted dependent pins module".into());
        }
        self.transition(handle, false, executor, arguments)
    }
    fn transition(
        &mut self,
        handle: u64,
        start: bool,
        executor: &mut impl Executor,
        arguments: Arguments,
    ) -> Result<()> {
        if self.exit_request.is_none() {
            self.exit_request = executor.exit_request();
        }
        if self.exit_request.is_some() {
            return Err(
                "Guest exit requested; module transitions prohibited while cleanup is pending"
                    .into(),
            );
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
        while let Some(dependency) = pending.pop() {
            if dependency == handle {
                return Err("Cyclic module execution dependency".into());
            }
            if required.insert(dependency) {
                let provider = self
                    .modules
                    .get(&dependency)
                    .ok_or("Missing execution dependency")?;
                if provider.phase != Phase::Started {
                    return Err("Execution dependency has not started".into());
                }
                pending.extend(provider.dependencies.iter().copied());
            }
        }
        let mut selected = None;
        let mut dependencies = Vec::new();
        for (&id, module) in &mut self.modules {
            if id == handle {
                selected = Some(module);
            } else if required.contains(&id) {
                if let Some(image) = &mut module.image {
                    dependencies.push(image);
                }
            }
        }
        let module = selected.ok_or("Unknown module handle")?;
        let entry = if start {
            module.entries.start
        } else {
            module.entries.stop
        };
        // A panicking executor must not leave an apparently retryable image.
        module.phase = Phase::Faulted;
        if start {
            for &address in &module.lifecycle.initializers {
                let initialized = executor.initialize_with_dependencies(
                    module.image.as_mut().ok_or("Missing module image")?,
                    address,
                    &mut dependencies,
                );
                if let Err(error) = initialized {
                    self.exit_request = executor.exit_request();
                    return Err(error);
                }
            }
        }
        let outcome = match entry {
            Some(address) => executor.call_with_dependencies(
                module
                    .image
                    .as_mut()
                    .ok_or("Native module has no guest image")?,
                address,
                arguments,
                &mut dependencies,
            ),
            None => Ok(0),
        };
        match outcome {
            Ok(0) => {
                if !start {
                    for &address in &module.lifecycle.finalizers {
                        let finalized = executor.initialize_with_dependencies(
                            module.image.as_mut().ok_or("Missing module image")?,
                            address,
                            &mut dependencies,
                        );
                        if let Err(error) = finalized {
                            self.exit_request = executor.exit_request();
                            return Err(error);
                        }
                    }
                }
                module.phase = if start {
                    Phase::Started
                } else {
                    Phase::Stopped
                };
                Ok(())
            }
            Ok(status) => {
                module.phase = Phase::Faulted;
                Err(format!("Guest module returned status {status}"))
            }
            Err(error) => {
                module.phase = Phase::Faulted;
                self.exit_request = executor.exit_request();
                Err(error)
            }
        }
    }
    /// No guest code may still be executing or retaining untracked pointers.
    /// Faulted modules stay pinned until process teardown; arbitrary retry is unsafe.
    /// Native callers must use unload_native to revoke executor translations too.
    pub fn unload(&mut self, handle: u64) -> Result<()> {
        self.validate_unload(handle)?;
        self.commit_unload(handle);
        Ok(())
    }
    fn validate_unload(&self, handle: u64) -> Result<()> {
        self.check_thread_pin(handle)?;
        self.check_sysmodule_pin(handle)?;
        if self.bootstrap == Some(handle) {
            return Err("Bootstrap root remains owned until process teardown".into());
        }
        let module = self.modules.get(&handle).ok_or("Unknown module handle")?;
        if !matches!(module.phase, Phase::Loaded | Phase::Stopped) {
            return Err("Module must stop before unload".into());
        }
        if self
            .modules
            .values()
            .any(|m| m.dependencies.contains(&handle))
        {
            return Err("Dependent module pins image".into());
        }
        Ok(())
    }
    /// All fallible checks precede this mutation; the manager is exclusively borrowed.
    fn commit_unload(&mut self, handle: u64) {
        let module = self
            .modules
            .remove(&handle)
            .expect("Preflighted unload handle");
        self.bytes -= module.bytes;
        self.sysmodules.retain(|_, (owner, _)| *owner != handle);
        self.native_paths.retain(|_, owner| *owner != handle);
        self.names.remove(&module.name);
        self.registry.remove_module(&module.name);
        self.dynamic_loads
            .retain(|&(caller, child)| caller != handle && child != handle);
    }
}
/// Minimal synthetic module used for lifecycle execution tests; not a game image.
pub fn fixture() -> Vec<u8> {
    let mut b = vec![0; 0x200];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (p, v) in [(16, 3u16), (18, 62), (52, 64), (54, 56), (56, 1)] {
        b[p..p + 2].copy_from_slice(&v.to_le_bytes());
    }
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    for (p, v) in [
        (24, 0x100u64),
        (32, 64),
        (64, 0x500000001),
        (72, 0),
        (80, 0),
        (96, 0x200),
        (104, 0x200),
        (112, 0x1000),
    ] {
        b[p..p + 8].copy_from_slice(&v.to_le_bytes());
    }
    b[0x100..0x103].copy_from_slice(&[0x89, 0xf8, 0xc3]);
    b[0x110..0x113].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    b
}
/// Synthetic module with relocated initializer/finalizer arrays and dynamic entries.
pub fn lifecycle_fixture(sce: bool) -> Vec<u8> {
    let mut b = fixture();
    b.resize(0x400, 0);
    fn put(b: &mut [u8], p: usize, v: u64) {
        b[p..p + 8].copy_from_slice(&v.to_le_bytes());
    }
    b[56..58].copy_from_slice(&2u16.to_le_bytes());
    put(&mut b, 96, 0x400);
    put(&mut b, 104, 0x400);
    for (i, value) in [0x400000002, 0x180, 0x180, 0, 160, 160, 8]
        .iter()
        .enumerate()
    {
        put(&mut b, 120 + i * 8, *value);
    }
    for (offset, value) in [(0x120, 7u32), (0x130, 8), (0x140, 9), (0x150, 10)] {
        b[offset] = 0xb8;
        b[offset + 1..offset + 5].copy_from_slice(&value.to_le_bytes());
        b[offset + 5] = 0xc3;
    }
    let tags = [
        (12, 0x120),
        (13, 0x140),
        (25, 0x240),
        (27, 8),
        (26, 0x250),
        (28, 8),
        (7, 0x280),
        (8, 48),
        (9, 24),
        (0, 0),
    ];
    for (i, (tag, value)) in tags.iter().enumerate() {
        let tag = if sce && [12, 13, 25, 26, 27, 28].contains(tag) {
            0x60000000 + tag
        } else {
            *tag
        };
        put(&mut b, 0x180 + i * 16, tag);
        put(&mut b, 0x188 + i * 16, *value);
    }
    for (p, v) in [
        (0x280, 0x240),
        (0x288, 8),
        (0x290, 0x130),
        (0x298, 0x250),
        (0x2a0, 8),
        (0x2a8, 0x150),
    ] {
        put(&mut b, p, v);
    }
    b
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_path_bindings_are_explicit_and_preserve_host_ownership() {
        let mut modules = super::Modules::new(65536);
        assert!(
            modules
                .bind_native_path("/system/provider.sprx", 99)
                .is_err()
        );
        let provider = modules
            .register_native("provider.prx", &[("service", 0x700000)])
            .unwrap();
        for path in [
            "provider.sprx",
            "/system/../provider.sprx",
            "/system//provider.sprx",
        ] {
            assert!(modules.bind_native_path(path, provider).is_err());
        }
        modules
            .bind_native_path("/system/provider.sprx", provider)
            .unwrap();
        assert!(
            modules
                .bind_native_path("/system/provider.sprx", provider)
                .is_err()
        );
        assert!(modules.unload(provider).is_err());
        assert_eq!(
            modules.native_paths.get("/system/provider.sprx"),
            Some(&provider)
        );
        assert!(
            modules
                .bind_native_path("/system/provider.sprx", provider)
                .is_err()
        );
    }
    use super::*;
    struct Exec {
        calls: Vec<u64>,
        result: Result<i32>,
    }
    impl Executor for Exec {
        fn call(&mut self, _: &mut Image, address: u64, _: Arguments) -> Result<i32> {
            self.calls.push(address);
            self.result.clone()
        }
    }
    #[test]
    fn legacy_executor_cannot_silently_ignore_dependency_images() {
        let mut primary = Image::load(&fixture(), 0x10000, 4096).unwrap();
        let mut provider = Image::load(&fixture(), 0x20000, 4096).unwrap();
        let mut executor = Exec {
            calls: vec![],
            result: Ok(0),
        };
        assert!(
            executor
                .call_with_dependencies(
                    &mut primary,
                    0x10100,
                    Arguments::default(),
                    &mut [&mut provider]
                )
                .is_err()
        );
        assert!(
            executor
                .initialize_with_dependencies(&mut primary, 0x10100, &mut [&mut provider])
                .is_err()
        );
        assert!(executor.calls.is_empty());
        assert_eq!(
            executor
                .call_with_dependencies(&mut primary, 0x10100, Arguments::default(), &mut [])
                .unwrap(),
            0
        );
        assert_eq!(executor.calls, [0x10100]);
    }
    fn entries() -> EntryPoints {
        EntryPoints {
            start: Some(0x100),
            stop: Some(0x110),
        }
    }
    #[test]
    fn lifecycle_budgets_ownership_and_stale_handles() {
        let mut modules = Modules::new(4096);
        let file = fixture();
        let id = modules
            .load("one.prx", &file, 0x10000, &[("hello", 0x100)], entries())
            .unwrap();
        assert_eq!(modules.bytes(), 1024);
        assert!(
            modules
                .load("overlap.prx", &file, 0x10000, &[], entries())
                .is_err()
        );
        assert_eq!(modules.bytes(), 1024);
        let mut exec = Exec {
            calls: vec![],
            result: Ok(0),
        };
        assert!(modules.stop(id, &mut exec, Arguments::default()).is_err());
        modules.start(id, &mut exec, Arguments::default()).unwrap();
        assert!(modules.unload(id).is_err());
        assert!(modules.start(id, &mut exec, Arguments::default()).is_err());
        modules.stop(id, &mut exec, Arguments::default()).unwrap();
        assert_eq!(exec.calls, [0x10100, 0x10110]);
        modules.unload(id).unwrap();
        assert_eq!(modules.bytes(), 0);
        assert!(modules.start(id, &mut exec, Arguments::default()).is_err());
        let next = modules
            .load("one.prx", &file, 0x10000, &[], entries())
            .unwrap();
        assert_ne!(id, next);
        assert!(
            Modules::new(1000)
                .load("x.prx", &file, 0x10000, &[], entries())
                .is_err()
        );
    }
    #[test]
    fn sysmodule_references_require_started_provider_and_pin_lifecycle() {
        let mut modules = Modules::new(4096);
        let id = modules
            .load(
                "fiber.prx",
                &fixture(),
                0x10000,
                &[("entry", 0x100)],
                entries(),
            )
            .unwrap();
        assert!(modules.bind_sysmodule(6, id).is_err());
        assert!(modules.retain_sysmodule(6).is_err());
        assert!(!modules.sysmodule_loaded(6));
        let mut exec = Exec {
            calls: vec![],
            result: Ok(0),
        };
        modules.start(id, &mut exec, Arguments::default()).unwrap();
        for invalid in [0, 0x80000000] {
            assert!(modules.bind_sysmodule(invalid, id).is_err());
        }
        modules.bind_sysmodule(6, id).unwrap();
        modules.bind_sysmodule(0x80000006, id).unwrap();
        assert!(modules.bind_sysmodule(6, id).is_err());
        assert!(!modules.sysmodule_loaded(6));
        assert!(modules.release_sysmodule(6).is_err());
        assert_eq!(modules.retain_sysmodule(6).unwrap(), id);
        modules.retain_sysmodule(6).unwrap();
        modules.retain_sysmodule(0x80000006).unwrap();
        assert!(modules.sysmodule_loaded(6));
        assert!(modules.stop(id, &mut exec, Arguments::default()).is_err());
        assert!(modules.unload(id).is_err());
        modules.release_sysmodule(6).unwrap();
        modules.release_sysmodule(6).unwrap();
        assert!(!modules.sysmodule_loaded(6));
        assert!(modules.stop(id, &mut exec, Arguments::default()).is_err());
        modules.release_sysmodule(0x80000006).unwrap();
        modules.stop(id, &mut exec, Arguments::default()).unwrap();
        assert!(modules.retain_sysmodule(6).is_err());
        modules.unload(id).unwrap();
        assert!(modules.retain_sysmodule(6).is_err());
        assert!(modules.release_sysmodule(0x80000006).is_err());
    }
    #[test]
    fn executor_panic_never_leaves_retryable_loaded_state() {
        struct Panics;
        impl Executor for Panics {
            fn call(&mut self, _: &mut Image, _: u64, _: Arguments) -> Result<i32> {
                panic!("abandoned guest call")
            }
        }
        let mut modules = Modules::new(4096);
        let id = modules
            .load("panic.prx", &fixture(), 0x10000, &[], entries())
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            modules.start(id, &mut Panics, Arguments::default())
        }));
        assert!(result.is_err());
        assert_eq!(modules.phase(id), Some(Phase::Faulted));
        assert!(modules.unload(id).is_err());
    }
    #[test]
    fn faults_do_not_claim_start_success_or_release_image() {
        for result in [Ok(7), Err("Guest execution fault".into())] {
            let mut modules = Modules::new(4096);
            let id = modules
                .load("bad.prx", &fixture(), 0x10000, &[], entries())
                .unwrap();
            let mut exec = Exec {
                calls: vec![],
                result,
            };
            assert!(modules.start(id, &mut exec, Arguments::default()).is_err());
            assert_eq!(modules.phase(id), Some(Phase::Faulted));
            assert!(modules.unload(id).is_err());
            assert_eq!(modules.bytes(), 1024);
        }
    }
    #[test]
    fn import_providers_pin_lifecycle_even_without_needed_tag() {
        let mut modules = Modules::new(65536);
        let provider = modules
            .load(
                "libSceAgcDriver.prx",
                &fixture(),
                0x20000,
                &[("sceAgcDriverSubmitDcb", 0x100)],
                EntryPoints::default(),
            )
            .unwrap();
        let mut file = crate::loader_fixture::graphics(false);
        // Replace DT_NEEDED with DT_DEBUG: dependency must come from resolved ownership.
        file[0x1f0..0x1f8].copy_from_slice(&21u64.to_le_bytes());
        let consumer = modules
            .load("consumer.prx", &file, 0x30000, &[], EntryPoints::default())
            .unwrap();
        let mut exec = Exec {
            calls: vec![],
            result: Ok(0),
        };
        assert!(modules.unload(provider).is_err());
        assert!(
            modules
                .start(consumer, &mut exec, Arguments::default())
                .is_err()
        );
        modules
            .start(provider, &mut exec, Arguments::default())
            .unwrap();
        modules
            .start(consumer, &mut exec, Arguments::default())
            .unwrap();
        assert!(
            modules
                .stop(provider, &mut exec, Arguments::default())
                .is_err()
        );
        modules
            .stop(consumer, &mut exec, Arguments::default())
            .unwrap();
        modules
            .stop(provider, &mut exec, Arguments::default())
            .unwrap();
        assert!(modules.unload(provider).is_err());
        modules.unload(consumer).unwrap();
        modules.unload(provider).unwrap();
        assert_eq!(modules.bytes(), 0);
        let identity =
            crate::imports::identify("sceAgcDriverSubmitDcb", &BTreeMap::new(), &[]).unwrap();
        assert_eq!(modules.resolve(&identity), None);
    }
    #[test]
    fn semaphore_wait_prevents_image_retirement_before_phase_or_code_changes() {
        use crate::{guest_memory::Memory, guest_semaphore::Outcome, module_executor::NativeExecutor};
        let mut file = fixture();
        file[68..72].copy_from_slice(&7u32.to_le_bytes());
        let mut modules = Modules::new(4096);
        let handle = modules.load("semaphore.prx", &file, 0x10000, &[], entries()).unwrap();
        let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000).unwrap();
        let ticket = {
            let image = modules.modules.get_mut(&handle).unwrap().image.as_mut().unwrap();
            let segment = &mut image.segments[0];
            let mut memory = Memory::default();
            memory.add(segment.address, &mut segment.bytes, true, false).unwrap();
            let plan = native.semaphores.init(&memory, 0x10180, 0, 0).unwrap();
            native.semaphores.commit(&mut memory, plan).unwrap();
            let plan = native.semaphores.wait(&memory, 0x10180, 1, 0,
                || Err("Untimed wait must not read realtime".into())).unwrap();
            let Outcome::Pending(ticket) = native.semaphores.commit(&mut memory, plan).unwrap() else {
                panic!("Expected pending semaphore wait");
            };
            ticket
        };
        let bytes = modules.bytes();
        assert!(modules.unload_native(handle, &mut native).is_err());
        assert_eq!(modules.phase(handle), Some(Phase::Loaded));
        assert_eq!(modules.bytes(), bytes);
        modules.modules.get_mut(&handle).unwrap().phase = Phase::Started;
        assert!(modules.stop(handle, &mut native, Arguments::default()).is_err());
        assert_eq!(modules.phase(handle), Some(Phase::Started));
        native.semaphores.abandon(&ticket, 1).unwrap();
        modules.modules.get_mut(&handle).unwrap().phase = Phase::Stopped;
        modules.unload_native(handle, &mut native).unwrap();
        assert_eq!(modules.bytes(), 0);
    }
    #[test]
    fn failed_stop_and_aggregate_budget_preserve_ownership() {
        let mut modules = Modules::new(2048);
        let file = fixture();
        let a = modules
            .load("a.prx", &file, 0x10000, &[], entries())
            .unwrap();
        let b = modules
            .load("b.prx", &file, 0x20000, &[], entries())
            .unwrap();
        assert_eq!(modules.bytes(), 2048);
        assert!(
            modules
                .load("c.prx", &file, 0x30000, &[], entries())
                .is_err()
        );
        let mut exec = Exec {
            calls: vec![],
            result: Ok(0),
        };
        modules.start(a, &mut exec, Arguments::default()).unwrap();
        exec.result = Err("Guest stop fault".into());
        assert!(modules.stop(a, &mut exec, Arguments::default()).is_err());
        assert_eq!(modules.phase(a), Some(Phase::Faulted));
        assert!(modules.unload(a).is_err());
        modules.unload(b).unwrap();
        assert_eq!(modules.bytes(), 1024);
        modules
            .load("c.prx", &file, 0x30000, &[], entries())
            .unwrap();
    }
    #[test]
    fn unresolved_or_invalid_loads_publish_nothing() {
        let mut modules = Modules::new(65536);
        assert!(
            modules
                .load(
                    "graphics.prx",
                    &crate::loader_fixture::graphics(true),
                    0x10000,
                    &[],
                    EntryPoints::default()
                )
                .is_err()
        );
        assert_eq!(modules.bytes(), 0);
        for entries in [
            EntryPoints {
                start: Some(u64::MAX),
                stop: None,
            },
            EntryPoints {
                start: Some(0x200),
                stop: None,
            },
        ] {
            assert!(
                modules
                    .load("x.prx", &fixture(), 0x10000, &[], entries)
                    .is_err()
            );
        }
        assert!(
            modules
                .load("x.prx", &fixture(), 0x10000, &[("bad", 0x200)], entries())
                .is_err()
        );
        assert_eq!(modules.bytes(), 0);
        modules
            .register_native("libSceAgcDriver.prx", &[("sceAgcDriverSubmitDcb", 0x1234)])
            .unwrap();
        let id = modules
            .load(
                "graphics.prx",
                &crate::loader_fixture::graphics(true),
                0x10000,
                &[],
                EntryPoints::default(),
            )
            .unwrap();
        assert_eq!(modules.image(id).unwrap().relocations, [(0x14000, 0x1234)]);
    }
}
