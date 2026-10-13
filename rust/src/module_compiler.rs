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
    time::Instant,
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
    mapped_bytes: usize,
}
impl Library {
    /// Rounded segment VM span, including zero-fill; not resident memory/RSS.
    pub(crate) fn mapped_bytes(&self) -> usize {
        self.mapped_bytes
    }
}

/// Thin little-endian arm64 dylib accounting from Apple's mach-o/loader.h.
/// Only segment mappings are counted; dyld/dependencies and allocator overhead
/// are outside this budget. Apple Silicon mappings are charged in 16 KiB pages.
fn macho_mapped_bytes(file: &[u8]) -> Result<usize> {
    fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
        let end = at.checked_add(4).ok_or("Mach-O field overflow")?;
        Ok(u32::from_le_bytes(
            bytes
                .get(at..end)
                .ok_or("Truncated Mach-O field")?
                .try_into()
                .unwrap(),
        ))
    }
    fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
        let end = at.checked_add(8).ok_or("Mach-O field overflow")?;
        Ok(u64::from_le_bytes(
            bytes
                .get(at..end)
                .ok_or("Truncated Mach-O field")?
                .try_into()
                .unwrap(),
        ))
    }
    const PAGE: u64 = 16 * 1024;
    if file.len() < 32
        || u32_at(file, 0)? != 0xfeedfacf
        || u32_at(file, 4)? != 0x0100000c
        || u32_at(file, 8)? != 0
        || u32_at(file, 12)? != 6
    {
        return Err("Generated library must be a thin arm64 Mach-O dylib".into());
    }
    let count = u32_at(file, 16)? as usize;
    let command_bytes = u32_at(file, 20)? as usize;
    let command_end = 32usize
        .checked_add(command_bytes)
        .ok_or("Mach-O command overflow")?;
    if command_end > file.len() || count > command_bytes / 8 {
        return Err("Invalid Mach-O load-command bounds".into());
    }
    let mut at = 32usize;
    let mut ranges = Vec::new();
    for _ in 0..count {
        if command_end.saturating_sub(at) < 8 {
            return Err("Truncated Mach-O load command".into());
        }
        let command = u32_at(file, at)?;
        let size = u32_at(file, at + 4)? as usize;
        let next = at.checked_add(size).ok_or("Mach-O command overflow")?;
        if size < 8 || size & 7 != 0 || next > command_end {
            return Err("Invalid Mach-O load-command size".into());
        }
        if command == 0x19 {
            if size < 72 {
                return Err("Truncated Mach-O segment".into());
            }
            let segment = &file[at..next];
            let section_bytes = (u32_at(segment, 64)? as usize)
                .checked_mul(80)
                .and_then(|n| n.checked_add(72))
                .ok_or("Mach-O section count overflow")?;
            if section_bytes != size {
                return Err("Invalid Mach-O segment section bounds".into());
            }
            let address = u64_at(segment, 24)?;
            let length = u64_at(segment, 32)?;
            let file_offset = u64_at(segment, 40)?;
            let file_size = u64_at(segment, 48)?;
            let file_end = file_offset
                .checked_add(file_size)
                .ok_or("Mach-O file span overflow")?;
            if file_size > length || file_end > file.len() as u64 {
                return Err("Invalid Mach-O segment file span".into());
            }
            if length != 0 {
                let end = address
                    .checked_add(length)
                    .ok_or("Mach-O VM span overflow")?;
                let rounded_end = end
                    .checked_add(PAGE - 1)
                    .ok_or("Mach-O page rounding overflow")?
                    & !(PAGE - 1);
                ranges.push((address & !(PAGE - 1), rounded_end));
            }
        }
        at = next;
    }
    if at != command_end {
        return Err("Mach-O load-command size mismatch".into());
    }
    if ranges.is_empty() {
        return Err("Mach-O dylib has no segment mappings".into());
    }
    ranges.sort_unstable();
    let mut previous_end = 0;
    let mut total = 0u64;
    for (start, end) in ranges {
        if start < previous_end {
            return Err("Overlapping Mach-O segment mappings".into());
        }
        total = total
            .checked_add(end - start)
            .ok_or("Mach-O mapped-byte sum overflow")?;
        previous_end = end;
    }
    usize::try_from(total).map_err(|_| "Mach-O mapped-byte count exceeds host usize".into())
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
    remaining_mapped_bytes: usize,
) -> Result<Compiled> {
    compile_incremental(
        file,
        roots,
        limit,
        archive,
        parent,
        &BTreeSet::new(),
        remaining_mapped_bytes,
    )
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
    remaining_mapped_bytes: usize,
) -> Result<Compiled> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("Module compilation requires Apple Silicon macOS".into());
    }
    let timing_enabled =
        std::env::var_os("NATIVE_PS5_TRANSLATION_TIMINGS").is_some_and(|value| value == "1");
    let started = Instant::now();
    let elf = Elf::parse(file)?;
    let discovery = Discovery::run(&elf, roots, limit)?;
    let manifest = union_manifest(&discovery, file);
    let discovery = incremental_discovery(discovery, existing)?;
    let artifacts = discovery.emit_with_trace(&elf, file, "module_aot", &BTreeSet::new())?;
    if artifacts.manifest.blocks.is_empty() {
        return Err("Module has no translatable blocks".into());
    }
    let assembly = aot::rust_memory_helpers(artifacts.assembly)?;
    let emitted = Instant::now();
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
        mapped_bytes: 0,
    };
    let source = library.directory.join("module.S");
    let output = library.directory.join("module.dylib");
    let exports = library.directory.join("exports.txt");
    // Only block entry points are resolved through dlsym. Hiding archive
    // symbols lets ld remove runtime functions unreachable from those roots.
    // Referenced helpers and their transitive dependencies remain live.
    let symbols: String = artifacts
        .manifest
        .blocks
        .iter()
        .map(|block| format!("_module_aot_{:x}\n", block.start))
        .collect();
    std::fs::write(&exports, symbols).map_err(|e| e.to_string())?;
    std::fs::write(&source, assembly).map_err(|e| e.to_string())?;
    let prepared = Instant::now();
    let build = Command::new("xcrun")
        .args(["clang", "-arch", "arm64", "-dynamiclib"])
        .args([
            "-Wl,-dead_strip",
            "-Xlinker",
            "-exported_symbols_list",
            "-Xlinker",
        ])
        .arg(&exports)
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
    let linked = Instant::now();
    let generated =
        std::fs::read(&output).map_err(|e| format!("Cannot read generated library: {e}"))?;
    library.mapped_bytes = macho_mapped_bytes(&generated)?;
    if library.mapped_bytes > remaining_mapped_bytes {
        return Err(format!(
            "Compiled library mapped-byte budget exceeded: {} > {}",
            library.mapped_bytes, remaining_mapped_bytes
        ));
    }
    drop(generated);
    let path = CString::new(
        output
            .canonicalize()
            .map_err(|e| e.to_string())?
            .as_os_str()
            .as_encoded_bytes(),
    )
    .map_err(|_| "Invalid module output path")?;
    let validated = Instant::now();
    // macOS RTLD_NOW | RTLD_LOCAL. Input is exclusively generated ARM64, linked
    // against the matching trusted Rust runtime archive supplied by the host.
    library.handle = unsafe { dlopen(path.as_ptr(), 2 | 4) };
    if library.handle.is_null() {
        return Err("Cannot load compiled ARM64 module library".into());
    }
    let loaded = Instant::now();
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
    if timing_enabled {
        // Emit only successful batches. Durations are disjoint wall-clock
        // stages; this ends before the executor's publication/live-byte checks.
        eprintln!(
            "TRANSLATION_TIMING discovery_emit_ns={} prepare_ns={} clang_link_ns={} validate_ns={} dlopen_ns={} entries_ns={} instructions={} blocks={} mapped_bytes={}",
            emitted.duration_since(started).as_nanos(),
            prepared.duration_since(emitted).as_nanos(),
            linked.duration_since(prepared).as_nanos(),
            validated.duration_since(linked).as_nanos(),
            loaded.duration_since(validated).as_nanos(),
            loaded.elapsed().as_nanos(),
            discovery.instructions.len(),
            artifacts.manifest.blocks.len(),
            library.mapped_bytes
        );
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
    fn macho_fixture(segments: &[(u64, u64, u64, u64)]) -> Vec<u8> {
        let mut file = vec![0u8; 32 + segments.len() * 72];
        for (at, value) in [
            (0, 0xfeedfacfu32),
            (4, 0x0100000c),
            (12, 6),
            (16, segments.len() as u32),
            (20, (segments.len() * 72) as u32),
        ] {
            file[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (n, &(address, size, file_offset, file_size)) in segments.iter().enumerate() {
            let at = 32 + n * 72;
            file[at..at + 4].copy_from_slice(&0x19u32.to_le_bytes());
            file[at + 4..at + 8].copy_from_slice(&72u32.to_le_bytes());
            for (offset, value) in [
                (24, address),
                (32, size),
                (40, file_offset),
                (48, file_size),
            ] {
                file[at + offset..at + offset + 8].copy_from_slice(&value.to_le_bytes());
            }
        }
        file
    }
    #[test]
    fn macho_accounting_counts_zero_fill_and_rounds_full_spans() {
        let file = macho_fixture(&[(1, 16_384, 0, 104)]);
        assert_eq!(macho_mapped_bytes(&file).unwrap(), 32_768);
        let file = macho_fixture(&[
            (0, 16_384, 0, 100),
            (32_768, 32_769, 0, 0),
            (u64::MAX, 0, 0, 0),
        ]);
        assert_eq!(macho_mapped_bytes(&file).unwrap(), 65_536);
        // Even disjoint raw byte ranges must not share a charged VM page.
        assert!(macho_mapped_bytes(&macho_fixture(&[(0, 1, 0, 0), (1, 1, 0, 0)])).is_err());
        assert!(
            macho_mapped_bytes(&macho_fixture(&[(0, 32768, 0, 0), (16384, 16384, 0, 0)])).is_err()
        );
        assert!(macho_mapped_bytes(&macho_fixture(&[])).is_err());
        assert!(macho_mapped_bytes(&macho_fixture(&[(0, 0, 0, 0)])).is_err());
    }
    #[test]
    fn macho_accounting_rejects_malformed_headers_commands_and_spans() {
        let valid = macho_fixture(&[(0, 16_384, 0, 104)]);
        for length in [0, 4, 31, 32, 39, 103] {
            assert!(
                macho_mapped_bytes(&valid[..length]).is_err(),
                "length {length}"
            );
        }
        for (offset, value) in [
            (0, 0xcffaedfeu32), // swapped endian
            (0, 0xcafebabe),    // universal binary
            (4, 0x01000007),    // x86_64
            (8, 2),             // arm64e is not the generated arm64 ABI
            (12, 2),            // executable, not dylib
            (16, 2),            // missing command
            (16, u32::MAX),
            (20, u32::MAX),
            (20, 71),
            (36, 0),
            (36, 7),
            (36, 71),
            (36, 80),
            (96, 1), // section count exceeds command
        ] {
            let mut file = valid.clone();
            file[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(
                macho_mapped_bytes(&file).is_err(),
                "field {offset}: {value}"
            );
        }
        for segments in [
            vec![(u64::MAX - 1, 3, 0, 0)], // VM overflow
            vec![(u64::MAX - 1, 1, 0, 0)], // rounding overflow
            vec![(0, 1, 0, 2)],            // file size exceeds VM size
            vec![(0, 16384, 100, 5)],      // file end outside 104-byte fixture
            vec![(0, 16384, u64::MAX, 1)], // file span overflow
        ] {
            assert!(macho_mapped_bytes(&macho_fixture(&segments)).is_err());
        }
        let mut extra = valid.clone();
        extra.extend_from_slice(&[0; 8]);
        extra[20..24].copy_from_slice(&80u32.to_le_bytes());
        assert!(macho_mapped_bytes(&extra).is_err()); // unaccounted command bytes
    }
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
        assert!(
            compile(
                &file,
                BTreeSet::new(),
                100,
                &archive,
                &directory,
                usize::MAX
            )
            .is_err()
        );
        assert!(
            compile(
                &file,
                BTreeSet::from([0x100]),
                1,
                &archive,
                &directory,
                usize::MAX
            )
            .is_err()
        );
        let mut unsupported = file;
        unsupported[0x100] = 0xf4;
        assert!(
            compile(
                &unsupported,
                BTreeSet::from([0x100]),
                100,
                &archive,
                &directory,
                usize::MAX,
            )
            .is_err()
        );
        assert!(!directory.exists());
    }
}
