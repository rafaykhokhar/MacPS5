//! Explicit guest-path catalog backed by already-open host files.
//! Guest strings never become host paths. Host assets are trusted and must not
//! be edited concurrently; this is not a sandbox against host file mutation.
use crate::{Result, modules::EntryPoints};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

struct Entry {
    file: File,
    name: String,
    entries: EntryPoints,
}
#[derive(Default)]
pub struct ModuleFiles {
    files: BTreeMap<String, Entry>,
}
pub struct Input {
    pub name: String,
    pub bytes: Vec<u8>,
    pub entries: EntryPoints,
}

pub(crate) fn valid_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 1024
        && !path.contains(['\0', '\\'])
        && path[1..]
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}
impl ModuleFiles {
    pub fn contains(&self, guest_path: &str) -> bool {
        self.files.contains_key(guest_path)
    }
    /// Host-side configuration only. The descriptor stays pinned even if its
    /// pathname is later replaced. Registering the same guest path is an error.
    pub fn register(
        &mut self,
        guest_path: &str,
        host_path: impl AsRef<Path>,
        name: &str,
        entries: EntryPoints,
    ) -> Result<()> {
        if !valid_path(guest_path) || self.files.contains_key(guest_path) || self.files.len() >= 128
        {
            return Err("Invalid, duplicate or excessive module file registration".into());
        }
        crate::registry::Registry::default().register_module(name, &[])?;
        let file = File::open(host_path).map_err(|e| format!("Cannot open module file: {e}"))?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("Module source must be a regular file".into());
        }
        self.files.insert(
            guest_path.into(),
            Entry {
                file,
                name: name.into(),
                entries,
            },
        );
        Ok(())
    }
    pub fn read(&mut self, guest_path: &str, budget: usize) -> Result<Input> {
        if !valid_path(guest_path) {
            return Err("Invalid guest module path".into());
        }
        let entry = self
            .files
            .get_mut(guest_path)
            .ok_or("Guest module file is not registered")?;
        let length = usize::try_from(entry.file.metadata().map_err(|e| e.to_string())?.len())
            .map_err(|_| "Module file length overflow")?;
        if length < 64 || length > budget {
            return Err("Module file is empty, truncated or exceeds budget".into());
        }
        entry
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        let mut bytes = vec![0; length];
        entry
            .file
            .read_exact(&mut bytes)
            .map_err(|e| format!("Cannot read complete module: {e}"))?;
        if entry.file.read(&mut [0u8; 1]).map_err(|e| e.to_string())? != 0 {
            return Err("Module file changed length during read".into());
        }
        Ok(Input {
            name: entry.name.clone(),
            bytes,
            entries: entry.entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{Arguments, Executor, Modules, Phase};
    struct Start(bool);
    impl Executor for Start {
        fn call(&mut self, _: &mut crate::loader::Image, _: u64, _: Arguments) -> Result<i32> {
            if self.0 {
                Ok(0)
            } else {
                Err("guest fault".into())
            }
        }
    }
    #[test]
    fn invalid_file_loads_do_not_publish_or_consume_module_budget() {
        let path =
            std::env::temp_dir().join(format!("native-ps5-invalid-module-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let host = path.join("invalid.elf");
        std::fs::write(&host, vec![0u8; 128]).unwrap();
        let mut files = ModuleFiles::default();
        assert!(
            files
                .register(
                    "/app0/directory",
                    &path,
                    "directory.prx",
                    EntryPoints::default()
                )
                .is_err()
        );
        files
            .register("/app0/bad.prx", &host, "bad.prx", EntryPoints::default())
            .unwrap();
        let mut modules = Modules::new(1024);
        let error = modules
            .load_start_file(
                &mut files,
                "/app0/bad.prx",
                0x10000,
                &mut Start(true),
                Arguments::default(),
            )
            .unwrap_err();
        assert_eq!(error.handle, None);
        assert_eq!(modules.bytes(), 0);
        // The catalog pins the descriptor, not stale file-length metadata.
        std::fs::write(&host, vec![0u8; 2048]).unwrap();
        assert!(files.read("/app0/bad.prx", 1024).is_err());
        std::fs::write(&host, vec![0u8; 10]).unwrap();
        assert!(files.read("/app0/bad.prx", 1024).is_err());
        drop(files);
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn file_catalog_loads_owned_images_and_retains_failed_start_handle() {
        let path = std::env::temp_dir().join(format!("native-ps5-modules-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let host = path.join("module.elf");
        let original = crate::modules::fixture();
        std::fs::write(&host, &original).unwrap();
        let mut files = ModuleFiles::default();
        let entries = EntryPoints {
            start: Some(0x100),
            stop: Some(0x110),
        };
        files
            .register("/app0/module.prx", &host, "module.prx", entries)
            .unwrap();
        assert!(
            files
                .register("/app0/module.prx", &host, "other.prx", entries)
                .is_err()
        );
        for invalid in [
            "module.prx",
            "/app0/../module.prx",
            "/app0//module.prx",
            "/app0/./module.prx",
            "/app0/",
            "/app0/mod\\ule",
            "/app0/\0module",
        ] {
            assert!(files.register(invalid, &host, "bad.prx", entries).is_err());
            assert!(files.read(invalid, 4096).is_err());
        }
        assert!(files.read("/app0/missing.prx", 4096).is_err());
        assert!(files.read("/app0/module.prx", original.len() - 1).is_err());
        // Replacement at the same host pathname cannot redirect an open catalog entry.
        std::fs::rename(&host, path.join("original.elf")).unwrap();
        std::fs::write(&host, b"replacement").unwrap();
        assert_eq!(
            files.read("/app0/module.prx", 4096).unwrap().bytes,
            original
        );
        let mut modules = Modules::new(8192);
        let handle = modules
            .load_start_file(
                &mut files,
                "/app0/module.prx",
                0x10000,
                &mut Start(true),
                Arguments::default(),
            )
            .unwrap();
        assert_eq!(modules.phase(handle), Some(Phase::Started));
        modules
            .stop(handle, &mut Start(true), Arguments::default())
            .unwrap();
        modules.unload(handle).unwrap();
        let error = modules
            .load_start_file(
                &mut files,
                "/app0/module.prx",
                0x10000,
                &mut Start(false),
                Arguments::default(),
            )
            .unwrap_err();
        let handle = error.handle.unwrap();
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert!(modules.unload(handle).is_err());
        let error = modules
            .load_start_file(
                &mut files,
                "/app0/missing.prx",
                0x20000,
                &mut Start(true),
                Arguments::default(),
            )
            .unwrap_err();
        assert_eq!(error.handle, None);
        drop(files);
        std::fs::remove_dir_all(&path).unwrap();
    }
}
