//! Owned guest directories and memory-backed files with shared content quota.
//! No symlinks, host lookup, path forwarding, ACLs or persistent save storage.
//! Virtual profile defaults: uid1000/gid1000/umask0; these are not console facts.
//! Host registration materializes searchable readonly ancestors transactionally.
//! mkdir/lookup policy: FreeBSD releng/11.4 sys/kern/{vfs_syscalls,vfs_lookup}.c.
//! ABI: ps5-payload-dev/sdk include/freebsd/sys/stat.h (FreeBSD 11.4 amd64),
//! corroborated by AnyPS5/core/libs/SceTypes.hpp::FileStat. Serialize fields
//! explicitly: host struct stat has a different layout on Apple Silicon.
//! https://raw.githubusercontent.com/ps5-payload-dev/sdk/master/include/freebsd/sys/stat.h
//! https://raw.githubusercontent.com/ps5-payload-dev/sdk/master/include/freebsd/sys/_types.h
use crate::{Result, guest_memory::Memory};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

pub const STAT_SIZE: usize = 120;
const PATH_MAX: usize = 1024;
const MAX_FILES: usize = 256;
const MAX_DIRECTORIES: usize = 256;
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRACE_RECORDS: usize = 256;
const MAX_TRACE_BYTES: usize = 65536;
pub(crate) struct Quota {
    pub used: Cell<usize>,
    pub revision: Cell<u64>,
}
impl Default for Quota {
    fn default() -> Self {
        Self {
            used: Cell::new(0),
            revision: Cell::new(0),
        }
    }
}
impl Quota {
    pub fn remaining(&self) -> usize {
        MAX_CONTENT_BYTES - self.used.get()
    }
    pub fn change(&self, old: usize, new: usize) -> std::result::Result<(), u32> {
        let used = self
            .used
            .get()
            .checked_sub(old)
            .and_then(|v| v.checked_add(new))
            .filter(|v| *v <= MAX_CONTENT_BYTES)
            .ok_or(28u32)?;
        let revision = self.revision.get().checked_add(1).ok_or(28u32)?;
        self.used.set(used);
        self.revision.set(revision);
        Ok(())
    }
}
pub(crate) struct FileState {
    pub bytes: Box<[u8]>,
    pub revision: u64,
}
pub(crate) struct FileBacking {
    pub state: RefCell<FileState>,
    pub quota: Rc<Quota>,
    pub writable: bool,
    inode: u32,
    mode: u16,
    uid: u32,
    gid: u32,
}
impl FileBacking {
    pub fn stat(&self) -> [u8; STAT_SIZE] {
        let length = self.state.borrow().bytes.len();
        let mut record = [0; STAT_SIZE];
        record[0..4].copy_from_slice(&1u32.to_le_bytes());
        record[4..8].copy_from_slice(&self.inode.to_le_bytes());
        record[8..10].copy_from_slice(&(0o100000 | self.mode).to_le_bytes());
        record[10..12].copy_from_slice(&1u16.to_le_bytes());
        record[12..16].copy_from_slice(&self.uid.to_le_bytes());
        record[16..20].copy_from_slice(&self.gid.to_le_bytes());
        record[72..80].copy_from_slice(&(length as i64).to_le_bytes());
        record[80..88].copy_from_slice(&(length.div_ceil(512) as i64).to_le_bytes());
        record[88..92].copy_from_slice(&512u32.to_le_bytes());
        record
    }
}
impl Drop for FileBacking {
    fn drop(&mut self) {
        self.quota
            .used
            .set(self.quota.used.get() - self.state.get_mut().bytes.len());
        self.quota
            .revision
            .set(self.quota.revision.get().saturating_add(1));
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct StatRequest {
    pub path: Vec<u8>,
    pub error: Option<u32>,
}
#[derive(Default, Debug)]
pub struct StatTrace {
    pub requests: Vec<StatRequest>,
    pub dropped: u64,
}
/// Explicit virtual owner policy, not measured console credentials. Privilege
/// escalation, supplementary groups and ACLs are not modeled. uid0 receives
/// the same mode checks as other configured identities (no superuser bypass).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamespaceProfile {
    pub uid: u32,
    pub gid: u32,
    pub umask: u16,
}
impl Default for NamespaceProfile {
    fn default() -> Self {
        Self {
            uid: 1000,
            gid: 1000,
            umask: 0,
        }
    }
}
#[derive(Clone)]
struct Directory {
    inode: u32,
    mode: u16,
    uid: u32,
    gid: u32,
    read_only: bool,
    links: u16,
}
pub struct MkdirPlan {
    directories: BTreeMap<Vec<u8>, Directory>,
    next_inode: u32,
    revision: u64,
    owner: Rc<()>,
}
pub struct Files {
    files: BTreeMap<Vec<u8>, Rc<FileBacking>>,
    quota: Rc<Quota>,
    directories: BTreeMap<Vec<u8>, Directory>,
    profile: NamespaceProfile,
    next_inode: u32,
    revision: u64,
    owner: Rc<()>,
    tracing: bool,
    trace: StatTrace,
    trace_bytes: usize,
}
impl Default for Files {
    fn default() -> Self {
        let profile = NamespaceProfile::default();
        Self {
            files: BTreeMap::new(),
            quota: Rc::new(Quota::default()),
            directories: BTreeMap::from([(
                b"/".to_vec(),
                Directory {
                    inode: 1,
                    mode: 0o555,
                    uid: profile.uid,
                    gid: profile.gid,
                    read_only: true,
                    links: 2,
                },
            )]),
            profile,
            next_inode: 2,
            revision: 0,
            owner: Rc::new(()),
            tracing: false,
            trace: StatTrace::default(),
            trace_bytes: 0,
        }
    }
}
impl Files {
    pub fn configure_profile(&mut self, profile: NamespaceProfile) -> Result<()> {
        if self.revision != 0 || profile.umask & !0o777 != 0 {
            return Err("Namespace profile is invalid or already frozen".into());
        }
        self.profile = profile;
        let root = self.directories.get_mut(&b"/"[..]).expect("Owned root");
        root.uid = profile.uid;
        root.gid = profile.gid;
        Ok(())
    }
    pub fn profile(&self) -> NamespaceProfile {
        self.profile
    }
    pub(crate) fn is_directory(&self, path: &[u8]) -> std::result::Result<bool, u32> {
        let resolved = self.resolve(path, None)?;
        Ok(self.directories.contains_key(trimmed(&resolved)))
    }
    pub(crate) fn check_open(
        &self,
        path: &[u8],
        read: bool,
        write: bool,
    ) -> std::result::Result<(), u32> {
        let backing = self.backing(path)?;
        if write && !backing.writable {
            return Err(13);
        }
        let bits = if self.profile.uid == backing.uid {
            backing.mode >> 6
        } else if self.profile.gid == backing.gid {
            backing.mode >> 3
        } else {
            backing.mode
        };
        let required = (if read { 4 } else { 0 }) | (if write { 2 } else { 0 });
        if bits & required != required {
            return Err(13);
        }
        Ok(())
    }
    fn access(&self, directory: &Directory, requested: u16) -> bool {
        let bits = if self.profile.uid == directory.uid {
            directory.mode >> 6
        } else if self.profile.gid == directory.gid {
            directory.mode >> 3
        } else {
            directory.mode
        };
        bits & requested == requested
    }
    /// Walk actual nodes before handling '..'. Only the final component may be
    /// absent. Trailing slash remains visible to the operation-specific layer.
    /// Relative callers must supply the explicit guest cwd; no host fallback.
    pub fn resolve(&self, path: &[u8], cwd: Option<&[u8]>) -> std::result::Result<Vec<u8>, u32> {
        if path.is_empty() {
            return Err(2);
        }
        if path.len() >= PATH_MAX || path.contains(&0) {
            return Err(63);
        }
        let mut current = if path[0] == b'/' {
            b"/".to_vec()
        } else {
            let cwd = cwd.ok_or(2u32)?;
            let cwd = cwd.strip_suffix(&[0]).unwrap_or(cwd);
            if cwd.first() != Some(&b'/') {
                return Err(2);
            }
            let resolved = self.resolve(cwd, None)?;
            let resolved = trimmed(&resolved).to_vec();
            if !self.directories.contains_key(&resolved) {
                return Err(if self.files.contains_key(&resolved) {
                    20
                } else {
                    2
                });
            }
            resolved
        };
        let parts: Vec<_> = path
            .split(|byte| *byte == b'/')
            .filter(|part| !part.is_empty())
            .collect();
        for (index, part) in parts.iter().enumerate() {
            let directory =
                self.directories
                    .get(&current)
                    .ok_or(if self.files.contains_key(&current) {
                        20u32
                    } else {
                        2
                    })?;
            if !self.access(directory, 1) {
                return Err(13);
            }
            if part.len() > 255 {
                return Err(63);
            }
            match *part {
                b"." => {}
                b".." => {
                    current = parent(&current).to_vec();
                }
                _ => {
                    if current != b"/" {
                        current.push(b'/');
                    }
                    current.extend_from_slice(part);
                    if current.len() >= PATH_MAX {
                        return Err(63);
                    }
                    if index + 1 < parts.len() && !self.directories.contains_key(&current) {
                        return Err(if self.files.contains_key(&current) {
                            20
                        } else {
                            2
                        });
                    }
                }
            }
        }
        if path.ends_with(b"/") && current != b"/" {
            if self.files.contains_key(&current) {
                return Err(20);
            }
            current.push(b'/');
        }
        Ok(current)
    }
    fn materialize(
        &self,
        path: &[u8],
    ) -> std::result::Result<(BTreeMap<Vec<u8>, Directory>, u32), u32> {
        let mut directories = self.directories.clone();
        let mut next = self.next_inode;
        let mut current = b"/".to_vec();
        for part in path[1..]
            .split(|byte| *byte == b'/')
            .filter(|part| !part.is_empty())
        {
            let previous = current.clone();
            if current != b"/" {
                current.push(b'/');
            }
            current.extend_from_slice(part);
            if self.files.contains_key(&current) {
                return Err(20);
            }
            if !directories.contains_key(&current) {
                if directories.len() == MAX_DIRECTORIES {
                    return Err(28);
                }
                let inode = next;
                next = next.checked_add(1).ok_or(28u32)?;
                directories
                    .get_mut(&previous)
                    .expect("Materialized parent")
                    .links += 1;
                directories.insert(
                    current.clone(),
                    Directory {
                        inode,
                        mode: 0o555,
                        uid: self.profile.uid,
                        gid: self.profile.gid,
                        read_only: true,
                        links: 2,
                    },
                );
            }
        }
        Ok((directories, next))
    }
    pub fn plan_mkdir(
        &self,
        path: &[u8],
        cwd: Option<&[u8]>,
        mode: u16,
    ) -> std::result::Result<MkdirPlan, u32> {
        let resolved = self.resolve(path, cwd)?;
        let resolved = trimmed(&resolved);
        if resolved == b"/" && path.iter().all(|byte| *byte == b'/') {
            return Err(21);
        }
        if self.files.contains_key(resolved) || self.directories.contains_key(resolved) {
            return Err(17);
        }
        let p = parent(resolved);
        let directory = self.directories.get(p).ok_or(2u32)?;
        if directory.read_only {
            return Err(30);
        }
        if !self.access(directory, 3) {
            return Err(13);
        }
        if self.directories.len() == MAX_DIRECTORIES {
            return Err(28);
        }
        self.revision.checked_add(1).ok_or(28u32)?;
        let next_inode = self.next_inode.checked_add(1).ok_or(28u32)?;
        let mut directories = self.directories.clone();
        directories.get_mut(p).expect("Checked parent").links += 1;
        directories.insert(
            resolved.to_vec(),
            Directory {
                inode: self.next_inode,
                mode: mode & 0o777 & !self.profile.umask,
                uid: self.profile.uid,
                gid: directory.gid,
                read_only: false,
                links: 2,
            },
        );
        Ok(MkdirPlan {
            directories,
            next_inode,
            revision: self.revision,
            owner: self.owner.clone(),
        })
    }
    pub fn commit_mkdir(&mut self, plan: MkdirPlan) -> std::result::Result<(), u32> {
        if !Rc::ptr_eq(&self.owner, &plan.owner) || self.revision != plan.revision {
            return Err(22);
        }
        let revision = self.revision.checked_add(1).ok_or(28u32)?;
        self.directories = plan.directories;
        self.next_inode = plan.next_inode;
        self.revision = revision;
        Ok(())
    }
    /// Register owned contents at a canonical absolute guest path. Missing
    /// ancestors become real searchable readonly virtual directory nodes.
    pub fn register(&mut self, path: &[u8], bytes: Vec<u8>) -> Result<()> {
        self.register_mode(path, bytes, false)
    }
    pub fn register_writable(&mut self, path: &[u8], bytes: Vec<u8>) -> Result<()> {
        self.register_mode(path, bytes, true)
    }
    fn register_mode(&mut self, path: &[u8], bytes: Vec<u8>, writable: bool) -> Result<()> {
        if !canonical(path)
            || self.files.contains_key(path)
            || self.directories.contains_key(path)
            || self.files.len() >= MAX_FILES
        {
            return Err("Invalid, duplicate or excessive guest file registration".into());
        }
        let (directories, next) = self
            .materialize(parent(path))
            .map_err(|e| format!("Cannot register file ancestors: errno {e}"))?;
        let next_inode = next
            .checked_add(1)
            .ok_or("Guest inode identities exhausted")?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Guest namespace revision exhausted")?;
        self.quota
            .change(0, bytes.len())
            .map_err(|_| "Guest file content budget exceeded")?;
        let inode = next;
        self.files.insert(
            path.to_vec(),
            Rc::new(FileBacking {
                state: RefCell::new(FileState {
                    bytes: bytes.into_boxed_slice(),
                    revision: 0,
                }),
                quota: self.quota.clone(),
                writable,
                inode,
                mode: if writable { 0o666 } else { 0o444 },
                uid: self.profile.uid,
                gid: self.profile.gid,
            }),
        );
        self.directories = directories;
        self.next_inode = next_inode;
        self.revision = revision;
        Ok(())
    }
    /// Explicit writable virtual directory; creation never reaches a host path.
    pub fn allow_create_directory(&mut self, path: &[u8]) -> Result<()> {
        self.register_directory(path, 0o777, false)
    }
    /// Host setup may explicitly set a directory's mode and readonly policy.
    /// Unlike guest mkdir, existing nodes keep their identities when configured.
    pub fn register_directory(&mut self, path: &[u8], mode: u16, read_only: bool) -> Result<()> {
        if (path != b"/" && !canonical(path)) || mode & !0o777 != 0 {
            return Err("Invalid guest directory registration".into());
        }
        let (mut directories, next) = self
            .materialize(path)
            .map_err(|e| format!("Cannot register directory: errno {e}"))?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Guest namespace revision exhausted")?;
        let directory = directories.get_mut(path).expect("Materialized directory");
        directory.mode = mode;
        directory.read_only = read_only;
        self.directories = directories;
        self.next_inode = next;
        self.revision = revision;
        Ok(())
    }
    /// Cwd setup preserves existing directory policy and creates missing nodes
    /// as readonly searchable directories, without consulting the host.
    pub(crate) fn ensure_working_directory(&mut self, path: &[u8]) -> Result<()> {
        if self.directories.contains_key(path) {
            return Ok(());
        }
        self.register_directory(path, 0o555, true)
    }
    pub(crate) fn backing(&self, path: &[u8]) -> std::result::Result<Rc<FileBacking>, u32> {
        let resolved = self.resolve(path, None)?;
        let path = trimmed(&resolved);
        if self.directories.contains_key(path) {
            return Err(21);
        }
        self.files.get(path).cloned().ok_or(2)
    }
    pub(crate) fn quota(&self) -> Rc<Quota> {
        self.quota.clone()
    }
    pub(crate) fn create(&mut self, path: &[u8]) -> std::result::Result<Rc<FileBacking>, u32> {
        let resolved = self.resolve(path, None)?;
        if resolved.ends_with(b"/") {
            return Err(2);
        }
        let path = resolved.as_slice();
        if self.directories.contains_key(path) {
            return Err(21);
        }
        if self.files.contains_key(path) {
            return Err(17);
        }
        let directory = self.directories.get(parent(path)).ok_or(2u32)?;
        if directory.read_only {
            return Err(30);
        }
        if !self.access(directory, 3) {
            return Err(13);
        }
        if self.files.len() >= MAX_FILES {
            return Err(28);
        }
        self.register_mode(path, Vec::new(), true)
            .map_err(|_| 28u32)?;
        // Newly created descriptors are authorized by the creation operation;
        // future opens use the stored permission bits.
        Rc::get_mut(self.files.get_mut(path).expect("New file"))
            .expect("Unpublished file")
            .mode &= !self.profile.umask;
        self.backing(path)
    }
    /// Tracing is off by default. Disabling retains already captured requests
    /// until drained; exhaustion drops trace records without changing syscall results.
    pub fn set_stat_tracing(&mut self, enabled: bool) {
        self.tracing = enabled;
    }
    pub fn take_stat_trace(&mut self) -> StatTrace {
        self.trace_bytes = 0;
        std::mem::take(&mut self.trace)
    }
    pub fn content_bytes(&self) -> usize {
        self.quota.used.get()
    }
    pub fn stat(
        &mut self,
        user: &mut Memory<'_>,
        path_address: u64,
        output: u64,
    ) -> std::result::Result<u64, u32> {
        self.stat_at(user, path_address, output, None)
    }
    pub fn stat_at(
        &mut self,
        user: &mut Memory<'_>,
        path_address: u64,
        output: u64,
        cwd: Option<&[u8]>,
    ) -> std::result::Result<u64, u32> {
        let path = read_path(user, path_address)?;
        let result = self
            .resolve(&path, cwd)
            .and_then(|resolved| self.stat_path(user, &resolved, output));
        if self.tracing {
            if self.trace.requests.len() < MAX_TRACE_RECORDS
                && self.trace_bytes + path.len() <= MAX_TRACE_BYTES
            {
                self.trace_bytes += path.len();
                self.trace.requests.push(StatRequest {
                    path,
                    error: result.as_ref().err().copied(),
                });
            } else {
                self.trace.dropped = self.trace.dropped.saturating_add(1);
            }
        }
        result
    }
    pub(crate) fn stat_record(&self, path: &[u8]) -> std::result::Result<[u8; STAT_SIZE], u32> {
        let resolved = self.resolve(path, None)?;
        let path = trimmed(&resolved);
        if let Some(directory) = self.directories.get(path) {
            let mut record = [0; STAT_SIZE];
            record[0..4].copy_from_slice(&1u32.to_le_bytes());
            record[4..8].copy_from_slice(&directory.inode.to_le_bytes());
            record[8..10].copy_from_slice(&(0o040000 | directory.mode).to_le_bytes());
            record[10..12].copy_from_slice(&directory.links.to_le_bytes());
            record[12..16].copy_from_slice(&directory.uid.to_le_bytes());
            record[16..20].copy_from_slice(&directory.gid.to_le_bytes());
            // Virtual directories contain no serialized on-disk blocks.
            record[88..92].copy_from_slice(&512u32.to_le_bytes());
            Ok(record)
        } else {
            Ok(self.backing(path)?.stat())
        }
    }
    fn stat_path(
        &self,
        user: &mut Memory<'_>,
        path: &[u8],
        output: u64,
    ) -> std::result::Result<u64, u32> {
        let record = self.stat_record(path)?;
        if output == 0 {
            return Err(14);
        }
        // Memory::write validates the entire range before touching any byte.
        user.write(output, &record).map_err(|_| 14u32)?;
        Ok(0)
    }
}
fn trimmed(path: &[u8]) -> &[u8] {
    if path.len() > 1 {
        path.strip_suffix(b"/").unwrap_or(path)
    } else {
        path
    }
}
fn parent(path: &[u8]) -> &[u8] {
    let index = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
    if index == 0 { b"/" } else { &path[..index] }
}
fn canonical(path: &[u8]) -> bool {
    path.len() > 1
        && path.len() < PATH_MAX
        && path[0] == b'/'
        && !path.contains(&0)
        && path[1..]
            .split(|b| *b == b'/')
            .all(|part| !part.is_empty() && part != b"." && part != b".." && part.len() <= 255)
}
pub(crate) fn read_path(user: &Memory<'_>, address: u64) -> std::result::Result<Vec<u8>, u32> {
    if address == 0 {
        return Err(14);
    }
    let mut path = Vec::new();
    let mut component = 0;
    for offset in 0..PATH_MAX {
        let byte = user
            .load(address.checked_add(offset as u64).ok_or(14u32)?, 1)
            .map_err(|_| 14u32)? as u8;
        if byte == 0 {
            return Ok(path);
        }
        component = if byte == b'/' { 0 } else { component + 1 };
        if component > 255 {
            return Err(63);
        }
        path.push(byte);
    }
    Err(63)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directories_resolve_components_before_dotdot_and_preserve_type_rules() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.register(b"/rom/file", b"asset".to_vec()).unwrap();
        let plan = files.plan_mkdir(b"new/", Some(b"/save\0"), 0o777).unwrap();
        files.commit_mkdir(plan).unwrap();
        assert_eq!(
            files.resolve(b"./new//../new/", Some(b"/save")),
            Ok(b"/save/new/".to_vec())
        );
        assert_eq!(files.resolve(b"/missing/../save", None), Err(2));
        assert_eq!(files.resolve(b"/rom/file/../save", None), Err(20));
        assert_eq!(files.resolve(b"/rom/file/", None), Err(20));
        assert_eq!(files.resolve(b"/../../save", None), Ok(b"/save".to_vec()));
        for (path, errno) in [
            (&b"/"[..], 21),
            (b"/save/new", 17),
            (b"/rom/file", 17),
            (b"/rom/file/", 20),
            (b"/absent/new", 2),
            (b"/rom/new", 30),
        ] {
            assert!(matches!(files.plan_mkdir(path, None, 0o777), Err(error) if error == errno));
        }
        assert!(files.create(b"/save/new/data/").is_err());
        let backing = files.create(b"/save/new/data").unwrap();
        assert!(backing.writable);
        assert!(files.backing(b"/save/new/../new/data").is_ok());
        assert!(matches!(files.backing(b"/save/new"), Err(21)));
    }
    #[test]
    fn directory_modes_ownership_umask_and_inode_identity_are_coherent() {
        let mut files = Files::default();
        files
            .configure_profile(NamespaceProfile {
                uid: 123,
                gid: 456,
                umask: 0o027,
            })
            .unwrap();
        files.allow_create_directory(b"/save").unwrap();
        assert!(
            files
                .configure_profile(NamespaceProfile::default())
                .is_err()
        );
        let plan = files.plan_mkdir(b"/save/new", None, u16::MAX).unwrap();
        files.commit_mkdir(plan).unwrap();
        let parent = files.stat_record(b"/save").unwrap();
        let directory = files.stat_record(b"/save/new/").unwrap();
        assert_eq!(
            u16::from_le_bytes(directory[8..10].try_into().unwrap()),
            0o40750
        );
        assert_eq!(u16::from_le_bytes(parent[10..12].try_into().unwrap()), 3);
        assert_eq!(
            u32::from_le_bytes(directory[12..16].try_into().unwrap()),
            123
        );
        assert_eq!(
            u32::from_le_bytes(directory[16..20].try_into().unwrap()),
            456
        );
        let backing = files.create(b"/save/new/file").unwrap();
        let regular = backing.stat();
        assert_eq!(
            u16::from_le_bytes(regular[8..10].try_into().unwrap()),
            0o100640
        );
        assert_ne!(directory[4..8], regular[4..8]);
        assert_ne!(parent[4..8], regular[4..8]);
        assert!(files.check_open(b"/save/new/file", true, true).is_ok());
        let plan = files.plan_mkdir(b"/save/locked", None, 0).unwrap();
        files.commit_mkdir(plan).unwrap();
        assert!(matches!(
            files.plan_mkdir(b"/save/locked/child", None, 0o777),
            Err(13)
        ));
        assert_eq!(files.resolve(b"/save/locked/../new", None), Err(13));
        let plan = files.plan_mkdir(b"/save/no_write", None, 0o555).unwrap();
        files.commit_mkdir(plan).unwrap();
        assert!(matches!(
            files.plan_mkdir(b"/save/no_write/child", None, 0o777),
            Err(13)
        ));
        let inode = files.stat_record(b"/save/no_write").unwrap()[4..8].to_vec();
        files
            .register_directory(b"/save/no_write", 0o777, true)
            .unwrap();
        assert_eq!(files.stat_record(b"/save/no_write").unwrap()[4..8], inode);
        assert!(matches!(
            files.plan_mkdir(b"/save/no_write/child", None, 0o777),
            Err(30)
        ));
    }
    #[test]
    fn mkdir_plans_and_host_registration_failures_are_transactional() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        let mut other = Files::default();
        let foreign = files.plan_mkdir(b"/save/foreign", None, 0o777).unwrap();
        assert_eq!(other.commit_mkdir(foreign), Err(22));
        let stale = files.plan_mkdir(b"/save/stale", None, 0o777).unwrap();
        files.register(b"/save/file", vec![1]).unwrap();
        assert_eq!(files.commit_mkdir(stale), Err(22));
        assert!(!files.is_directory(b"/save/stale").unwrap());
        let before = (
            files.directories.len(),
            files.next_inode,
            files.revision,
            files.content_bytes(),
        );
        assert!(files.register(b"/save/file/sub/new", vec![2]).is_err());
        assert!(files.allow_create_directory(b"/save/file/sub").is_err());
        assert_eq!(
            before,
            (
                files.directories.len(),
                files.next_inode,
                files.revision,
                files.content_bytes()
            )
        );
        let prepared = files
            .plan_mkdir(b"/save/after_failure", None, 0o777)
            .unwrap();
        files.commit_mkdir(prepared).unwrap();
        assert!(files.is_directory(b"/save/after_failure").unwrap());
    }
    #[test]
    fn directory_limits_do_not_partially_materialize_ancestors() {
        let mut files = Files::default();
        for index in 1..MAX_DIRECTORIES {
            files
                .allow_create_directory(format!("/d{index}").as_bytes())
                .unwrap();
        }
        let before = (files.directories.len(), files.next_inode, files.revision);
        assert!(files.register(b"/new/sub/file", vec![1]).is_err());
        assert_eq!(
            before,
            (files.directories.len(), files.next_inode, files.revision)
        );
        assert_eq!(files.content_bytes(), 0);
        assert!(matches!(files.plan_mkdir(b"/d1/new", None, 0o777), Err(28)));
        let mut fresh = Files::default();
        fresh.allow_create_directory(b"/save").unwrap();
        let too_long = [b"/save/".as_slice(), &[b'a'; 256]].concat();
        assert_eq!(fresh.resolve(&too_long, None), Err(63));
    }
    #[test]
    fn file_count_and_trace_byte_limits_are_enforced_independently() {
        let mut files = Files::default();
        for index in 0..MAX_FILES {
            files
                .register(format!("/file{index}").as_bytes(), vec![])
                .unwrap();
        }
        assert!(files.register(b"/overflow", vec![]).is_err());
        assert_eq!(files.content_bytes(), 0);
        files.set_stat_tracing(true);
        let mut path = vec![b'x'; 1001];
        for i in (0..1000).step_by(100) {
            path[i] = b'/';
        }
        path[1000] = 0;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut path, false, false).unwrap();
        for _ in 0..100 {
            assert_eq!(files.stat(&mut memory, 0x1000, 0), Err(2));
        }
        let trace = files.take_stat_trace();
        assert_eq!(trace.requests.len(), 65);
        assert_eq!(trace.dropped, 35);
        assert_eq!(
            trace.requests.iter().map(|r| r.path.len()).sum::<usize>(),
            65000
        );
    }
    #[test]
    fn stat_layout_matches_sdk_and_sizes_come_from_owned_contents() {
        let mut files = Files::default();
        files.register(b"/guest/file", vec![0x5a; 513]).unwrap();
        let mut path = *b"/guest/file\0";
        let mut output = [0xa5; STAT_SIZE + 2];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut path, false, false).unwrap();
        memory.add(0x2000, &mut output, true, false).unwrap();
        assert_eq!(files.stat(&mut memory, 0x1000, 0x2001), Ok(0));
        let record = memory.read(0x2001, STAT_SIZE).unwrap();
        assert_eq!(u32::from_le_bytes(record[0..4].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(record[4..8].try_into().unwrap()), 3);
        assert_eq!(
            u16::from_le_bytes(record[8..10].try_into().unwrap()),
            0o100444
        );
        assert_eq!(u16::from_le_bytes(record[10..12].try_into().unwrap()), 1);
        assert_eq!(i64::from_le_bytes(record[72..80].try_into().unwrap()), 513);
        assert_eq!(i64::from_le_bytes(record[80..88].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(record[88..92].try_into().unwrap()), 512);
        assert_eq!(u32::from_le_bytes(record[12..16].try_into().unwrap()), 1000);
        assert_eq!(u32::from_le_bytes(record[16..20].try_into().unwrap()), 1000);
        assert!(record[20..72].iter().all(|b| *b == 0));
        assert!(record[92..120].iter().all(|b| *b == 0));
        assert_eq!(memory.load(0x2000, 1).unwrap(), 0xa5);
        assert_eq!(memory.load(0x2000 + STAT_SIZE as u64 + 1, 1).unwrap(), 0xa5);
        assert_eq!(files.content_bytes(), 513);
    }
    #[test]
    fn stat_failures_are_atomic_and_path_output_alias_is_safe() {
        let mut files = Files::default();
        files.register(b"/file", vec![]).unwrap();
        let mut path = *b"/file\0";
        let mut absent = *b"/missing\0";
        let mut short = [0xa5; STAT_SIZE - 1];
        let mut readonly = [0xa5; STAT_SIZE];
        let mut alias = [0xa5; STAT_SIZE];
        alias[..6].copy_from_slice(b"/file\0");
        let mut memory = Memory::default();
        memory.add(0x1000, &mut path, false, false).unwrap();
        memory.add(0x1100, &mut absent, false, false).unwrap();
        memory.add(0x2000, &mut short, true, false).unwrap();
        memory.add(0x3000, &mut readonly, false, false).unwrap();
        memory.add(0x4000, &mut alias, true, false).unwrap();
        for output in [0, 0x2000, 0x3000, u64::MAX - 10] {
            assert_eq!(files.stat(&mut memory, 0x1000, output), Err(14));
        }
        assert_eq!(files.stat(&mut memory, 0x1100, 0x2000), Err(2));
        for pointer in [0, 0x1006, u64::MAX] {
            assert_eq!(files.stat(&mut memory, pointer, 0x3000), Err(14));
        }
        assert!(
            memory
                .read(0x2000, STAT_SIZE - 1)
                .unwrap()
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert!(
            memory
                .read(0x3000, STAT_SIZE)
                .unwrap()
                .iter()
                .all(|b| *b == 0xa5)
        );
        assert_eq!(files.stat(&mut memory, 0x4000, 0x4000), Ok(0));
        assert_eq!(memory.load(0x4000 + 72, 8).unwrap(), 0);
    }
    #[test]
    fn file_namespace_and_path_and_trace_storage_are_bounded() {
        let mut files = Files::default();
        for path in [
            &b"relative"[..],
            b"/",
            b"/a//b",
            b"/a/../b",
            b"/a/./b",
            b"/a\0b",
        ] {
            assert!(files.register(path, vec![]).is_err());
        }
        files.register(b"/file", vec![1]).unwrap();
        assert!(files.register(b"/file", vec![2]).is_err());
        assert_eq!(files.content_bytes(), 1);
        files.quota.used.set(MAX_CONTENT_BYTES);
        assert!(files.register(b"/over", vec![1]).is_err());
        files.quota.used.set(1);
        let mut path = *b"/missing\0";
        let mut long = vec![b'x'; PATH_MAX];
        for i in (0..PATH_MAX).step_by(100) {
            long[i] = b'/';
        }
        let mut component = vec![b'x'; 257];
        component[256] = 0;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut path, false, false).unwrap();
        memory.add(0x2000, &mut long, false, false).unwrap();
        memory.add(0x3000, &mut component, false, false).unwrap();
        assert_eq!(files.stat(&mut memory, 0x2000, 0), Err(63));
        assert_eq!(files.stat(&mut memory, 0x3000, 0), Err(63));
        assert_eq!(files.stat(&mut memory, 0x1000, 0), Err(2));
        assert!(files.take_stat_trace().requests.is_empty());
        files.set_stat_tracing(true);
        for _ in 0..MAX_TRACE_RECORDS + 3 {
            assert_eq!(files.stat(&mut memory, 0x1000, 0), Err(2));
        }
        let trace = files.take_stat_trace();
        assert_eq!(trace.requests.len(), MAX_TRACE_RECORDS);
        assert_eq!(trace.dropped, 3);
        assert!(
            trace
                .requests
                .iter()
                .all(|r| r.path == b"/missing" && r.error == Some(2))
        );
        assert!(files.take_stat_trace().requests.is_empty());
        files.set_stat_tracing(false);
        files.stat(&mut memory, 0x1000, 0).unwrap_err();
        assert!(files.take_stat_trace().requests.is_empty());
    }
}
