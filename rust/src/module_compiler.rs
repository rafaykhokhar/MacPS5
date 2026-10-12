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
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("Module compilation requires Apple Silicon macOS".into());
    }
    let elf = Elf::parse(file)?;
    let discovery = Discovery::run(&elf, roots, limit)?;
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
        manifest: artifacts.manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
