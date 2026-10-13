//! Bounded process-local descriptors for registered files and control pairs.
//! Writable files share memory-backed contents, cursor ownership and quota accounting.
//! No host filesystem, sockets, or network access.
//! File ABI sources: pinned SDK sys/syscall.h, sys/sysproto.h, sys/errno.h at
//! 10b539de8f2b6773a9a78caa7576a97dbd99b29a; FreeBSD releng/11.4
//! sys/kern/vfs_vnops.c::vn_seek and sys/kern/sys_generic.c::sys_read.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_vnops.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/sys_generic.c
//! Guarded copies are all-or-nothing; this does not model partial EFAULT copies.
use crate::{Result, guest_memory::Memory};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

const LIMIT: usize = 256;
/// Process-table and open-instance identity; fd reuse cannot revive this token.
#[derive(Clone, Debug)]
pub struct FileToken {
    descriptor: u32,
    identity: u64,
    owner: Rc<()>,
}
impl FileToken {
    pub fn descriptor(&self) -> u32 {
        self.descriptor
    }
}
/// Internal directory handle identity. Not yet a guest open/read/seek ABI.
#[derive(Clone, Debug)]
pub struct DirectoryDescriptorToken {
    descriptor: u32,
    identity: u64,
    owner: Rc<()>,
}
impl DirectoryDescriptorToken {
    pub fn descriptor(&self) -> u32 {
        self.descriptor
    }
}
pub struct DirectoryReadPlan {
    bytes: Vec<u8>,
    token: DirectoryDescriptorToken,
    position: usize,
    next_position: usize,
    revision: u64,
    namespace: crate::kernel::DirectoryRevision,
    eof: bool,
}
impl DirectoryReadPlan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn eof(&self) -> bool {
        self.eof
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn next_position(&self) -> usize {
        self.next_position
    }
}
struct OpenDirectory {
    backing: crate::kernel::DirectoryToken,
    identity: u64,
    position: Cell<usize>,
    revision: Cell<u64>,
    namespace: RefCell<Option<crate::kernel::DirectoryRevision>>,
}
pub struct FileReadPlan {
    bytes: Vec<u8>,
    token: FileToken,
    position: u64,
    revision: u64,
    file_revision: u64,
}
/// A delimiter-limited read from an owned regular file. EOF means the line
/// operation attempted to read beyond the available bytes, not merely that
/// the final consumed byte happened to be the last byte of the file.
pub struct FileLineReadPlan {
    read: FileReadPlan,
    eof: bool,
}
impl FileLineReadPlan {
    pub fn bytes(&self) -> &[u8] {
        self.read.bytes()
    }
    pub fn eof(&self) -> bool {
        self.eof
    }
}
impl FileReadPlan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod stdin_line_tests {
    use super::*;
    use crate::kernel::Files;

    #[test]
    fn redirected_stdin_shares_raw_cursor_and_retirement() {
        let mut files = Files::default();
        files
            .register(b"/input", b"first\nsecond\n".to_vec())
            .unwrap();
        let mut descriptors = Descriptors::default();
        assert!(descriptors.file_token(0).is_err());
        assert!(
            descriptors
                .configure_stdin_from_registered_file(&files, b"/missing")
                .is_err()
        );
        let token = descriptors
            .configure_stdin_from_registered_file(&files, b"/input")
            .unwrap();
        assert_eq!(token.descriptor(), 0);
        assert_eq!(descriptors.kind(0), Some(DescriptorKind::RegisteredFile));
        assert!(
            descriptors
                .configure_stdin_from_registered_file(&files, b"/input")
                .is_err()
        );
        assert_eq!(
            descriptors
                .open_file(&files, b"/input")
                .unwrap()
                .descriptor(),
            3
        );
        let plan = descriptors.preview_line_read(&token, 100).unwrap();
        assert_eq!(plan.bytes(), b"first\n");
        assert!(!plan.eof());
        assert_eq!(descriptors.file_position(&token), Ok(0));
        descriptors.commit_line_read(&plan).unwrap();
        assert_eq!(descriptors.commit_line_read(&plan), Err(9));
        let mut bytes = [0; 7];
        let mut user = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        let mut kernel = Memory::default();
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, 0, 0x1000, 7, false),
            Ok(7)
        );
        assert_eq!(user.read(0x1000, 7).unwrap(), b"second\n");
        assert_eq!(descriptors.file_position(&token), Ok(13));
        assert_eq!(descriptors.seek(0, 0, 0), Ok(0));
        let pending = descriptors.preview_line_read(&token, 100).unwrap();
        assert_eq!(descriptors.close(0), 0);
        let replacement = descriptors
            .configure_stdin_from_registered_file(&files, b"/input")
            .unwrap();
        assert_eq!(descriptors.commit_line_read(&pending), Err(9));
        assert_eq!(descriptors.close_file(&token), 9);
        assert_eq!(descriptors.file_position(&replacement), Ok(0));
        assert_eq!(descriptors.close_file(&replacement), 0);
        assert_eq!(descriptors.close(0), 9);
    }

    #[test]
    fn line_limits_and_eof_match_bytewise_oracle() {
        for contents in [&b""[..], b"a", b"a\n", b"\n\nb", b"a\0b\ncd", b"abcdef"] {
            let mut files = Files::default();
            files.register(b"/input", contents.to_vec()).unwrap();
            let mut descriptors = Descriptors::default();
            let token = descriptors
                .configure_stdin_from_registered_file(&files, b"/input")
                .unwrap();
            for offset in 0..=contents.len() + 2 {
                for capacity in [0, 1, 2, 3, 8, u64::MAX] {
                    descriptors.seek(0, offset as i64, 0).unwrap();
                    let mut expected = Vec::new();
                    let mut cursor = offset;
                    let mut eof = false;
                    while (expected.len() as u64) < capacity {
                        let Some(&byte) = contents.get(cursor) else {
                            eof = true;
                            break;
                        };
                        expected.push(byte);
                        cursor += 1;
                        if byte == b'\n' {
                            break;
                        }
                    }
                    let plan = descriptors.preview_line_read(&token, capacity).unwrap();
                    assert_eq!(
                        plan.bytes(),
                        expected,
                        "offset={offset}, capacity={capacity}"
                    );
                    assert_eq!(plan.eof(), eof, "offset={offset}, capacity={capacity}");
                    assert_eq!(descriptors.file_position(&token), Ok(offset as u64));
                    descriptors.commit_line_read(&plan).unwrap();
                    assert_eq!(descriptors.file_position(&token), Ok(cursor as u64));
                    assert_eq!(descriptors.commit_line_read(&plan), Err(9));
                }
            }
        }
    }

    #[test]
    fn stdin_quota_domain_and_line_plan_staleness() {
        let mut files = Files::default();
        files
            .register_writable(b"/input", b"a\nb".to_vec())
            .unwrap();
        let mut descriptors = Descriptors::default();
        let token = descriptors
            .configure_stdin_from_registered_file(&files, b"/input")
            .unwrap();
        let plan = descriptors.preview_line_read(&token, 100).unwrap();
        let mut foreign = Descriptors::default();
        foreign
            .configure_stdin_from_registered_file(&files, b"/input")
            .unwrap();
        assert_eq!(foreign.commit_line_read(&plan), Err(9));
        let mut other_files = Files::default();
        other_files.register(b"/other", b"z".to_vec()).unwrap();
        assert!(descriptors.open_file(&other_files, b"/other").is_err());
        assert_eq!(descriptors.registered_file_bytes(), 3);
        let writer = descriptors
            .open_file_options(
                &mut files,
                b"/input",
                OpenOptions {
                    read: false,
                    write: true,
                    ..OpenOptions::read_only()
                },
            )
            .unwrap()
            .unwrap();
        assert!(descriptors.preview_line_read(&writer, 10).is_err());
        let write = descriptors.preview_write_count(&writer, b"x", 1).unwrap();
        descriptors.commit_write(write).unwrap();
        assert_eq!(descriptors.commit_line_read(&plan), Err(9));
        assert_eq!(descriptors.file_position(&token), Ok(0));
        let plan = descriptors.preview_line_read(&token, 10).unwrap();
        descriptors.seek(0, 1, 0).unwrap();
        assert_eq!(descriptors.commit_line_read(&plan), Err(9));
        assert_eq!(descriptors.file_position(&token), Ok(1));
        assert_eq!(descriptors.close_file(&writer), 0);
        assert!(descriptors.open_file(&other_files, b"/other").is_err());
        assert_eq!(descriptors.close(0), 0);
        assert!(descriptors.open_file(&other_files, b"/other").is_ok());
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
    pub exclusive: bool,
    pub cloexec: bool,
}
impl OpenOptions {
    pub fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
            cloexec: false,
        }
    }
}
pub struct FileWritePlan {
    token: FileToken,
    cursor: u64,
    position: u64,
    revision: u64,
    file_revision: u64,
    quota_revision: u64,
    replacement: Box<[u8]>,
    amount: usize,
    terminal_error: Option<u32>,
}
impl FileWritePlan {
    pub fn bytes_written(&self) -> usize {
        self.amount
    }
    pub fn terminal_error(&self) -> Option<u32> {
        self.terminal_error
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    Directory,
    RegisteredFile,
    Pipe,
    Control,
}
struct OpenFile {
    backing: Rc<crate::kernel::FileBacking>,
    options: OpenOptions,
    identity: u64,
    position: Cell<u64>,
    revision: Cell<u64>,
}

enum Endpoint {
    File(OpenFile),
    Directory(OpenDirectory),
    Control {
        target: Rc<Cell<u64>>,
        selector: bool,
    },
    Pipe {
        state: Rc<Pipe>,
        writer: bool,
    },
}
struct Pipe {
    control: u64,
    reader_open: Cell<bool>,
    writer_open: Cell<bool>,
}
pub struct Descriptors {
    endpoints: [Option<Endpoint>; LIMIT],
    stdin: Option<Endpoint>,
    owner: Rc<()>,
    next_file: u64,
    file_quota: Option<Rc<crate::kernel::Quota>>,
}
impl Default for Descriptors {
    fn default() -> Self {
        Self {
            endpoints: std::array::from_fn(|_| None),
            stdin: None,
            owner: Rc::new(()),
            next_file: 1,
            file_quota: None,
        }
    }
}
impl Descriptors {
    /// A table may rebind a namespace only after all regular/directory endpoints close.
    pub fn registered_file_bytes(&self) -> usize {
        self.file_quota.as_ref().map_or(0, |quota| quota.used.get())
    }
    pub fn validate_file_namespace(&self, files: &crate::kernel::Files) -> Result<()> {
        if let Some(quota) = &self.file_quota {
            if self
                .endpoints
                .iter()
                .chain(std::iter::once(&self.stdin))
                .any(|e| matches!(e, Some(Endpoint::File(_) | Endpoint::Directory(_))))
                && !Rc::ptr_eq(quota, &files.quota())
            {
                return Err(
                    "Cannot mix registered-file quota domains while file descriptors remain live"
                        .into(),
                );
            }
        }
        Ok(())
    }
    /// Install an internal read-authorized directory handle. This intentionally
    /// does not route open_file_options through a regular-file token. Raw guest
    /// directory read/seek/stat remain separate, unsupported integration work.
    pub fn open_directory(
        &mut self,
        files: &crate::kernel::Files,
        path: &[u8],
        cwd: Option<&[u8]>,
    ) -> Result<std::result::Result<DirectoryDescriptorToken, u32>> {
        self.validate_file_namespace(files)?;
        if !path.is_empty() && path[0] != b'/' && cwd.is_none() {
            return Err(
                "Guest working directory is not configured for relative directory open".into(),
            );
        }
        let Some(index) = self.endpoints.iter().position(Option::is_none) else {
            return Ok(Err(24));
        };
        let identity = self.next_file;
        let Some(next) = identity.checked_add(1) else {
            return Ok(Err(24));
        };
        let backing = match files.open_directory(path, cwd) {
            Ok(backing) => backing,
            Err(errno) => return Ok(Err(errno)),
        };
        self.file_quota = Some(files.quota());
        self.endpoints[index] = Some(Endpoint::Directory(OpenDirectory {
            backing,
            identity,
            position: Cell::new(0),
            revision: Cell::new(0),
            namespace: RefCell::new(None),
        }));
        self.next_file = next;
        Ok(Ok(DirectoryDescriptorToken {
            descriptor: index as u32 + 3,
            identity,
            owner: self.owner.clone(),
        }))
    }
    pub fn directory_token(
        &self,
        descriptor: u64,
    ) -> std::result::Result<DirectoryDescriptorToken, u32> {
        let Some(Endpoint::Directory(directory)) = self.endpoint(descriptor) else {
            return Err(9);
        };
        Ok(DirectoryDescriptorToken {
            descriptor: descriptor as u32,
            identity: directory.identity,
            owner: self.owner.clone(),
        })
    }
    pub fn directory_backing(
        &self,
        token: &DirectoryDescriptorToken,
    ) -> std::result::Result<&crate::kernel::DirectoryToken, u32> {
        if !Rc::ptr_eq(&self.owner, &token.owner) {
            return Err(9);
        }
        match self.endpoint(token.descriptor.into()) {
            Some(Endpoint::Directory(directory)) if directory.identity == token.identity => {
                Ok(&directory.backing)
            }
            _ => Err(9),
        }
    }
    fn directory(&self, token: &DirectoryDescriptorToken) -> Result<&OpenDirectory> {
        if !Rc::ptr_eq(&self.owner, &token.owner) {
            return Err("Foreign directory descriptor token".into());
        }
        match self.endpoint(token.descriptor.into()) {
            Some(Endpoint::Directory(directory)) if directory.identity == token.identity => {
                Ok(directory)
            }
            _ => Err("Stale or closed directory descriptor token".into()),
        }
    }
    pub fn directory_position(&self, token: &DirectoryDescriptorToken) -> Result<usize> {
        Ok(self.directory(token)?.position.get())
    }
    /// Internal record cursor, not a kernel byte-offset/cookie implementation.
    /// Namespace mutation after the first committed batch requires rewind.
    pub fn preview_directory_read(
        &self,
        files: &crate::kernel::Files,
        token: &DirectoryDescriptorToken,
        capacity: u64,
    ) -> Result<DirectoryReadPlan> {
        if capacity > 1_048_576 {
            return Err("Directory read exceeds prototype capacity".into());
        }
        let directory = self.directory(token)?;
        if let Some(namespace) = directory.namespace.borrow().as_ref() {
            files.validate_directory_revision(namespace)?;
        }
        let namespace = files.directory_revision();
        let entries = files.directory_entries_for_token(&directory.backing)?;
        let position = directory.position.get();
        if position > entries.len() {
            return Err("Directory cursor exceeds current enumeration".into());
        }
        let mut bytes = Vec::new();
        let mut next = position;
        for entry in &entries[position..] {
            let record = entry.record()?;
            if record.len() > capacity as usize - bytes.len() {
                if bytes.is_empty() {
                    return Err("Next complete directory record does not fit".into());
                }
                break;
            }
            bytes
                .try_reserve(record.len())
                .map_err(|_| "Cannot reserve directory read batch")?;
            bytes.extend_from_slice(&record);
            next += 1;
        }
        directory
            .revision
            .get()
            .checked_add(1)
            .ok_or("Directory cursor revision exhausted")?;
        Ok(DirectoryReadPlan {
            bytes,
            token: token.clone(),
            position,
            next_position: next,
            revision: directory.revision.get(),
            namespace,
            eof: next == entries.len(),
        })
    }
    pub fn validate_directory_read(
        &self,
        files: &crate::kernel::Files,
        plan: &DirectoryReadPlan,
    ) -> Result<()> {
        let directory = self.directory(&plan.token)?;
        if directory.position.get() != plan.position || directory.revision.get() != plan.revision {
            return Err("Stale directory read plan".into());
        }
        if !files.owns_directory_token(&directory.backing) {
            return Err("Foreign directory namespace".into());
        }
        files.validate_directory_revision(&plan.namespace)?;
        if let Some(namespace) = directory.namespace.borrow().as_ref() {
            files.validate_directory_revision(namespace)?;
        }
        directory
            .revision
            .get()
            .checked_add(1)
            .ok_or("Directory cursor revision exhausted")?;
        Ok(())
    }
    pub fn commit_directory_read(
        &self,
        files: &crate::kernel::Files,
        plan: &DirectoryReadPlan,
    ) -> Result<()> {
        self.validate_directory_read(files, plan)?;
        let directory = self.directory(&plan.token)?;
        directory.position.set(plan.next_position);
        directory.revision.set(plan.revision + 1);
        *directory.namespace.borrow_mut() = Some(plan.namespace.clone());
        Ok(())
    }
    /// Guard and copy before committing cursor state. Rc/Cell ownership is
    /// single-threaded and this path runs no callbacks between validations.
    pub fn copy_directory_read(
        &self,
        files: &crate::kernel::Files,
        plan: &DirectoryReadPlan,
        memory: &mut Memory<'_>,
        address: u64,
    ) -> Result<usize> {
        self.validate_directory_read(files, plan)?;
        if !plan.bytes().is_empty() {
            memory.write(address, plan.bytes())?;
        }
        self.commit_directory_read(files, plan)?;
        Ok(plan.bytes().len())
    }
    pub fn rewind_directory(
        &self,
        files: &crate::kernel::Files,
        token: &DirectoryDescriptorToken,
    ) -> Result<()> {
        let directory = self.directory(token)?;
        files.directory_entries_for_token(&directory.backing)?;
        let next = directory
            .revision
            .get()
            .checked_add(1)
            .ok_or("Directory cursor revision exhausted")?;
        directory.position.set(0);
        directory.revision.set(next);
        *directory.namespace.borrow_mut() = None;
        Ok(())
    }
    pub fn close_directory(&mut self, token: &DirectoryDescriptorToken) -> u32 {
        if self.directory_backing(token).is_err() {
            return 9;
        }
        self.close(token.descriptor.into())
    }
    pub fn open_file(
        &mut self,
        files: &crate::kernel::Files,
        path: &[u8],
    ) -> std::result::Result<FileToken, u32> {
        self.validate_file_namespace(files).map_err(|_| 9u32)?;
        files.check_open(path, true, false)?;
        let backing = files.backing(path)?;
        self.install_file(backing, OpenOptions::read_only())
    }
    /// Explicit regular-file redirection. No input source is installed by
    /// default. The execution owner must prevent configuration during guest
    /// execution. Reconfiguration requires closing the previous fd0 instance;
    /// its tokens and pending plans never become valid again.
    pub fn configure_stdin_from_registered_file(
        &mut self,
        files: &crate::kernel::Files,
        path: &[u8],
    ) -> Result<FileToken> {
        self.validate_file_namespace(files)?;
        if self.stdin.is_some() {
            return Err("Guest stdin is already configured".into());
        }
        files
            .check_open(path, true, false)
            .map_err(|error| format!("Cannot configure guest stdin: errno {error}"))?;
        let backing = files
            .backing(path)
            .map_err(|error| format!("Cannot configure guest stdin: errno {error}"))?;
        let identity = self.next_file;
        let next = identity
            .checked_add(1)
            .ok_or("Guest file identity exhausted")?;
        self.file_quota = Some(backing.quota.clone());
        self.stdin = Some(Endpoint::File(OpenFile {
            backing,
            options: OpenOptions::read_only(),
            identity,
            position: Cell::new(0),
            revision: Cell::new(0),
        }));
        self.next_file = next;
        Ok(FileToken {
            descriptor: 0,
            identity,
            owner: self.owner.clone(),
        })
    }
    pub fn open_file_options(
        &mut self,
        files: &mut crate::kernel::Files,
        path: &[u8],
        options: OpenOptions,
    ) -> Result<std::result::Result<FileToken, u32>> {
        self.open_file_options_at(files, path, options, None)
    }
    pub fn open_file_options_at(
        &mut self,
        files: &mut crate::kernel::Files,
        path: &[u8],
        options: OpenOptions,
        cwd: Option<&[u8]>,
    ) -> Result<std::result::Result<FileToken, u32>> {
        self.validate_file_namespace(files)?;
        if !path.is_empty() && path[0] != b'/' && cwd.is_none() {
            return Err("Guest working directory is not configured for relative open".into());
        }
        if (!options.read && !options.write) || (options.truncate && !options.write) {
            return Ok(Err(22));
        }
        // Capacity/identity checks precede namespace or content mutation.
        if !self.endpoints.iter().any(Option::is_none) || self.next_file == u64::MAX {
            return Ok(Err(24));
        }
        let resolved = match files.resolve(path, cwd) {
            Ok(path) => path,
            Err(errno) => return Ok(Err(errno)),
        };
        let path = resolved.as_slice();
        if files.is_directory(path).unwrap_or(false) {
            if options.create && options.exclusive {
                return Ok(Err(17));
            }
            if options.write {
                return Ok(Err(21));
            }
            return Err("Read-only directory descriptors are unsupported".into());
        }
        Ok((|| {
            let backing = match files.backing(path) {
                Ok(file) => {
                    if options.create && options.exclusive {
                        return Err(17);
                    }
                    files.check_open(path, options.read, options.write)?;
                    file
                }
                Err(2) if options.create => files.create(path)?,
                Err(error) => return Err(error),
            };
            self.install_file(backing, options)
        })())
    }

    fn install_file(
        &mut self,
        backing: Rc<crate::kernel::FileBacking>,
        options: OpenOptions,
    ) -> std::result::Result<FileToken, u32> {
        if options.write && !backing.writable {
            return Err(13);
        }
        let index = self
            .endpoints
            .iter()
            .position(Option::is_none)
            .ok_or(24u32)?;
        let identity = self.next_file;
        let next = identity.checked_add(1).ok_or(24u32)?;
        if options.truncate {
            Self::resize_backing(&backing, 0)?;
        }
        self.file_quota = Some(backing.quota.clone());
        self.endpoints[index] = Some(Endpoint::File(OpenFile {
            backing,
            options,
            identity,
            position: Cell::new(0),
            revision: Cell::new(0),
        }));
        self.next_file = next;
        Ok(FileToken {
            descriptor: index as u32 + 3,
            identity,
            owner: self.owner.clone(),
        })
    }
    fn resized(bytes: &[u8], length: usize) -> std::result::Result<Box<[u8]>, u32> {
        let mut replacement = Vec::new();
        replacement.try_reserve_exact(length).map_err(|_| 12u32)?;
        replacement.resize(length, 0);
        let retained = bytes.len().min(length);
        replacement[..retained].copy_from_slice(&bytes[..retained]);
        Ok(replacement.into_boxed_slice())
    }
    fn resize_backing(
        backing: &crate::kernel::FileBacking,
        length: u64,
    ) -> std::result::Result<(), u32> {
        let mut state = backing.state.borrow_mut();
        let length = usize::try_from(length).map_err(|_| 28u32)?;
        if length > state.bytes.len() + backing.quota.remaining() {
            return Err(28);
        }
        let revision = state.revision.checked_add(1).ok_or(28u32)?;
        let replacement = Self::resized(&state.bytes, length)?;
        backing.quota.change(state.bytes.len(), replacement.len())?;
        state.bytes = replacement;
        state.revision = revision;
        Ok(())
    }
    pub fn write_capacity(
        &self,
        token: &FileToken,
        requested: u64,
    ) -> std::result::Result<usize, u32> {
        let file = self.file(token)?;
        if !file.options.write {
            return Err(9);
        }
        let state = file.backing.state.borrow();
        let position = if file.options.append {
            state.bytes.len() as u64
        } else {
            file.position.get()
        };
        let maximum = state.bytes.len() + file.backing.quota.remaining();
        Ok(requested.min((maximum as u64).saturating_sub(position)) as usize)
    }
    pub fn preview_write(
        &self,
        token: &FileToken,
        bytes: &[u8],
    ) -> std::result::Result<FileWritePlan, u32> {
        let capacity = self.write_capacity(token, bytes.len() as u64)?;
        self.preview_write_count(token, &bytes[..capacity], bytes.len() as u64)
    }
    /// Input is exactly the accepted prefix; requested retains the original
    /// full64 byte count so a short write carries its terminal quota condition.
    pub fn preview_write_count(
        &self,
        token: &FileToken,
        bytes: &[u8],
        requested: u64,
    ) -> std::result::Result<FileWritePlan, u32> {
        let amount = self.write_capacity(token, requested)?;
        if bytes.len() != amount {
            return Err(22);
        }
        let file = self.file(token)?;
        let state = file.backing.state.borrow();
        let position = if file.options.append {
            state.bytes.len() as u64
        } else {
            file.position.get()
        };
        let terminal_error = ((amount as u64) < requested).then_some(28);
        let length = if amount == 0 {
            state.bytes.len()
        } else {
            state.bytes.len().max(position as usize + amount)
        };
        let mut replacement = if amount == 0 {
            Box::default()
        } else {
            Self::resized(&state.bytes, length)?
        };
        if amount != 0 {
            replacement[position as usize..position as usize + amount].copy_from_slice(bytes);
        }
        Ok(FileWritePlan {
            token: token.clone(),
            cursor: file.position.get(),
            position,
            revision: file.revision.get(),
            file_revision: state.revision,
            quota_revision: file.backing.quota.revision.get(),
            replacement,
            amount,
            terminal_error,
        })
    }
    /// Consumes the prepared replacement only after all caller output guards.
    pub fn commit_write(&self, plan: FileWritePlan) -> std::result::Result<(), u32> {
        let file = self.file(&plan.token)?;
        let mut state = file.backing.state.borrow_mut();
        if file.position.get() != plan.cursor
            || file.revision.get() != plan.revision
            || state.revision != plan.file_revision
            || file.backing.quota.revision.get() != plan.quota_revision
        {
            return Err(9);
        }
        let revision = plan.revision.checked_add(1).ok_or(22u32)?;
        let file_revision = state.revision.checked_add(1).ok_or(28u32)?;
        if plan.amount != 0 {
            file.backing
                .quota
                .change(state.bytes.len(), plan.replacement.len())?;
            state.bytes = plan.replacement;
            state.revision = file_revision;
            file.position.set(plan.position + plan.amount as u64);
        }
        file.revision.set(revision);
        Ok(())
    }
    pub fn truncate_file(&self, token: &FileToken, length: u64) -> std::result::Result<(), u32> {
        let file = self.file(token)?;
        if !file.options.write {
            return Err(22);
        }
        Self::resize_backing(&file.backing, length)
    }
    pub fn file_options(&self, token: &FileToken) -> std::result::Result<OpenOptions, u32> {
        Ok(self.file(token)?.options)
    }
    pub fn kind(&self, descriptor: u64) -> Option<DescriptorKind> {
        self.endpoint(descriptor).map(|endpoint| match endpoint {
            Endpoint::File(_) => DescriptorKind::RegisteredFile,
            Endpoint::Directory(_) => DescriptorKind::Directory,
            Endpoint::Pipe { .. } => DescriptorKind::Pipe,
            Endpoint::Control { .. } => DescriptorKind::Control,
        })
    }
    pub fn stat_file(
        &self,
        token: &FileToken,
    ) -> std::result::Result<[u8; crate::kernel::STAT_SIZE], u32> {
        Ok(self.file(token)?.backing.stat())
    }
    pub fn file_token(&self, descriptor: u64) -> std::result::Result<FileToken, u32> {
        let Some(Endpoint::File(file)) = self.endpoint(descriptor) else {
            return Err(9);
        };
        Ok(FileToken {
            descriptor: descriptor as u32,
            identity: file.identity,
            owner: self.owner.clone(),
        })
    }
    fn file(&self, token: &FileToken) -> std::result::Result<&OpenFile, u32> {
        if !Rc::ptr_eq(&self.owner, &token.owner) {
            return Err(9);
        }
        match self.endpoint(token.descriptor.into()) {
            Some(Endpoint::File(file)) if file.identity == token.identity => Ok(file),
            _ => Err(9),
        }
    }
    pub fn file_position(&self, token: &FileToken) -> std::result::Result<u64, u32> {
        Ok(self.file(token)?.position.get())
    }
    pub fn file_len(&self, token: &FileToken) -> std::result::Result<u64, u32> {
        Ok(self.file(token)?.backing.state.borrow().bytes.len() as u64)
    }
    pub fn preview_read(
        &self,
        token: &FileToken,
        count: u64,
    ) -> std::result::Result<FileReadPlan, u32> {
        let file = self.file(token)?;
        if !file.options.read {
            return Err(9);
        }
        let state = file.backing.state.borrow();
        let position = file.position.get();
        let amount = count.min((state.bytes.len() as u64).saturating_sub(position)) as usize;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(amount).map_err(|_| 12u32)?;
        if amount != 0 {
            bytes.extend_from_slice(&state.bytes[position as usize..position as usize + amount]);
        }
        Ok(FileReadPlan {
            bytes,
            token: token.clone(),
            position,
            revision: file.revision.get(),
            file_revision: state.revision,
        })
    }
    /// Snapshot a regular-file line without consuming it. The delimiter is
    /// included, and no bytes after it are consumed or used to establish EOF.
    /// A zero capacity never attempts a read and therefore never reports EOF.
    pub fn preview_line_read(
        &self,
        token: &FileToken,
        capacity: u64,
    ) -> std::result::Result<FileLineReadPlan, u32> {
        let file = self.file(token)?;
        if !file.options.read {
            return Err(9);
        }
        let state = file.backing.state.borrow();
        let position = file.position.get();
        let available = (state.bytes.len() as u64).saturating_sub(position);
        let limit = capacity.min(available) as usize;
        let candidates = if limit == 0 {
            &[][..]
        } else {
            &state.bytes[position as usize..position as usize + limit]
        };
        let newline = candidates.iter().position(|&byte| byte == b'\n');
        let amount = newline.map_or(limit, |index| index + 1);
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(amount).map_err(|_| 12u32)?;
        bytes.extend_from_slice(&candidates[..amount]);
        Ok(FileLineReadPlan {
            read: FileReadPlan {
                bytes,
                token: token.clone(),
                position,
                revision: file.revision.get(),
                file_revision: state.revision,
            },
            eof: newline.is_none() && available < capacity,
        })
    }
    pub fn commit_line_read(&self, plan: &FileLineReadPlan) -> std::result::Result<(), u32> {
        self.commit_read(&plan.read)
    }
    /// Call only after all output guards. One-shot even for an EOF/zero-byte read.
    pub fn commit_read(&self, plan: &FileReadPlan) -> std::result::Result<(), u32> {
        let file = self.file(&plan.token)?;
        if file.position.get() != plan.position
            || file.revision.get() != plan.revision
            || file.backing.state.borrow().revision != plan.file_revision
        {
            return Err(9);
        }
        let next = plan.revision.checked_add(1).ok_or(22u32)?;
        file.position.set(plan.position + plan.bytes.len() as u64);
        file.revision.set(next);
        Ok(())
    }
    pub fn read_file(
        &self,
        token: &FileToken,
        memory: &mut Memory<'_>,
        address: u64,
        count: u64,
    ) -> std::result::Result<u64, u32> {
        let plan = self.preview_read(token, count)?;
        if !plan.bytes.is_empty() {
            memory
                .validate_write(address, plan.bytes.len())
                .map_err(|_| 14u32)?;
        }
        // No fallible guest-memory operation remains after this validation.
        self.commit_read(&plan)?;
        if !plan.bytes.is_empty() {
            memory
                .write(address, &plan.bytes)
                .expect("Prevalidated file read");
        }
        Ok(plan.bytes.len() as u64)
    }
    /// LP64 off_t, SEEK_SET/CUR/END. Seeking beyond EOF is legal; negative offsets are not.
    pub fn seek_file(
        &self,
        token: &FileToken,
        offset: i64,
        whence: i32,
    ) -> std::result::Result<u64, u32> {
        let file = self.file(token)?;
        let base = match whence {
            0 => 0,
            1 => file.position.get(),
            2 => file.backing.state.borrow().bytes.len() as u64,
            _ => return Err(22),
        };
        let position = i128::from(base) + i128::from(offset);
        if position > i128::from(i64::MAX) {
            return Err(84);
        }
        if position < 0 {
            return Err(22);
        }
        let revision = file.revision.get().checked_add(1).ok_or(22u32)?;
        file.position.set(position as u64);
        file.revision.set(revision);
        Ok(position as u64)
    }
    pub fn close_file(&mut self, token: &FileToken) -> u32 {
        if self.file(token).is_err() {
            return 9;
        }
        self.close(token.descriptor.into())
    }
    pub fn seek(&self, descriptor: u64, offset: i64, whence: i32) -> std::result::Result<u64, u32> {
        match self.endpoint(descriptor) {
            None => Err(9),
            Some(Endpoint::File(_)) => {
                self.seek_file(&self.file_token(descriptor)?, offset, whence)
            }
            Some(_) => Err(29),
        }
    }
    pub fn packet_control_pair(&mut self) -> Result<(u32, u32)> {
        let mut free = self
            .endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none());
        let first = free.next().ok_or("Guest descriptor limit reached")?.0;
        let second = free.next().ok_or("Guest descriptor limit reached")?.0;
        let target = Rc::new(Cell::new(0));
        self.endpoints[first] = Some(Endpoint::Control {
            target: target.clone(),
            selector: true,
        });
        self.endpoints[second] = Some(Endpoint::Control {
            target,
            selector: false,
        });
        Ok((first as u32 + 3, second as u32 + 3))
    }
    pub fn kernel_pipe(&mut self, kernel: &Memory<'_>, control: u64) -> Result<(u32, u32)> {
        if control == 0 {
            return Err("Null kernel pipe control".into());
        }
        kernel.validate_write(control, 24)?;
        let mut free = self
            .endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_none());
        let first = free.next().ok_or("Guest descriptor limit reached")?.0;
        let second = free.next().ok_or("Guest descriptor limit reached")?.0;
        let state = Rc::new(Pipe {
            control,
            reader_open: Cell::new(true),
            writer_open: Cell::new(true),
        });
        self.endpoints[first] = Some(Endpoint::Pipe {
            state: state.clone(),
            writer: false,
        });
        self.endpoints[second] = Some(Endpoint::Pipe {
            state,
            writer: true,
        });
        Ok((first as u32 + 3, second as u32 + 3))
    }
    fn index(descriptor: u64) -> Option<usize> {
        descriptor
            .checked_sub(3)
            .filter(|&n| n < LIMIT as u64)
            .map(|n| n as usize)
    }
    fn endpoint(&self, descriptor: u64) -> Option<&Endpoint> {
        if descriptor == 0 {
            self.stdin.as_ref()
        } else {
            Self::index(descriptor).and_then(|index| self.endpoints[index].as_ref())
        }
    }
    /// Guest errno, matching the retained payload model (zero is success).
    pub fn close(&mut self, descriptor: u64) -> u32 {
        if descriptor == 0 {
            return if self.stdin.take().is_some() { 0 } else { 9 };
        }
        let Some(index) = Self::index(descriptor) else {
            return 9;
        };
        if let Some(endpoint) = self.endpoints[index].take() {
            if let Endpoint::Pipe { state, writer } = endpoint {
                if writer {
                    state.writer_open.set(false);
                } else {
                    state.reader_open.set(false);
                }
            }
            0
        } else {
            9
        }
    }
    /// The caller supplies the same process-owned kernel address space on every
    /// call. No guest address becomes a host pointer; failed writes are atomic.
    pub fn set_socket_option(
        &mut self,
        user: &Memory<'_>,
        kernel: &mut Memory<'_>,
        arguments: [u64; 5],
    ) -> u32 {
        let [descriptor, level, option, address, length] = arguments;
        let Some(endpoint) = self.endpoint(descriptor) else {
            return 9;
        };
        let Endpoint::Control { target, selector } = endpoint else {
            return 38;
        };
        if level != 41 || option != 46 {
            return 42;
        }
        if length != 20 {
            return 22;
        }
        let Ok(value) = user.read(address, 20) else {
            return 14;
        };
        if *selector {
            target.set(u64::from_le_bytes(value[..8].try_into().unwrap()));
        } else {
            let target = target.get();
            if target == 0 || kernel.write(target, value).is_err() {
                return 14;
            }
        }
        0
    }
    /// Transfer through the guest-controlled kernel ring. All ranges are
    /// validated before data or control indices change. Maximum copy: 1 MiB.
    pub fn transfer(
        &self,
        user: &mut Memory<'_>,
        kernel: &mut Memory<'_>,
        descriptor: u64,
        address: u64,
        count: u64,
        write: bool,
    ) -> std::result::Result<u64, u32> {
        if let Ok(token) = self.file_token(descriptor) {
            if write {
                if count > i64::MAX as u64 {
                    return Err(22);
                }
                let capacity = self.write_capacity(&token, count)?;
                let bytes = if capacity == 0 {
                    &[][..]
                } else {
                    user.read(address, capacity).map_err(|_| 14u32)?
                };
                let plan = self.preview_write_count(&token, bytes, count)?;
                let amount = plan.bytes_written();
                let error = plan.terminal_error();
                self.commit_write(plan)?;
                return if amount == 0 && count != 0 {
                    Err(error.unwrap_or(28))
                } else {
                    Ok(amount as u64)
                };
            }
            if count > i64::MAX as u64 {
                return Err(22);
            }
            return self.read_file(&token, user, address, count);
        }
        let Some(Endpoint::Pipe { state, writer }) =
            Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref())
        else {
            return Err(9);
        };
        if *writer != write {
            return Err(9);
        }
        if count > 1_048_576 {
            return Err(22);
        }
        if count == 0 {
            return Ok(0);
        }
        let c = state.control;
        kernel.validate_write(c, 24).map_err(|_| 14u32)?;
        let used = kernel.load(c, 4).map_err(|_| 14u32)?;
        let input = kernel.load(c + 4, 4).map_err(|_| 14u32)?;
        let output = kernel.load(c + 8, 4).map_err(|_| 14u32)?;
        let capacity = kernel.load(c + 12, 4).map_err(|_| 14u32)?;
        let buffer = kernel.load(c + 16, 8).map_err(|_| 14u32)?;
        if capacity == 0
            || capacity > 0x40000000
            || used > capacity
            || input > capacity
            || output > capacity
            || buffer.checked_add(capacity).is_none()
        {
            return Err(22);
        }
        if write && !state.reader_open.get() {
            return Err(32);
        }
        let available = if write { capacity - used } else { used };
        if available == 0 {
            return if !write && !state.writer_open.get() {
                Ok(0)
            } else {
                Err(35)
            };
        }
        if write && count <= 512 && count > available {
            return Err(35);
        }
        let amount = count.min(available);
        let position = if write { input } else { output } % capacity;
        let first = amount.min(capacity - position);
        let second = amount - first;
        let overlaps =
            |start: u64, length: u64| length != 0 && start < c + 24 && c < start + length;
        if write && (overlaps(buffer + position, first) || overlaps(buffer, second)) {
            return Err(22);
        }
        let checked = (|| -> Result<()> {
            if write {
                let copy = user.read(address, amount as usize)?.to_vec();
                kernel.validate_write(buffer + position, first as usize)?;
                kernel.validate_write(buffer, second as usize)?;
                kernel.write(buffer + position, &copy[..first as usize])?;
                if second != 0 {
                    kernel.write(buffer, &copy[first as usize..])?;
                }
            } else {
                user.validate_write(address, amount as usize)?;
                let mut copy = kernel.read(buffer + position, first as usize)?.to_vec();
                if second != 0 {
                    copy.extend_from_slice(kernel.read(buffer, second as usize)?);
                }
                user.write(address, &copy)?;
            }
            kernel.store(c, if write { used + amount } else { used - amount }, 4)?;
            kernel.store(
                c + if write { 4 } else { 8 },
                (position + amount) % capacity,
                4,
            )?;
            Ok(())
        })();
        checked.map_err(|_| 14u32)?;
        Ok(amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn writable_options() -> OpenOptions {
        OpenOptions {
            read: true,
            write: true,
            ..OpenOptions::read_only()
        }
    }
    #[test]
    fn mutable_contents_are_shared_while_cursors_and_access_modes_are_independent() {
        let mut files = crate::kernel::Files::default();
        files
            .register_writable(b"/save/file", b"abcd".to_vec())
            .unwrap();
        files.register(b"/asset", b"fixed".to_vec()).unwrap();
        files.allow_create_directory(b"/save").unwrap();
        let mut d = Descriptors::default();
        let a = d
            .open_file_options(&mut files, b"/save/file", writable_options())
            .unwrap()
            .unwrap();
        let b = d.open_file(&files, b"/save/file").unwrap();
        let stale_read = d.preview_read(&b, 4).unwrap();
        d.seek_file(&a, 6, 0).unwrap();
        let write = d.preview_write(&a, b"XY").unwrap();
        assert_eq!(write.bytes_written(), 2);
        d.commit_write(write).unwrap();
        assert_eq!(d.preview_read(&b, 8).unwrap().bytes(), b"abcd\0\0XY");
        assert_eq!(d.commit_read(&stale_read), Err(9));
        assert_eq!(d.file_position(&b), Ok(0));
        assert_eq!(d.file_position(&a), Ok(8));
        assert_eq!(
            u64::from_le_bytes(d.stat_file(&b).unwrap()[72..80].try_into().unwrap()),
            8
        );
        assert!(matches!(d.preview_write(&b, b"!"), Err(9)));
        assert!(matches!(
            d.open_file_options(&mut files, b"/asset", writable_options())
                .unwrap(),
            Err(13)
        ));
        let append = d
            .open_file_options(
                &mut files,
                b"/save/file",
                OpenOptions {
                    append: true,
                    ..writable_options()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(d.file_position(&append), Ok(0));
        d.seek_file(&append, 1, 0).unwrap();
        let plan = d.preview_write(&append, b"!").unwrap();
        d.commit_write(plan).unwrap();
        assert_eq!(d.file_position(&append), Ok(9));
        assert_eq!(d.preview_read(&b, 20).unwrap().bytes(), b"abcd\0\0XY!");
        let stale = d.preview_write(&a, b"old").unwrap();
        d.truncate_file(&append, 2).unwrap();
        assert_eq!(d.commit_write(stale), Err(9));
        assert_eq!(d.file_position(&a), Ok(8));
        assert_eq!(d.preview_read(&b, 20).unwrap().bytes(), b"ab");
        assert_eq!(files.content_bytes(), 7);
        let create = OpenOptions {
            create: true,
            exclusive: true,
            ..writable_options()
        };
        assert!(matches!(
            d.open_file_options(&mut files, b"/save/file", create)
                .unwrap(),
            Err(17)
        ));
        assert!(matches!(
            d.open_file_options(&mut files, b"/missing/new", create)
                .unwrap(),
            Err(2)
        ));
        let new = d
            .open_file_options(&mut files, b"/save/new", create)
            .unwrap()
            .unwrap();
        assert_eq!(d.file_len(&new), Ok(0));
        let truncated = d
            .open_file_options(
                &mut files,
                b"/save/file",
                OpenOptions {
                    truncate: true,
                    ..writable_options()
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(d.file_len(&a), Ok(0));
        assert_eq!(d.file_len(&truncated), Ok(0));
    }
    #[test]
    fn quota_partial_write_prefix_guards_and_namespace_lifetime() {
        const CAP: usize = 64 * 1024 * 1024;
        let mut files = crate::kernel::Files::default();
        files.register_writable(b"/file", b"abcd".to_vec()).unwrap();
        files.register(b"/filler", vec![0; CAP - 6]).unwrap();
        let mut d = Descriptors::default();
        let token = d
            .open_file_options(&mut files, b"/file", writable_options())
            .unwrap()
            .unwrap();
        d.seek_file(&token, 4, 0).unwrap();
        assert_eq!(d.write_capacity(&token, u64::MAX), Ok(2));
        let mut input = *b"XY";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut input, false, false).unwrap();
        let mut kernel = Memory::default();
        assert_eq!(
            d.transfer(
                &mut memory,
                &mut kernel,
                token.descriptor().into(),
                0x1000,
                4,
                true
            ),
            Ok(2)
        );
        assert_eq!(
            d.transfer(
                &mut memory,
                &mut kernel,
                token.descriptor().into(),
                u64::MAX,
                1,
                true
            ),
            Err(28)
        );
        assert_eq!(files.content_bytes(), CAP);
        d.seek_file(&token, 0, 0).unwrap();
        assert_eq!(d.preview_read(&token, 8).unwrap().bytes(), b"abcdXY");
        let stale = d.preview_write(&token, b"Z").unwrap();
        d.truncate_file(&token, 2).unwrap();
        assert_eq!(files.content_bytes(), CAP - 4);
        assert_eq!(d.commit_write(stale), Err(9));
        // Dropping the namespace releases filler, but the open file remains charged.
        drop(files);
        assert_eq!(d.registered_file_bytes(), 2);
        let mut replacement = crate::kernel::Files::default();
        replacement.register(b"/file", vec![9]).unwrap();
        assert!(
            d.open_file_options(&mut replacement, b"/file", OpenOptions::read_only())
                .unwrap_err()
                .contains("quota domains")
        );
        assert_eq!(d.close_file(&token), 0);
        assert_eq!(d.registered_file_bytes(), 0);
        assert!(
            d.open_file_options(&mut replacement, b"/file", OpenOptions::read_only())
                .unwrap()
                .is_ok()
        );
        assert_eq!(d.registered_file_bytes(), 1);
    }
    #[test]
    fn files_share_cursor_guard_copies_and_reject_stale_tokens() {
        let mut files = crate::kernel::Files::default();
        files.register(b"/file", b"abcdef".to_vec()).unwrap();
        let mut d = Descriptors::default();
        let token = d.open_file(&files, b"/file").unwrap();
        let independent = d.open_file(&files, b"/file").unwrap();
        assert_eq!(token.descriptor(), 3);
        let mut bytes = [0xa5; 8];
        let mut readonly = [0xa5; 8];
        let mut user = Memory::default();
        user.add(0x1_0000_0001, &mut bytes, true, false).unwrap();
        user.add(0x2000, &mut readonly, false, false).unwrap();
        for address in [0x2000, 0x1_0000_0008, u64::MAX] {
            assert_eq!(d.read_file(&token, &mut user, address, 3), Err(14));
            assert_eq!(d.file_position(&token), Ok(0));
        }
        assert_eq!(d.read_file(&token, &mut user, u64::MAX, 0), Ok(0));
        let pending = d.preview_read(&token, 2).unwrap();
        assert_eq!(pending.bytes, b"ab");
        assert_eq!(d.read_file(&token, &mut user, 0x1_0000_0001, 3), Ok(3));
        assert_eq!(d.commit_read(&pending), Err(9));
        assert_eq!(d.file_position(&independent), Ok(0));
        let alias = d.file_token(3).unwrap();
        let plan = d.preview_read(&alias, u64::MAX).unwrap();
        assert_eq!(plan.bytes, b"def");
        d.commit_read(&plan).unwrap();
        assert_eq!(d.commit_read(&plan), Err(9));
        assert_eq!(d.file_position(&token), Ok(6));
        assert_eq!(d.read_file(&token, &mut user, u64::MAX, u64::MAX), Ok(0));
        assert_eq!(d.seek_file(&token, 7, 0), Ok(7));
        assert_eq!(d.seek_file(&token, -1, 2), Ok(5));
        assert_eq!(d.seek_file(&token, -6, 1), Err(22));
        assert_eq!(d.seek_file(&token, i64::MAX, 1), Err(84));
        assert_eq!(d.seek_file(&token, 0, 7), Err(22));
        assert_eq!(d.file_position(&token), Ok(5));
        let pending = d.preview_read(&token, 1).unwrap();
        d.seek_file(&token, 5, 0).unwrap();
        assert_eq!(d.commit_read(&pending), Err(9));
        let other = Descriptors::default();
        assert_eq!(other.file_position(&token), Err(9));
        assert_eq!(d.close_file(&token), 0);
        let replacement = d.open_file(&files, b"/file").unwrap();
        assert_eq!(replacement.descriptor(), token.descriptor());
        assert_eq!(d.close_file(&token), 9);
        assert_eq!(d.file_position(&replacement), Ok(0));
        assert_eq!(d.file_position(&token), Err(9));
        let (control, _) = d.packet_control_pair().unwrap();
        assert_eq!(d.seek(control.into(), 0, 0), Err(29));
        assert!(d.file_token(control.into()).is_err());
        let mut kernel = Memory::default();
        assert_eq!(
            d.transfer(
                &mut user,
                &mut kernel,
                replacement.descriptor().into(),
                0x2000,
                1,
                true
            ),
            Err(9)
        );
        assert_eq!(
            d.transfer(
                &mut user,
                &mut kernel,
                replacement.descriptor().into(),
                0x2000,
                u64::MAX,
                false
            ),
            Err(22)
        );
        assert_eq!(
            user.read(0x1_0000_0001, 8).unwrap(),
            &[b'a', b'b', b'c', 0xa5, 0xa5, 0xa5, 0xa5, 0xa5]
        );
        assert_eq!(user.read(0x2000, 8).unwrap(), &[0xa5; 8]);
    }
    #[test]
    fn file_descriptor_capacity_is_atomic() {
        let mut files = crate::kernel::Files::default();
        files.register(b"/file", vec![7]).unwrap();
        let mut d = Descriptors::default();
        let token = d.open_file(&files, b"/file").unwrap();
        let plan = d.preview_read(&token, 1).unwrap();
        assert_eq!(plan.bytes(), &[7]);
        assert_eq!(d.file_position(&token), Ok(0));
        for _ in 1..LIMIT {
            d.open_file(&files, b"/file").unwrap();
        }
        assert!(matches!(d.open_file(&files, b"/file"), Err(24)));
        assert!(matches!(d.open_file(&files, b"/absent"), Err(2)));
        assert_eq!(files.content_bytes(), 1);
    }
    #[test]
    fn kernel_ring_wraparound_failures_and_close() {
        let mut descriptors = Descriptors::default();
        let mut control = [0u8; 24];
        let mut ring = [0u8; 16];
        let mut bytes = [0u8; 64];
        bytes[..8].copy_from_slice(b"abcdefgh");
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        kernel.add(0x2000, &mut control, true, false).unwrap();
        kernel.add(0x3000, &mut ring, true, false).unwrap();
        kernel.store(0x2004, 12, 4).unwrap();
        kernel.store(0x2008, 12, 4).unwrap();
        kernel.store(0x200c, 16, 4).unwrap();
        kernel.store(0x2010, 0x3000, 8).unwrap();
        let (reader, writer) = descriptors.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, writer.into(), 0x1000, 8, true),
            Ok(8)
        );
        assert_eq!(kernel.read(0x300c, 4).unwrap(), b"abcd");
        assert_eq!(kernel.read(0x3000, 4).unwrap(), b"efgh");
        let before = kernel.read(0x2000, 24).unwrap().to_vec();
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, writer.into(), 0x1000, 9, true),
            Err(35)
        );
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x103c, 8, false),
            Err(14)
        );
        assert_eq!(kernel.read(0x2000, 24).unwrap(), before);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 8, false),
            Ok(8)
        );
        assert_eq!(user.read(0x1020, 8).unwrap(), b"abcdefgh");
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 1, false),
            Err(35)
        );
        assert_eq!(descriptors.close(writer.into()), 0);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, reader.into(), 0x1020, 1, false),
            Ok(0)
        );
        let (r2, w2) = descriptors.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(descriptors.close(r2.into()), 0);
        assert_eq!(
            descriptors.transfer(&mut user, &mut kernel, w2.into(), 0x1000, 1, true),
            Err(32)
        );
    }
    #[test]
    fn kernel_ring_split_write_is_atomic_and_control_alias_is_rejected() {
        let mut d = Descriptors::default();
        let mut control = [0u8; 24];
        let mut tail = [0x55u8; 4];
        let mut bytes = [9u8; 8];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        kernel.add(0x2000, &mut control, true, false).unwrap();
        kernel.add(0x300c, &mut tail, true, false).unwrap();
        kernel.store(0x2004, 12, 4).unwrap();
        kernel.store(0x200c, 16, 4).unwrap();
        kernel.store(0x2010, 0x3000, 8).unwrap();
        let (r, w) = d.kernel_pipe(&kernel, 0x2000).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 8, true),
            Err(14)
        );
        assert_eq!(kernel.read(0x300c, 4).unwrap(), &[0x55; 4]);
        assert_eq!(kernel.load(0x2000, 4).unwrap(), 0);
        assert_eq!(kernel.load(0x2004, 4).unwrap(), 12);
        kernel.store(0x2010, 0x2000, 8).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 8, true),
            Err(22)
        );
        assert_eq!(
            d.transfer(&mut user, &mut kernel, r.into(), 0x1000, 1, true),
            Err(9)
        );
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 1_048_577, true),
            Err(22)
        );
        kernel.store(0x2010, u64::MAX - 1, 8).unwrap();
        assert_eq!(
            d.transfer(&mut user, &mut kernel, w.into(), 0x1000, 1, true),
            Err(22)
        );
    }
    #[test]
    fn selection_copy_faults_and_pair_isolation() {
        let mut descriptors = Descriptors::default();
        let (selector, writer) = descriptors.packet_control_pair().unwrap();
        let (_, other_writer) = descriptors.packet_control_pair().unwrap();
        let mut input = [0u8; 40];
        input[..8].copy_from_slice(&0xffff800000001000u64.to_le_bytes());
        input[20..].fill(0xa5);
        let mut output = [0x55u8; 24];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut input, true, false).unwrap();
        kernel
            .add(0xffff800000001000, &mut output, true, false)
            .unwrap();
        let call = |fd, ptr| [u64::from(fd), 41, 46, ptr, 20];
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            14
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1000)),
            0
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(other_writer, 0x1014)),
            14
        );
        // A failed selector read must preserve the previous selection.
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1020)),
            14
        );
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            0
        );
        assert_eq!(
            kernel.read(0xffff800000001000, 24).unwrap(),
            [&[0xa5; 20][..], &[0x55; 4][..]].concat()
        );
        user.store(0x1000, 0xffff800000001008, 8).unwrap();
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(selector, 0x1000)),
            0
        );
        let before = kernel.read(0xffff800000001000, 24).unwrap().to_vec();
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            14
        );
        assert_eq!(kernel.read(0xffff800000001000, 24).unwrap(), before);
        assert_eq!(descriptors.close(writer.into()), 0);
        assert_eq!(descriptors.close(writer.into()), 9);
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(writer, 0x1014)),
            9
        );
        assert_eq!(descriptors.close(selector.into()), 0);
        let replacement = descriptors.packet_control_pair().unwrap();
        assert_eq!(replacement, (selector, writer));
        // Reusing the descriptor numbers must not retain the old target.
        assert_eq!(
            descriptors.set_socket_option(&user, &mut kernel, call(replacement.1, 0x1014)),
            14
        );
    }
    #[test]
    fn capacity_failure_is_atomic_and_freed_slots_are_reused() {
        let mut d = Descriptors::default();
        for i in 0..128 {
            assert_eq!(d.packet_control_pair().unwrap(), (3 + 2 * i, 4 + 2 * i));
        }
        assert!(d.packet_control_pair().is_err());
        assert_eq!(d.close(3), 0);
        assert!(d.packet_control_pair().is_err());
        assert_eq!(d.close(258), 0);
        assert_eq!(d.packet_control_pair().unwrap(), (3, 258));
        for fd in [0, 2, 259, u64::MAX] {
            assert_eq!(d.close(fd), 9);
        }
    }
    #[test]
    fn read_only_kernel_and_argument_errors() {
        let mut d = Descriptors::default();
        let (s, w) = d.packet_control_pair().unwrap();
        let mut input = [0u8; 20];
        input[..8].copy_from_slice(&0x2000u64.to_le_bytes());
        let mut output = [7u8; 20];
        let mut user = Memory::default();
        let mut kernel = Memory::default();
        user.add(0x1000, &mut input, false, false).unwrap();
        kernel.add(0x2000, &mut output, false, false).unwrap();
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 41, 46, 0x1000, 20]),
            0
        );
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [w.into(), 41, 46, 0x1000, 20]),
            14
        );
        assert_eq!(kernel.read(0x2000, 20).unwrap(), &[7; 20]);
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 0, 46, 0x1000, 20]),
            42
        );
        assert_eq!(
            d.set_socket_option(&user, &mut kernel, [s.into(), 41, 46, 0x1000, 19]),
            22
        );
    }
}

#[cfg(test)]
mod directory_descriptor_tests {
    use super::*;
    use crate::kernel::Files;
    #[test]
    fn table_owner_generation_and_regular_file_reuse_are_independent() {
        let mut files = Files::default();
        files.allow_create_directory(b"/dir").unwrap();
        files.register(b"/dir/file", vec![1]).unwrap();
        let mut table = Descriptors::default();
        let token = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert_eq!(
            table.kind(token.descriptor().into()),
            Some(DescriptorKind::Directory)
        );
        assert!(table.file_token(token.descriptor().into()).is_err());
        let mut foreign = Descriptors::default();
        let foreign_token = foreign
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert_eq!(foreign.close_directory(&token), 9);
        assert_eq!(table.close_directory(&foreign_token), 9);
        let retained = table.directory_backing(&token).unwrap().clone();
        assert_eq!(table.close_directory(&token), 0);
        let file = table.open_file(&files, b"/dir/file").unwrap();
        assert_eq!(file.descriptor(), token.descriptor());
        assert_eq!(table.close_directory(&token), 9);
        assert_eq!(table.close_file(&file), 0);
        let replacement = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert_eq!(replacement.descriptor(), token.descriptor());
        assert_eq!(table.close_directory(&token), 9);
        assert_eq!(
            table.directory_backing(&replacement).unwrap().inode(),
            retained.inode()
        );
        assert!(files.owns_directory_token(&retained));
    }
    #[test]
    fn directories_hold_namespace_binding_and_share_all_descriptor_slots() {
        let mut files = Files::default();
        files.allow_create_directory(b"/dir").unwrap();
        files.register(b"/file", vec![1]).unwrap();
        let mut other = Files::default();
        other.allow_create_directory(b"/dir").unwrap();
        other.register(b"/file", vec![2]).unwrap();
        let mut table = Descriptors::default();
        let directory = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert!(!other.owns_directory_token(table.directory_backing(&directory).unwrap()));
        assert!(table.open_file(&other, b"/file").is_err());
        assert!(table.open_directory(&other, b"/dir", None).is_err());
        for _ in 1..LIMIT {
            table.open_file(&files, b"/file").unwrap();
        }
        assert!(matches!(
            table.open_directory(&files, b"/dir", None),
            Ok(Err(24))
        ));
        assert_eq!(table.close_directory(&directory), 0);
        assert!(table.open_directory(&other, b"/dir", None).is_err());
        for descriptor in 4..LIMIT as u64 + 3 {
            assert_eq!(table.close(descriptor), 0);
        }
        assert!(table.open_directory(&other, b"/dir", None).unwrap().is_ok());
    }
    #[test]
    fn directory_identity_follows_rename_and_detached_handle_keeps_namespace() {
        let mut files = Files::default();
        files.allow_create_directory(b"/save").unwrap();
        files.allow_create_directory(b"/save/dir").unwrap();
        files
            .register_writable(b"/save/dir/child", vec![1])
            .unwrap();
        let mut table = Descriptors::default();
        let token = table
            .open_directory(&files, b"/save/dir", None)
            .unwrap()
            .unwrap();
        let inode = table.directory_backing(&token).unwrap().inode();
        let plan = files
            .plan_rename(b"/save/dir", b"/save/moved", None)
            .unwrap();
        files.commit_rename(plan).unwrap();
        assert_eq!(table.directory_backing(&token).unwrap().inode(), inode);
        assert!(
            files
                .directory_entries_for_token(table.directory_backing(&token).unwrap())
                .unwrap()
                .iter()
                .any(|entry| entry.name == b"child")
        );
        files.remove(b"/save/moved/child", None).unwrap();
        files.remove(b"/save/moved", None).unwrap();
        assert!(
            files
                .directory_entries_for_token(table.directory_backing(&token).unwrap())
                .is_err()
        );
        let mut other = Files::default();
        other.allow_create_directory(b"/dir").unwrap();
        assert!(table.open_directory(&other, b"/dir", None).is_err());
        assert_eq!(table.close_directory(&token), 0);
        assert!(table.open_directory(&other, b"/dir", None).unwrap().is_ok());
    }
    #[test]
    fn failed_open_does_not_consume_slot_or_generation() {
        let mut files = Files::default();
        files.allow_create_directory(b"/dir").unwrap();
        files.register(b"/file", vec![1]).unwrap();
        let mut table = Descriptors::default();
        assert!(matches!(
            table.open_directory(&files, b"/missing", None),
            Ok(Err(2))
        ));
        assert!(matches!(
            table.open_directory(&files, b"/file", None),
            Ok(Err(20))
        ));
        assert!(table.open_directory(&files, b"dir", None).is_err());
        assert_eq!(table.next_file, 1);
        let token = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert_eq!(token.descriptor(), 3);
        assert_eq!(table.close_directory(&token), 0);
        table.next_file = u64::MAX;
        assert!(matches!(
            table.open_directory(&files, b"/dir", None),
            Ok(Err(24))
        ));
        assert!(table.endpoints.iter().all(Option::is_none));
    }
}

#[cfg(test)]
mod directory_read_tests {
    use super::*;
    use crate::kernel::Files;
    fn setup() -> (Files, Descriptors, DirectoryDescriptorToken) {
        let mut files = Files::default();
        files.allow_create_directory(b"/dir").unwrap();
        files.register(b"/dir/a", vec![1]).unwrap();
        files.register(b"/dir/long-name", vec![2]).unwrap();
        let mut table = Descriptors::default();
        let token = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        (files, table, token)
    }
    #[test]
    fn whole_record_boundaries_eof_and_replay() {
        let (files, table, token) = setup();
        for capacity in [0, 11, 1_048_577, u64::MAX] {
            assert!(
                table
                    .preview_directory_read(&files, &token, capacity)
                    .is_err()
            );
            assert_eq!(table.directory_position(&token).unwrap(), 0);
        }
        let first = table.preview_directory_read(&files, &token, 12).unwrap();
        assert_eq!(first.bytes().len(), 12);
        assert_eq!(&first.bytes()[4..8], &[12, 0, 4, 1]);
        assert_eq!(&first.bytes()[8..10], b".\0");
        assert!(!first.eof());
        assert_eq!(first.next_position(), 1);
        let competing = table.preview_directory_read(&files, &token, 12).unwrap();
        table.commit_directory_read(&files, &first).unwrap();
        assert!(table.commit_directory_read(&files, &competing).is_err());
        assert!(table.commit_directory_read(&files, &first).is_err());
        let rest = table.preview_directory_read(&files, &token, 1024).unwrap();
        assert!(rest.eof());
        let mut offset = 0;
        while offset < rest.bytes().len() {
            let len = u16::from_le_bytes(rest.bytes()[offset + 4..offset + 6].try_into().unwrap())
                as usize;
            assert_eq!(len % 4, 0);
            assert!(len >= 12);
            offset += len;
        }
        assert_eq!(offset, rest.bytes().len());
        table.commit_directory_read(&files, &rest).unwrap();
        let eof = table.preview_directory_read(&files, &token, 0).unwrap();
        assert!(eof.bytes().is_empty());
        assert!(eof.eof());
        table.commit_directory_read(&files, &eof).unwrap();
        assert!(table.commit_directory_read(&files, &eof).is_err());
    }
    #[test]
    fn namespace_mutation_requires_rewind_and_invalidates_previews() {
        let (mut files, table, token) = setup();
        let first = table.preview_directory_read(&files, &token, 12).unwrap();
        table.commit_directory_read(&files, &first).unwrap();
        let pending = table.preview_directory_read(&files, &token, 12).unwrap();
        files.register(b"/dir/new", vec![3]).unwrap();
        assert!(table.validate_directory_read(&files, &pending).is_err());
        assert!(table.commit_directory_read(&files, &pending).is_err());
        assert_eq!(table.directory_position(&token).unwrap(), 1);
        assert!(table.preview_directory_read(&files, &token, 1024).is_err());
        table.rewind_directory(&files, &token).unwrap();
        assert_eq!(table.directory_position(&token).unwrap(), 0);
        assert!(table.commit_directory_read(&files, &pending).is_err());
        let fresh = table.preview_directory_read(&files, &token, 1024).unwrap();
        table.rewind_directory(&files, &token).unwrap();
        assert!(table.commit_directory_read(&files, &fresh).is_err());
        assert!(table.preview_directory_read(&files, &token, 1024).is_ok());
    }
    #[test]
    fn foreign_tables_namespaces_and_reused_fds_reject_plan() {
        let (files, mut table, token) = setup();
        let pending = table.preview_directory_read(&files, &token, 12).unwrap();
        let (other, foreign, _) = setup();
        assert!(table.validate_directory_read(&other, &pending).is_err());
        assert!(foreign.validate_directory_read(&files, &pending).is_err());
        assert!(table.preview_directory_read(&other, &token, 12).is_err());
        assert_eq!(table.close_directory(&token), 0);
        let next = table
            .open_directory(&files, b"/dir", None)
            .unwrap()
            .unwrap();
        assert_eq!(next.descriptor(), token.descriptor());
        assert!(table.commit_directory_read(&files, &pending).is_err());
        assert_eq!(table.directory_position(&next).unwrap(), 0);
    }
}
