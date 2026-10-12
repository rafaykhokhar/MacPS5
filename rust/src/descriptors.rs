//! Bounded process-local descriptors for registered read-only files and control pairs.
//! No host filesystem, sockets, or network access.
//! File ABI sources: pinned SDK sys/syscall.h, sys/sysproto.h, sys/errno.h at
//! 10b539de8f2b6773a9a78caa7576a97dbd99b29a; FreeBSD releng/11.4
//! sys/kern/vfs_vnops.c::vn_seek and sys/kern/sys_generic.c::sys_read.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/vfs_vnops.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/sys_generic.c
//! Guarded copies are all-or-nothing; this does not model partial EFAULT copies.
use crate::{Result, guest_memory::Memory};
use std::{cell::Cell, rc::Rc};

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
pub struct FileReadPlan {
    bytes: Vec<u8>,
    token: FileToken,
    position: u64,
    revision: u64,
}
impl FileReadPlan {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind {
    RegisteredFile,
    Pipe,
    Control,
}
struct OpenFile {
    stat: [u8; crate::kernel::STAT_SIZE],
    bytes: Rc<[u8]>,
    identity: u64,
    position: Cell<u64>,
    revision: Cell<u64>,
}

enum Endpoint {
    File(OpenFile),
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
    owner: Rc<()>,
    next_file: u64,
}
impl Default for Descriptors {
    fn default() -> Self {
        Self {
            endpoints: std::array::from_fn(|_| None),
            owner: Rc::new(()),
            next_file: 1,
        }
    }
}
impl Descriptors {
    pub fn open_file(
        &mut self,
        files: &crate::kernel::Files,
        path: &[u8],
    ) -> std::result::Result<FileToken, u32> {
        let bytes = files.contents(path)?;
        let stat = files.stat_record(path)?;
        let index = self
            .endpoints
            .iter()
            .position(Option::is_none)
            .ok_or(24u32)?;
        let identity = self.next_file;
        let next = identity.checked_add(1).ok_or(24u32)?;
        self.endpoints[index] = Some(Endpoint::File(OpenFile {
            bytes,
            stat,
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
    pub fn kind(&self, descriptor: u64) -> Option<DescriptorKind> {
        Self::index(descriptor)
            .and_then(|i| self.endpoints[i].as_ref())
            .map(|endpoint| match endpoint {
                Endpoint::File(_) => DescriptorKind::RegisteredFile,
                Endpoint::Pipe { .. } => DescriptorKind::Pipe,
                Endpoint::Control { .. } => DescriptorKind::Control,
            })
    }
    pub fn stat_file(
        &self,
        token: &FileToken,
    ) -> std::result::Result<[u8; crate::kernel::STAT_SIZE], u32> {
        Ok(self.file(token)?.stat)
    }
    pub fn file_token(&self, descriptor: u64) -> std::result::Result<FileToken, u32> {
        let Some(Endpoint::File(file)) =
            Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref())
        else {
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
        match Self::index(token.descriptor.into()).and_then(|i| self.endpoints[i].as_ref()) {
            Some(Endpoint::File(file)) if file.identity == token.identity => Ok(file),
            _ => Err(9),
        }
    }
    pub fn file_position(&self, token: &FileToken) -> std::result::Result<u64, u32> {
        Ok(self.file(token)?.position.get())
    }
    pub fn file_len(&self, token: &FileToken) -> std::result::Result<u64, u32> {
        Ok(self.file(token)?.bytes.len() as u64)
    }
    pub fn preview_read(
        &self,
        token: &FileToken,
        count: u64,
    ) -> std::result::Result<FileReadPlan, u32> {
        let file = self.file(token)?;
        let position = file.position.get();
        let amount = count.min((file.bytes.len() as u64).saturating_sub(position)) as usize;
        let bytes = if amount == 0 {
            Vec::new()
        } else {
            file.bytes[position as usize..position as usize + amount].to_vec()
        };
        Ok(FileReadPlan {
            bytes,
            token: token.clone(),
            position,
            revision: file.revision.get(),
        })
    }
    /// Call only after all output guards. One-shot even for an EOF/zero-byte read.
    pub fn commit_read(&self, plan: &FileReadPlan) -> std::result::Result<(), u32> {
        let file = self.file(&plan.token)?;
        if file.position.get() != plan.position || file.revision.get() != plan.revision {
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
            2 => file.bytes.len() as u64,
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
        match Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref()) {
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
    /// Guest errno, matching the retained payload model (zero is success).
    pub fn close(&mut self, descriptor: u64) -> u32 {
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
        let Some(endpoint) = Self::index(descriptor).and_then(|i| self.endpoints[i].as_ref())
        else {
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
                return Err(9);
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
