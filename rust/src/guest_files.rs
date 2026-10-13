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
#[cfg(test)]
mod directory_entry_tests {
    use super::*;
    #[test]
    fn retained_directory_follows_move_and_preserves_open_access() {
        let mut files = Files::default();
        files.register_directory(b"/save", 0o777, false).unwrap();
        files
            .register_directory(b"/save/source", 0o777, false)
            .unwrap();
        files.register(b"/save/source/item", vec![]).unwrap();
        let token = files.open_directory(b"/save/source", None).unwrap();
        let before = files.stat_record(b"/save").unwrap();
        {
            let _discarded = files
                .plan_rename(b"/save/source", b"/save/moved", None)
                .unwrap();
            assert_eq!(files.stat_record(b"/save").unwrap(), before);
        }
        let stale = files
            .plan_rename(b"/save/source", b"/save/moved", None)
            .unwrap();
        files.register(b"/save/other", vec![]).unwrap();
        assert_eq!(files.commit_rename(stale), Err(22));
        assert_eq!(files.stat_record(b"/save").unwrap(), before);
        assert_eq!(
            files.directory_entries_for_token(&token).unwrap(),
            files.directory_entries(b"/save/source", None).unwrap()
        );
        let plan = files
            .plan_rename(b"/save/source", b"/save/moved", None)
            .unwrap();
        assert_eq!(
            files.directory_entries_for_token(&token).unwrap()[0].inode,
            token.inode()
        );
        assert_eq!(files.directory_entries(b"/save/moved", None), Err(2));
        files.commit_rename(plan).unwrap();
        files.register_directory(b"/save/moved", 0, false).unwrap();
        assert_eq!(files.directory_entries(b"/save/moved", None), Err(13));
        let entries = files.directory_entries_for_token(&token).unwrap();
        assert_eq!(entries[0].inode, token.inode());
        assert_eq!(entries[2].name, b"item");
        let foreign = Files::default();
        assert!(!foreign.owns_directory_token(&token));
        assert!(
            foreign
                .directory_entries_for_token(&token)
                .unwrap_err()
                .contains("another namespace")
        );
    }
    #[test]
    fn replaced_and_removed_tokens_never_retarget_new_nodes() {
        let mut files = Files::default();
        files.register_directory(b"/save", 0o777, false).unwrap();
        files.register_directory(b"/save/a", 0o777, false).unwrap();
        files.register_directory(b"/save/b", 0o777, false).unwrap();
        let source = files.open_directory(b"/save/a", None).unwrap();
        let victim = files.open_directory(b"/save/b", None).unwrap();
        let plan = files.plan_rename(b"/save/a", b"/save/b", None).unwrap();
        assert_eq!(
            files.directory_entries_for_token(&victim).unwrap()[0].inode,
            victim.inode()
        );
        files.commit_rename(plan).unwrap();
        assert!(
            files
                .directory_entries_for_token(&victim)
                .unwrap_err()
                .contains("detached")
        );
        assert_eq!(
            files.directory_entries_for_token(&source).unwrap()[0].inode,
            source.inode()
        );
        files.remove(b"/save/b", None).unwrap();
        files.register_directory(b"/save/b", 0o777, false).unwrap();
        let fresh = files.open_directory(b"/save/b", None).unwrap();
        assert_ne!(fresh.inode(), source.inode());
        assert!(files.owns_directory_token(&source));
        assert!(
            files
                .directory_entries_for_token(&source)
                .unwrap_err()
                .contains("detached")
        );
        assert_eq!(
            files.directory_entries_for_token(&fresh).unwrap()[0].inode,
            fresh.inode()
        );
    }
    #[test]
    fn listing_is_immediate_ordered_and_preserves_inode_identity() {
        let mut files = Files::default();
        files.register(b"/data/z", vec![]).unwrap();
        files.register(b"/data/sub/nested", vec![]).unwrap();
        files.register(b"/data/a", vec![]).unwrap();
        let listing = files.directory_entries(b"./data/", Some(b"/\0")).unwrap();
        assert_eq!(
            listing
                .iter()
                .map(|e| e.name.as_slice())
                .collect::<Vec<_>>(),
            [b".".as_slice(), b"..", b"a", b"sub", b"z"]
        );
        assert_eq!(
            listing.iter().map(|e| e.directory).collect::<Vec<_>>(),
            [true, true, false, true, false]
        );
        assert_eq!(
            listing[0].inode,
            files.directories[b"/data".as_slice()].inode
        );
        assert_eq!(listing[1].inode, 1);
        assert_eq!(listing[2].inode, files.files[b"/data/a".as_slice()].inode);
        let root = files.directory_entries(b"/", None).unwrap();
        assert_eq!(root.len(), 3);
        assert_eq!(root[0].inode, root[1].inode);
        // Snapshots are owned: future namespace additions cannot alter them.
        files.register(b"/data/b", vec![]).unwrap();
        assert_eq!(listing.len(), 5);
        assert_eq!(files.directory_entries(b"/data", None).unwrap().len(), 6);
    }
    #[test]
    fn listing_permissions_and_lookup_errors() {
        let mut files = Files::default();
        files.register_directory(b"/read", 0o400, true).unwrap();
        assert!(files.directory_entries(b"/read", None).is_ok());
        assert_eq!(files.directory_entries(b"/read/.", None), Err(13));
        files.register_directory(b"/search", 0o100, false).unwrap();
        assert_eq!(files.directory_entries(b"/search", None), Err(13));
        files.register(b"/regular", vec![]).unwrap();
        assert_eq!(files.directory_entries(b"/regular", None), Err(20));
        assert_eq!(files.directory_entries(b"/regular/", None), Err(20));
        assert_eq!(files.directory_entries(b"/absent", None), Err(2));
        assert_eq!(files.directory_entries(b"", None), Err(2));
        assert_eq!(files.directory_entries(b"relative", None), Err(2));
        files
            .register_directory(b"/hidden/child", 0o555, false)
            .unwrap();
        files.register_directory(b"/hidden", 0o400, false).unwrap();
        assert_eq!(files.directory_entries(b"/hidden/child", None), Err(13));
    }
    #[test]
    fn dirent_layout_padding_and_name_limits() {
        for length in 1..=255 {
            let entry = DirectoryEntry {
                inode: 0x12345678,
                directory: length % 2 == 0,
                name: vec![0xff; length],
            };
            let record = entry.record().unwrap();
            assert_eq!(&record[..4], &[0x78, 0x56, 0x34, 0x12]);
            assert_eq!(
                u16::from_le_bytes(record[4..6].try_into().unwrap()) as usize,
                record.len()
            );
            assert_eq!(record.len() % 4, 0);
            assert_eq!(record[6], if entry.directory { 4 } else { 8 });
            assert_eq!(record[7] as usize, length);
            assert_eq!(&record[8..8 + length], &entry.name);
            assert!(record[8 + length..].iter().all(|byte| *byte == 0));
        }
        for name in [vec![], vec![b'x'; 256], b"a/b".to_vec(), b"a\0".to_vec()] {
            assert!(
                DirectoryEntry {
                    inode: 1,
                    directory: false,
                    name
                }
                .record()
                .is_err()
            );
        }
    }
}
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
    links: Cell<u16>,
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
        record[10..12].copy_from_slice(&self.links.get().to_le_bytes());
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
    identity: Rc<()>,
    inode: u32,
    mode: u16,
    uid: u32,
    gid: u32,
    read_only: bool,
    links: u16,
}
/// Retains namespace and node identity, not a pathname or mutable metadata.
/// Cloned namespace plans share identity only; their metadata stays isolated.
#[derive(Clone, Debug)]
pub struct DirectoryToken {
    owner: Rc<()>,
    identity: Rc<()>,
    inode: u32,
}
impl DirectoryToken {
    pub fn inode(&self) -> u32 {
        self.inode
    }
}
pub struct MkdirPlan {
    directories: BTreeMap<Vec<u8>, Directory>,
    next_inode: u32,
    revision: u64,
    owner: Rc<()>,
}
/// Owned namespace enumeration, before any descriptor cursor or guest copy.
/// A snapshot is not a retained directory handle across namespace mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub inode: u32,
    pub directory: bool,
    pub name: Vec<u8>,
}
impl DirectoryEntry {
    /// FreeBSD 11.4 dirent ABI: u32 inode, u16 record length, u8 type,
    /// u8 name length, NUL-terminated name padded to a four-byte boundary.
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/sys/dirent.h
    pub fn record(&self) -> Result<Vec<u8>> {
        if self.name.is_empty()
            || self.name.len() > 255
            || self.name.contains(&0)
            || self.name.contains(&b'/')
        {
            return Err("Invalid directory entry name".into());
        }
        let length = 8 + (self.name.len() + 1).next_multiple_of(4);
        let mut bytes = vec![0; length];
        bytes[..4].copy_from_slice(&self.inode.to_le_bytes());
        bytes[4..6].copy_from_slice(&(length as u16).to_le_bytes());
        bytes[6] = if self.directory { 4 } else { 8 };
        bytes[7] = self.name.len() as u8;
        bytes[8..8 + self.name.len()].copy_from_slice(&self.name);
        Ok(bytes)
    }
}
/// Prepared namespace replacement. Rc file contents remain shared, but no
/// destination link count changes until an owner/revision-checked commit.
pub struct RenamePlan {
    files: BTreeMap<Vec<u8>, Rc<FileBacking>>,
    directories: BTreeMap<Vec<u8>, Directory>,
    victim: Option<Rc<FileBacking>>,
    moved: Option<(Vec<u8>, Vec<u8>)>,
    replaced_directory: Option<Vec<u8>>,
    changed: bool,
    revision: u64,
    owner: Rc<()>,
}
impl RenamePlan {
    pub fn moved_directory(&self) -> Option<(&[u8], &[u8])> {
        self.moved
            .as_ref()
            .map(|(old, new)| (old.as_slice(), new.as_slice()))
    }
    pub fn replaced_directory(&self) -> Option<&[u8]> {
        self.replaced_directory.as_deref()
    }
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
                    identity: Rc::new(()),
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
    /// Enumerate the owned namespace only. Ancestors require search access;
    /// the final directory requires read access, not write permission.
    /// Dot entries precede byte-sorted immediate children. Ordering is a
    /// virtual filesystem policy, not an assertion about console disk order.
    pub fn directory_entries(
        &self,
        path: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<Vec<DirectoryEntry>, u32> {
        let resolved = self.resolve(path, cwd)?;
        let path = trimmed(&resolved);
        let directory = self
            .directories
            .get(path)
            .ok_or(if self.files.contains_key(path) {
                20u32
            } else {
                2
            })?;
        if !self.access(directory, 4) {
            return Err(13);
        }
        self.entries_at(path, directory)
    }
    /// Captures successful traversal and read access. Enumeration through the
    /// token does not repeat pathname permission checks after opening.
    pub fn open_directory(
        &self,
        path: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<DirectoryToken, u32> {
        let resolved = self.resolve(path, cwd)?;
        let path = trimmed(&resolved);
        let directory = self
            .directories
            .get(path)
            .ok_or(if self.files.contains_key(path) {
                20u32
            } else {
                2
            })?;
        if !self.access(directory, 4) {
            return Err(13);
        }
        Ok(DirectoryToken {
            owner: self.owner.clone(),
            identity: directory.identity.clone(),
            inode: directory.inode,
        })
    }
    pub fn owns_directory_token(&self, token: &DirectoryToken) -> bool {
        Rc::ptr_eq(&self.owner, &token.owner)
    }
    /// Resolves retained identity after rename, never the former pathname.
    /// Reading an unlinked/replaced directory is currently unsupported: no
    /// claim is made about filesystem-specific detached-directory contents.
    pub fn directory_entries_for_token(
        &self,
        token: &DirectoryToken,
    ) -> Result<Vec<DirectoryEntry>> {
        if !self.owns_directory_token(token) {
            return Err("Directory token belongs to another namespace".into());
        }
        let (path, directory) = self
            .directories
            .iter()
            .find(|(_, directory)| Rc::ptr_eq(&directory.identity, &token.identity))
            .ok_or("Enumeration of a detached directory is unsupported")?;
        self.entries_at(path, directory)
            .map_err(|_| "Retained directory parent is missing".into())
    }
    fn entries_at(
        &self,
        path: &[u8],
        directory: &Directory,
    ) -> std::result::Result<Vec<DirectoryEntry>, u32> {
        let parent_directory = self.directories.get(parent(path)).ok_or(2u32)?;
        let mut entries = vec![
            DirectoryEntry {
                inode: directory.inode,
                directory: true,
                name: b".".to_vec(),
            },
            DirectoryEntry {
                inode: parent_directory.inode,
                directory: true,
                name: b"..".to_vec(),
            },
        ];
        let mut children = BTreeMap::new();
        for (name, node) in &self.directories {
            if name.as_slice() != path && parent(name) == path {
                children.insert(
                    name.rsplit(|b| *b == b'/').next().unwrap().to_vec(),
                    (node.inode, true),
                );
            }
        }
        for (name, node) in &self.files {
            if parent(name) == path {
                children.insert(
                    name.rsplit(|b| *b == b'/').next().unwrap().to_vec(),
                    (node.inode, false),
                );
            }
        }
        entries.extend(
            children
                .into_iter()
                .map(|(name, (inode, directory))| DirectoryEntry {
                    inode,
                    directory,
                    name,
                }),
        );
        Ok(entries)
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
                        identity: Rc::new(()),
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
                identity: Rc::new(()),
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
    /// Remove a guest namespace entry; never consult or delete a host path.
    /// FreeBSD11.4 libc remove.c performs lstat first, then rmdir for directories
    /// or unlink otherwise. Resolve/type/existence checks therefore precede
    /// deletion permission checks. No symlink, hardlink, mountpoint, ACL, inode
    /// flags or sticky-directory support is claimed: registration rejects sticky
    /// modes, and the existing guest mkdir profile keeps permission bits only.
    /// Directory read_only controls mutation of its children, not a mount root;
    /// a readonly regular file can be unlinked from a writable parent.
    /// Cwd is supplied as a pathname, not a retained directory inode: relative
    /// traversal after unlinking the cwd by absolute name is outside this profile.
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/remove.c
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_lookup.c
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_syscalls.c
    pub fn remove(&mut self, path: &[u8], cwd: Option<&[u8]>) -> std::result::Result<(), u32> {
        // Lookup must preserve errors in intermediates before dot/dotdot
        // normalization. In particular, missing/../existing is not existing.
        let resolved = self.resolve(path, cwd)?;
        let resolved = trimmed(&resolved);
        let is_directory = self.directories.contains_key(resolved);
        if !is_directory && !self.files.contains_key(resolved) {
            return Err(2);
        }
        // DELETE lookup rejects these final spellings even when ordinary
        // lstat/lookup successfully resolves them to an existing directory.
        match path
            .rsplit(|&byte| byte == b'/')
            .find(|part| !part.is_empty())
        {
            None => return Err(21), // all-slash root: EISDIR before rmdir's root test
            Some(b"." | b"..") => return Err(22),
            _ => (),
        }
        let p = parent(resolved);
        let directory = self.directories.get(p).ok_or(2u32)?;
        if directory.read_only {
            return Err(30);
        }
        if !self.access(directory, 3) {
            return Err(13);
        }
        let parent_links = if is_directory {
            if self
                .files
                .keys()
                .chain(self.directories.keys())
                .any(|child| child.as_slice() != resolved && parent(child) == resolved)
            {
                return Err(66); // ENOTEMPTY
            }
            Some(
                directory
                    .links
                    .checked_sub(1)
                    .filter(|&n| n >= 2)
                    .ok_or(22u32)?,
            )
        } else {
            None
        };
        let revision = self.revision.checked_add(1).ok_or(28u32)?;
        // No fallible work follows namespace publication. Open descriptors keep
        // their Rc backing, inode and content; final Rc drop releases its quota.
        if let Some(links) = parent_links {
            self.directories
                .remove(resolved)
                .expect("Checked directory");
            self.directories.get_mut(p).expect("Checked parent").links = links;
        } else {
            let backing = self.files.remove(resolved).expect("Checked file");
            backing.links.set(0);
        }
        self.revision = revision;
        Ok(())
    }
    // DELETE/RENAME lookup differs from ordinary lookup: terminal dot/dotdot
    // fail before looking up that final component, while trailing slash type
    // checks follow parent mutation permissions. Do not lose either spelling.
    fn rename_lookup(
        &self,
        path: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<(Vec<u8>, bool), u32> {
        if path.is_empty() {
            return Err(2);
        }
        if path.len() >= PATH_MAX || path.contains(&0) {
            return Err(63);
        }
        let end = path
            .iter()
            .rposition(|&b| b != b'/')
            .map(|i| i + 1)
            .ok_or(21u32)?;
        let plain = &path[..end];
        let start = plain
            .iter()
            .rposition(|&b| b == b'/')
            .map(|i| i + 1)
            .unwrap_or(0);
        if matches!(&plain[start..], b"." | b"..") {
            let prefix = if start == 0 {
                let cwd = cwd.ok_or(2u32)?;
                self.resolve(cwd.strip_suffix(&[0]).unwrap_or(cwd), None)?
            } else {
                self.resolve(&plain[..start], cwd)?
            };
            let prefix = trimmed(&prefix);
            if !self.directories.contains_key(prefix) {
                return Err(if self.files.contains_key(prefix) {
                    20
                } else {
                    2
                });
            }
            return Err(22);
        }
        Ok((self.resolve(plain, cwd)?, end != path.len()))
    }
    fn rename_parent_access(&self, path: &[u8]) -> std::result::Result<(), u32> {
        let directory = self.directories.get(parent(path)).ok_or(2u32)?;
        if directory.read_only {
            return Err(30);
        }
        if !self.access(directory, 3) {
            return Err(13);
        }
        Ok(())
    }
    fn rename_source(
        &self,
        old: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<(Vec<u8>, bool), u32> {
        let (resolved, trailing) = self.rename_lookup(old, cwd)?;
        let directory = self.directories.contains_key(&resolved);
        if !directory && !self.files.contains_key(&resolved) {
            return Err(2);
        }
        self.rename_parent_access(&resolved)?;
        if trailing && !directory {
            return Err(20);
        }
        Ok((resolved, directory))
    }
    /// Kernel reads the source guest path and performs this preflight before
    /// reading the destination guest path, matching rename's ordered namei calls.
    pub(crate) fn check_rename_source(
        &self,
        old: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<(), u32> {
        self.rename_source(old, cwd).map(|_| ())
    }
    /// Source-audited FreeBSD11.4 rename within the existing virtual profile.
    /// No hardlinks, symlinks, mount boundaries, sticky modes, inode flags, ACLs
    /// or privilege bypass are added. Readonly-directory policy is per-node.
    /// A cross-parent directory move needs write access to that directory to
    /// change its logical '..'. Input paths retain PATH_MAX checks, but moving
    /// an ancestor may produce longer stored descendant names, as on a real FS.
    /// The kernel must prepare any pathname-based cwd update before committing.
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_syscalls.c
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/ufs/ufs/ufs_vnops.c
    /// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_lookup.c
    pub fn plan_rename(
        &self,
        old: &[u8],
        new: &[u8],
        cwd: Option<&[u8]>,
    ) -> std::result::Result<RenamePlan, u32> {
        let (old, source_directory) = self.rename_source(old, cwd)?;
        let (new, trailing) = self
            .rename_lookup(new, cwd)
            .map_err(|e| if e == 21 && source_directory { 22 } else { e })?;
        let target_directory = self.directories.contains_key(&new);
        let target_file = self.files.contains_key(&new);
        self.rename_parent_access(&new)?;
        if trailing && !target_directory {
            if target_file {
                return Err(20);
            }
            if !source_directory {
                return Err(2);
            }
        }
        if source_directory && target_file {
            return Err(20);
        }
        if !source_directory && target_directory {
            return Err(21);
        }
        let changed = old != new;
        // Both lookups and their delete/create permission checks precede the
        // same-inode success. No source name is fabricated for a missing path.
        if changed && source_directory {
            if old == parent(&new) {
                return Err(22);
            }
            if parent(&old) != parent(&new) {
                let source = self
                    .directories
                    .get(&old)
                    .expect("Checked source directory");
                if source.read_only {
                    return Err(30);
                }
                if !self.access(source, 2) {
                    return Err(13);
                }
                if descendant(&new, &old) {
                    return Err(22);
                }
            }
            if target_directory
                && self
                    .files
                    .keys()
                    .chain(self.directories.keys())
                    .any(|child| child.as_slice() != new && parent(child) == new.as_slice())
            {
                return Err(66);
            }
        }
        if changed {
            self.revision.checked_add(1).ok_or(28u32)?;
        }
        let mut plan = RenamePlan {
            files: self.files.clone(),
            directories: self.directories.clone(),
            victim: None,
            moved: None,
            replaced_directory: None,
            changed,
            revision: self.revision,
            owner: self.owner.clone(),
        };
        if !changed {
            return Ok(plan);
        }
        if source_directory {
            if target_directory {
                plan.directories
                    .remove(&new)
                    .expect("Checked target directory");
                plan.replaced_directory = Some(new.clone());
            }
            // Construct all remapped names before touching the live namespace.
            // Rc clones preserve descriptor identity and concurrent content edits.
            for path in self
                .directories
                .keys()
                .filter(|path| path.as_slice() == old || descendant(path, &old))
            {
                let renamed = renamed_path(path, &old, &new);
                let directory = plan
                    .directories
                    .remove(path)
                    .expect("Owned moved directory");
                plan.directories.insert(renamed, directory);
            }
            for path in self.files.keys().filter(|path| descendant(path, &old)) {
                let renamed = renamed_path(path, &old, &new);
                let file = plan.files.remove(path).expect("Owned moved file");
                plan.files.insert(renamed, file);
            }
            let from = plan.directories.get_mut(parent(&old)).ok_or(22u32)?;
            from.links = from.links.checked_sub(1).filter(|&n| n >= 2).ok_or(22u32)?;
            if !target_directory {
                let to = plan.directories.get_mut(parent(&new)).ok_or(22u32)?;
                to.links = to.links.checked_add(1).ok_or(31u32)?;
            }
            plan.moved = Some((old, new));
        } else {
            let file = plan.files.remove(&old).expect("Checked source file");
            plan.victim = plan.files.insert(new, file);
        }
        Ok(plan)
    }
    pub fn commit_rename(&mut self, plan: RenamePlan) -> std::result::Result<(), u32> {
        if !Rc::ptr_eq(&self.owner, &plan.owner) || self.revision != plan.revision {
            return Err(22);
        }
        if !plan.changed {
            return Ok(());
        }
        let revision = self.revision.checked_add(1).ok_or(28u32)?;
        // Only the destination loses its namespace link. The source backing is
        // shared between maps, so open descriptor identity/content is preserved.
        if let Some(victim) = plan.victim {
            victim.links.set(0);
        }
        self.files = plan.files;
        self.directories = plan.directories;
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
                links: Cell::new(1),
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
fn descendant(path: &[u8], directory: &[u8]) -> bool {
    path.starts_with(directory) && path.get(directory.len()) == Some(&b'/')
}
fn renamed_path(path: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut renamed = new.to_vec();
    renamed.extend_from_slice(&path[old.len()..]);
    renamed
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
    fn rename_replacement_preserves_source_and_retains_unlinked_destination_until_close() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files
            .register_writable(b"/save/a", b"abc".to_vec())
            .unwrap();
        files
            .register_writable(b"/save/b", b"defg".to_vec())
            .unwrap();
        let source = files.backing(b"/save/a").unwrap();
        let victim = files.backing(b"/save/b").unwrap();
        let plan = files.plan_rename(b"a", b"b", Some(b"/save\0")).unwrap();
        assert!(plan.moved_directory().is_none());
        assert!(plan.replaced_directory().is_none());
        assert_eq!(victim.stat()[10..12], [1, 0]);
        assert!(Rc::ptr_eq(&files.backing(b"/save/a").unwrap(), &source));
        assert!(Rc::ptr_eq(&files.backing(b"/save/b").unwrap(), &victim));
        source.state.borrow_mut().bytes[0] = b'A'; // contents remain shared after planning
        files.commit_rename(plan).unwrap();
        assert!(matches!(files.backing(b"/save/a"), Err(2)));
        let renamed = files.backing(b"/save/b").unwrap();
        assert!(Rc::ptr_eq(&renamed, &source));
        assert_eq!(&*renamed.state.borrow().bytes, b"Abc");
        assert_eq!(renamed.stat()[10..12], [1, 0]);
        assert_eq!(victim.stat()[10..12], [0, 0]);
        assert_eq!(&*victim.state.borrow().bytes, b"defg");
        assert_eq!(files.content_bytes(), 7);
        drop(victim);
        assert_eq!(files.content_bytes(), 3);
    }
    #[test]
    fn rename_plans_are_discardable_and_reject_stale_or_foreign_publication() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.register(b"/save/a", vec![1]).unwrap();
        files.register(b"/save/b", vec![2, 3]).unwrap();
        let victim = files.backing(b"/save/b").unwrap();
        drop(files.plan_rename(b"/save/a", b"/save/b", None).unwrap());
        assert_eq!(victim.stat()[10..12], [1, 0]);
        let foreign = files.plan_rename(b"/save/a", b"/save/b", None).unwrap();
        assert_eq!(Files::default().commit_rename(foreign), Err(22));
        let stale = files.plan_rename(b"/save/a", b"/save/b", None).unwrap();
        files.register(b"/save/c", vec![4]).unwrap();
        assert_eq!(files.commit_rename(stale), Err(22));
        assert_eq!(victim.stat()[10..12], [1, 0]);
        assert_eq!(files.content_bytes(), 4);
        assert_eq!(
            &*files.backing(b"/save/a").unwrap().state.borrow().bytes,
            &[1]
        );
        assert!(Rc::ptr_eq(&files.backing(b"/save/b").unwrap(), &victim));
    }
    #[test]
    fn rename_directory_subtree_preserves_identities_metadata_and_parent_links() {
        let mut files = Files::default();
        for path in [
            &b"/save"[..],
            b"/save/from",
            b"/save/to",
            b"/save/from/tree",
            b"/save/from/tree/sub",
            b"/save/to/empty",
        ] {
            files.allow_create_directory(path).unwrap();
        }
        files
            .register(b"/save/from/tree/sub/data", vec![9])
            .unwrap();
        let file = files.backing(b"/save/from/tree/sub/data").unwrap();
        let tree = files.stat_record(b"/save/from/tree").unwrap();
        let sub = files.stat_record(b"/save/from/tree/sub").unwrap();
        let plan = files
            .plan_rename(b"/save/from/tree", b"/save/to/empty/", None)
            .unwrap();
        assert_eq!(
            plan.moved_directory(),
            Some((b"/save/from/tree".as_slice(), b"/save/to/empty".as_slice()))
        );
        assert_eq!(
            plan.replaced_directory(),
            Some(b"/save/to/empty".as_slice())
        );
        files.commit_rename(plan).unwrap();
        assert_eq!(files.stat_record(b"/save/to/empty").unwrap(), tree);
        assert_eq!(files.stat_record(b"/save/to/empty/sub").unwrap(), sub);
        assert!(Rc::ptr_eq(
            &files.backing(b"/save/to/empty/sub/data").unwrap(),
            &file
        ));
        assert_eq!(files.stat_record(b"/save/from").unwrap()[10..12], [2, 0]);
        assert_eq!(files.stat_record(b"/save/to").unwrap()[10..12], [3, 0]);
        assert!(matches!(files.stat_record(b"/save/from/tree"), Err(2)));
        let plan = files
            .plan_rename(b"/save/to/empty", b"/save/to/new///", None)
            .unwrap();
        assert!(plan.replaced_directory().is_none());
        files.commit_rename(plan).unwrap();
        assert_eq!(files.stat_record(b"/save/to").unwrap()[10..12], [3, 0]);
        files.allow_create_directory(b"/save/to/other").unwrap();
        let plan = files
            .plan_rename(b"/save/to/new", b"/save/to/other", None)
            .unwrap();
        files.commit_rename(plan).unwrap();
        assert_eq!(files.stat_record(b"/save/to").unwrap()[10..12], [3, 0]);
    }
    #[test]
    fn rename_errors_and_noop_require_ordered_lookup_permissions_and_type_checks() {
        let mut files = Files::default();
        for path in [&b"/save"[..], b"/save/dir", b"/save/full", b"/save/dir/sub"] {
            files.allow_create_directory(path).unwrap();
        }
        files.register(b"/save/file", vec![1]).unwrap();
        files.register(b"/save/full/data", vec![2]).unwrap();
        files.register(b"/rom/file", vec![3]).unwrap();
        for (old, new, error) in [
            (&b"/absent"[..], &b"/also_absent"[..], 2),
            (b"/rom/file", b"/rom/file", 30),
            (b"/save/file", b"/save/dir", 21),
            (b"/save/dir", b"/save/file", 20),
            (b"/save/dir", b"/save/full", 66),
            (b"/save/dir", b"/save/dir/sub/new", 22),
            (b"/save/dir", b"/save/dir/new", 22),
            (b"/save/file", b"/save/absent/", 2),
            (b"/save/file", b"/save/file/", 20),
            (b"/save/file/", b"/save/new", 20),
            (b"/save/.", b"/save/new", 22),
            (b"/save/..", b"/save/new", 22),
            (b"/save/file", b"/save/.", 22),
            (b"/save/file", b"/save/..", 22),
            (b"/", b"/save/new", 21),
            (b"/save/file", b"/", 21),
            (b"/save/dir", b"/", 22),
        ] {
            let before = files.revision;
            assert!(
                matches!(files.plan_rename(old,new,None),Err(e) if e == error),
                "{old:?} -> {new:?}: expected {error}"
            );
            assert_eq!(files.revision, before);
            assert_eq!(files.content_bytes(), 3);
        }
        let before = files.revision;
        let plan = files
            .plan_rename(b"/save/dir/../file", b"/save/file", None)
            .unwrap();
        assert!(plan.moved_directory().is_none());
        assert!(plan.replaced_directory().is_none());
        files.commit_rename(plan).unwrap();
        assert_eq!(files.revision, before);
        files
            .register_directory(b"/save/dir", 0o555, false)
            .unwrap();
        assert!(matches!(
            files.plan_rename(b"/save/dir", b"/save/full/new", None),
            Err(13)
        ));
        files.register_directory(b"/save", 0o555, false).unwrap();
        assert_eq!(files.check_rename_source(b"/save/file/", None), Err(13)); // access precedes trailing type
        files.register_directory(b"/save/dir", 0, false).unwrap();
        assert_eq!(files.check_rename_source(b"/save/dir/.", None), Err(22)); // namei rejects before final search
    }
    #[test]
    fn renamed_descendant_keys_can_exceed_pathmax_without_rejecting_valid_inputs() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.allow_create_directory(b"/save/a").unwrap();
        let mut leaf = b"/save/a".to_vec();
        for _ in 0..3 {
            leaf.push(b'/');
            leaf.extend_from_slice(&[b'x'; 250]);
            files.allow_create_directory(&leaf).unwrap();
        }
        leaf.extend_from_slice(b"/data");
        files.register(&leaf, vec![4]).unwrap();
        let backing = files.backing(&leaf).unwrap();
        let mut destination = b"/save/".to_vec();
        destination.extend_from_slice(&[b'y'; 250]);
        files.allow_create_directory(&destination).unwrap();
        destination.push(b'/');
        destination.extend_from_slice(&[b'z'; 250]);
        let plan = files.plan_rename(b"/save/a", &destination, None).unwrap();
        files.commit_rename(plan).unwrap();
        let new_leaf = renamed_path(&leaf, b"/save/a", &destination);
        assert!(new_leaf.len() >= PATH_MAX);
        assert!(Rc::ptr_eq(files.files.get(&new_leaf).unwrap(), &backing));
        assert_eq!(files.content_bytes(), 1);
    }
    #[test]
    fn unlink_retains_open_backing_inode_zero_links_and_quota_until_last_close() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files
            .register_writable(b"/save/file", b"old".to_vec())
            .unwrap();
        let live = files.backing(b"/save/file").unwrap();
        let second = live.clone();
        let old_inode = live.stat()[4..8].to_vec();
        files.remove(b"file", Some(b"/save\0")).unwrap();
        assert!(matches!(files.backing(b"/save/file"), Err(2)));
        assert_eq!(live.stat()[10..12], [0, 0]);
        assert_eq!(live.stat()[4..8], old_inode);
        assert_eq!(&*live.state.borrow().bytes, b"old");
        assert_eq!(files.content_bytes(), 3);
        files
            .register_writable(b"/save/file", b"new!".to_vec())
            .unwrap();
        let recreated = files.backing(b"/save/file").unwrap();
        assert_ne!(recreated.stat()[4..8], old_inode);
        assert_eq!(recreated.stat()[10..12], [1, 0]);
        assert_eq!(files.content_bytes(), 7);
        live.state.borrow_mut().bytes[0] = b'O';
        assert_eq!(&*recreated.state.borrow().bytes, b"new!");
        drop(live);
        assert_eq!(files.content_bytes(), 7);
        drop(second);
        assert_eq!(files.content_bytes(), 4);
        drop(recreated);
        files.remove(b"/save/file", None).unwrap();
        assert_eq!(files.content_bytes(), 0);
    }
    #[test]
    fn remove_directories_requires_empty_children_and_updates_parent_link_count() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.allow_create_directory(b"/save/dir").unwrap();
        files.register(b"/save/dir/readonly", vec![1]).unwrap();
        assert_eq!(files.remove(b"/save/dir/", None), Err(66));
        files.remove(b"/save/dir/readonly", None).unwrap(); // target content mode is irrelevant
        files.allow_create_directory(b"/save/dir/sub").unwrap();
        assert_eq!(files.remove(b"/save/dir", None), Err(66));
        files.remove(b"/save/dir/sub///", None).unwrap();
        assert_eq!(files.stat_record(b"/save").unwrap()[10..12], [3, 0]);
        files.register_directory(b"/save/dir", 0o555, true).unwrap();
        files.remove(b"/save/dir", None).unwrap(); // readonly child is not a mounted root
        assert_eq!(files.stat_record(b"/save").unwrap()[10..12], [2, 0]);
        assert_eq!(files.content_bytes(), 0);
    }
    #[test]
    fn remove_error_order_preserves_dot_spellings_and_actual_lookup_failures() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.register(b"/save/file", vec![1]).unwrap();
        files.register_directory(b"/locked", 0, false).unwrap();
        for (path, errno) in [
            (&b"/"[..], 21),
            (b"///", 21),
            (b"/save/.", 22),
            (b"/save/..", 22),
            (b"/save/../", 22),
            (b"/save/file/", 20),
            (b"/save/file/.", 20),
            (b"/missing/../save", 2),
            (b"/missing", 2),
            (b"/save/no/", 2),
            (b"/locked/.", 13),
            (b"/locked/../save", 13),
            (b"", 2),
        ] {
            let revision = files.revision;
            assert_eq!(files.remove(path, None), Err(errno), "{:?}", path);
            assert_eq!(files.revision, revision);
            assert_eq!(files.content_bytes(), 1);
        }
        assert_eq!(files.remove(b".", Some(b"/save")), Err(22));
        assert_eq!(files.remove(b"../", Some(b"/save")), Err(22));
        assert!(files.register_directory(b"/sticky", 0o1777, false).is_err());
    }
    #[test]
    fn remove_parent_policy_and_revision_checks_precede_mutation() {
        let mut files = Files::default();
        files.register(b"/rom/file", vec![1, 2]).unwrap();
        assert_eq!(files.remove(b"/rom/file", None), Err(30));
        assert_eq!(files.remove(b"/rom/missing", None), Err(2)); // lstat first
        files.register_directory(b"/rom", 0o555, false).unwrap();
        assert_eq!(files.remove(b"/rom/file", None), Err(13));
        files.allow_create_directory(b"/rom").unwrap();
        let stale = files.plan_mkdir(b"/rom/new", None, 0o777).unwrap();
        files.remove(b"/rom/file", None).unwrap();
        assert_eq!(files.commit_mkdir(stale), Err(22));
        files.register(b"/rom/file", vec![3]).unwrap();
        files.revision = u64::MAX;
        assert_eq!(files.remove(b"/rom/file", None), Err(28));
        assert_eq!(files.content_bytes(), 1);
        assert_eq!(files.backing(b"/rom/file").unwrap().stat()[10..12], [1, 0]);
    }
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
