//! Native module compilation. Generated libraries own their code until all
//! registered blocks release them; guest bytes are never executed directly.
#![allow(unsafe_code)]
use crate::{
    Result, aot,
    discover::{Discovery, Manifest},
    elf::Elf,
    legacy_cpu::Cpu,
};
use std::{
    collections::BTreeSet,
    ffi::{CString, c_void},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};
// Native static archives do not carry rustc's framework-link metadata into
// clang consumers. CPU and GPU helpers can share archive codegen objects.
pub(crate) const RUNTIME_LINK_ARGS: &[&str] = &[
    "-framework",
    "GameController",
    "-framework",
    "AudioToolbox",
    "-framework",
    "AppKit",
    "-framework",
    "QuartzCore",
    "-framework",
    "CoreFoundation",
    "-liconv",
    "-lobjc",
    "-framework",
    "Metal",
    "-framework",
    "Foundation",
];
unsafe extern "C" {
    fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
    fn dlclose(handle: *mut c_void) -> i32;
}
pub(crate) struct Library {
    handle: *mut c_void,
    directory: PathBuf,
}
impl Drop for Library {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                dlclose(self.handle);
            }
        }
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
pub(crate) struct Entry {
    pub pc: u64,
    pub start: u64,
    pub bytes: Rc<[u8]>,
    pub function: unsafe extern "C" fn(*mut Cpu),
}
pub(crate) struct Compiled {
    pub library: Rc<Library>,
    pub entries: Vec<Entry>,
    pub manifest: Manifest,
}
pub(crate) fn compile(
    file: &[u8],
    roots: BTreeSet<u64>,
    limit: usize,
    archive: &Path,
    parent: &Path,
) -> Result<Compiled> {
    compile_incremental(file, roots, limit, archive, parent, &BTreeSet::new())
}

/// Full discovery still validates overlap and the cumulative instruction limit.
/// Existing instruction PCs are boundaries, never emitted into a new library.
pub(crate) fn compile_incremental(
    file: &[u8],
    roots: BTreeSet<u64>,
    limit: usize,
    archive: &Path,
    parent: &Path,
    existing: &BTreeSet<u64>,
) -> Result<Compiled> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("Module compilation requires Apple Silicon macOS".into());
    }
    let elf = Elf::parse(file)?;
    let discovery = Discovery::run(&elf, roots, limit)?;
    let manifest = union_manifest(&discovery, file);
    let discovery = incremental_discovery(discovery, existing)?;
    let artifacts = discovery.emit_with_trace(&elf, file, "module_aot", &BTreeSet::new())?;
    if artifacts.manifest.blocks.is_empty() {
        return Err("Module has no translatable blocks".into());
    }
    let assembly = aot::rust_memory_helpers(artifacts.assembly)?;
    let archive = archive
        .canonicalize()
        .map_err(|e| format!("Missing Rust runtime archive: {e}"))?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let directory = parent.join(format!(
        "module-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
    let mut library = Library {
        handle: std::ptr::null_mut(),
        directory,
    };
    let source = library.directory.join("module.S");
    let output = library.directory.join("module.dylib");
    std::fs::write(&source, assembly).map_err(|e| e.to_string())?;
    let build = Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-dynamiclib"])
        .arg(&source)
        .arg(&archive)
        .args(RUNTIME_LINK_ARGS)
        .arg("-o")
        .arg(&output)
        .output()
        .map_err(|e| e.to_string())?;
    if !build.status.success() {
        return Err(format!(
            "Module compiler failed: {}",
            String::from_utf8_lossy(&build.stderr)
        ));
    }
    let path = CString::new(
        output
            .canonicalize()
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes(),
    )
    .map_err(|_| "Invalid module output path")?;
    // macOS RTLD_NOW | RTLD_LOCAL. Input is exclusively generated ARM64, linked
    // against the matching trusted Rust runtime archive supplied by the host.
    library.handle = unsafe { dlopen(path.as_ptr(), 2 | 4) };
    if library.handle.is_null() {
        return Err("Cannot load compiled ARM64 module library".into());
    }
    let mut entries = Vec::new();
    for block in &artifacts.manifest.blocks {
        let symbol = CString::new(format!("module_aot_{:x}", block.start))
            .map_err(|_| "Invalid generated symbol")?;
        let pointer = unsafe { dlsym(library.handle, symbol.as_ptr()) };
        if pointer.is_null() {
            return Err("Compiled module symbol missing".into());
        }
        // SAFETY: emitted symbol has the sole checked Cpu ABI; Library ownership
        // pins its executable mapping for every registered function pointer.
        let function =
            unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut Cpu)>(pointer) };
        let bytes: Rc<[u8]> = elf
            .code(block.start, (block.end - block.start) as usize)?
            .into();
        for (&pc, _) in discovery.instructions.range(block.start..block.end) {
            entries.push(Entry {
                pc,
                start: block.start,
                bytes: bytes.clone(),
                function,
            });
        }
    }
    Ok(Compiled {
        library: Rc::new(library),
        entries,
        manifest,
    })
}

// Keep the public manifest cumulative, independently of the emitted batch.
fn union_manifest(discovery: &Discovery, file: &[u8]) -> Manifest {
    use crate::discover::{Block, Failure};
    use sha2::{Digest, Sha256};
    let mut leaders = discovery.roots.clone();
    for instruction in discovery.instructions.values() {
        if aot::terminal(instruction) {
            leaders.extend(aot::successors(instruction));
        }
    }
    let mut remaining: BTreeSet<_> = discovery.instructions.keys().copied().collect();
    let mut blocks = Vec::new();
    while let Some(start) = remaining.first().copied() {
        let mut pc = start;
        let mut count = 0;
        let end = loop {
            let instruction = &discovery.instructions[&pc];
            remaining.remove(&pc);
            count += 1;
            if aot::terminal(instruction)
                || leaders.contains(&instruction.next_ip())
                || !remaining.contains(&instruction.next_ip())
            {
                break instruction.next_ip();
            }
            pc = instruction.next_ip();
        };
        blocks.push(Block {
            start,
            end,
            instructions: count,
        });
    }
    Manifest {
        sha256: format!("{:x}", Sha256::digest(file)),
        roots: discovery.roots.iter().copied().collect(),
        instruction_count: discovery.instructions.len(),
        block_count: blocks.len(),
        untranslated: discovery
            .failures
            .iter()
            .map(|(&pc, reason)| Failure {
                pc,
                reason: reason.clone(),
            })
            .collect(),
        indirect_calls: discovery
            .instructions
            .values()
            .filter(|i| i.is_call_near_indirect())
            .map(|i| i.ip())
            .collect(),
        blocks,
    }
}
fn incremental_discovery(mut discovery: Discovery, existing: &BTreeSet<u64>) -> Result<Discovery> {
    if existing
        .iter()
        .any(|pc| !discovery.instructions.contains_key(pc))
    {
        return Err("Existing translation is absent from cumulative discovery".into());
    }
    discovery
        .instructions
        .retain(|pc, _| !existing.contains(pc));
    Ok(discovery)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incremental_emission_stops_at_old_boundaries_and_preserves_union_manifest() {
        let mut file = crate::module_executor::memory_fixture();
        // New prefix falls through into an old entry. A second new island
        // branches back to it; neither batch may re-emit the old instructions.
        file[0x100..0x104].copy_from_slice(&[0x90, 0x90, 0x90, 0xc3]);
        file[0x110..0x112].copy_from_slice(&[0xeb, 0xf0]);
        let elf = Elf::parse(&file).unwrap();
        let full = Discovery::run(&elf, BTreeSet::from([0x100, 0x102, 0x110]), 16).unwrap();
        let expected = full
            .emit_with_trace(&elf, &file, "full", &BTreeSet::new())
            .unwrap();
        let manifest = union_manifest(&full, &file);
        assert_eq!(
            serde_json::to_value(&manifest).unwrap(),
            serde_json::to_value(&expected.manifest).unwrap()
        );
        let delta = incremental_discovery(full, &BTreeSet::from([0x102, 0x103])).unwrap();
        assert_eq!(
            delta.instructions.keys().copied().collect::<Vec<_>>(),
            [0x100, 0x101, 0x110]
        );
        let emitted = delta
            .emit_with_trace(&elf, &file, "delta", &BTreeSet::new())
            .unwrap();
        assert_eq!(emitted.manifest.instruction_count, 3);
        assert_eq!(
            emitted
                .manifest
                .blocks
                .iter()
                .map(|b| (b.start, b.end))
                .collect::<Vec<_>>(),
            [(0x100, 0x102), (0x110, 0x112)]
        );
        assert!(!emitted.assembly.contains(".globl _delta_102"));
        assert!(emitted.assembly.contains(".globl _delta_100"));
        // Discovery still rejects entry into an instruction owned by an old batch.
        file[0x120..0x126].copy_from_slice(&[0xb8, 1, 0, 0, 0, 0xc3]);
        let elf = Elf::parse(&file).unwrap();
        assert!(Discovery::run(&elf, BTreeSet::from([0x120, 0x121]), 16).is_err());
        assert!(Discovery::run(&elf, BTreeSet::from([0x100, 0x102, 0x110]), 4).is_err());
        let old = Discovery::run(&elf, BTreeSet::from([0x102]), 16).unwrap();
        assert!(incremental_discovery(old, &BTreeSet::from([0x120])).is_err());
    }
    #[test]
    fn invalid_discovery_does_not_create_compiler_outputs() {
        let directory = std::env::temp_dir().join(format!(
            "native-ps5-compile-reject-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = crate::module_executor::memory_fixture();
        let archive = directory.join("missing.a");
        assert!(compile(&file, BTreeSet::new(), 100, &archive, &directory).is_err());
        assert!(compile(&file, BTreeSet::from([0x100]), 1, &archive, &directory).is_err());
        let mut unsupported = file;
        unsupported[0x100] = 0xf4;
        assert!(
            compile(
                &unsupported,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory
            )
            .is_err()
        );
        assert!(!directory.exists());
    }
}
