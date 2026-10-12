//! Owned read-only FILE state over the shared kernel descriptor cursor.
//! Policy: FreeBSD releng/11.4 libc/stdio/{fread,fseek,ftell,flags}.c.
//! Layout: SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/freebsd/stdio.h.
//! No host FILE pointers, host paths, duplicated cursors or invented close.
use crate::descriptors::{Descriptors, FileReadPlan, FileToken};
use std::rc::Rc;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Errno(u32),
    Unsupported(&'static str),
}
type Result<T> = std::result::Result<T, Error>;
pub struct ReadPlan {
    pub elements: u64,
    pub errno: Option<u32>,
    read: Option<FileReadPlan>,
    eof: bool,
    error: bool,
    orientation: bool,
    revision: u64,
    owner: Rc<()>,
}
impl ReadPlan {
    pub fn bytes(&self) -> &[u8] {
        self.read.as_ref().map_or(&[], |read| read.bytes())
    }
}
pub struct Stream {
    token: FileToken,
    eof: bool,
    error: bool,
    orientation: bool,
    revision: u64,
    closed: bool,
    owner: Rc<()>,
}
impl Stream {
    pub fn new(token: FileToken) -> Result<Self> {
        if token.descriptor() > i16::MAX as u32 {
            return Err(Error::Errno(24));
        }
        Ok(Self {
            token,
            eof: false,
            error: false,
            orientation: false,
            revision: 0,
            closed: false,
            owner: Rc::new(()),
        })
    }
    fn live(&self) -> Result<()> {
        if self.closed {
            Err(Error::Unsupported("Closed FILE identity"))
        } else {
            Ok(())
        }
    }
    /// Snapshot only. Caller guards output bytes and FILE metadata before commit.
    pub fn read_plan(&self, descriptors: &Descriptors, size: u64, count: u64) -> Result<ReadPlan> {
        self.live()?;
        let mut plan = ReadPlan {
            elements: 0,
            errno: None,
            read: None,
            eof: self.eof,
            error: self.error,
            orientation: self.orientation,
            revision: self.revision,
            owner: self.owner.clone(),
        };
        if size == 0 || count == 0 {
            return Ok(plan);
        }
        let Some(total) = size.checked_mul(count) else {
            plan.error = true;
            plan.errno = Some(22);
            return Ok(plan);
        };
        plan.orientation = true;
        if self.eof {
            return Ok(plan);
        }
        let read = match descriptors.preview_read(&self.token, total) {
            Ok(read) => read,
            Err(errno) => {
                plan.error = true;
                plan.errno = Some(errno);
                return Ok(plan);
            }
        };
        plan.elements = read.bytes().len() as u64 / size;
        plan.eof |= (read.bytes().len() as u64) < total;
        plan.orientation = true;
        plan.read = Some(read);
        Ok(plan)
    }
    pub fn commit_read(&mut self, descriptors: &mut Descriptors, plan: &ReadPlan) -> Result<()> {
        self.live()?;
        if !Rc::ptr_eq(&self.owner, &plan.owner) || plan.revision != self.revision {
            return Err(Error::Unsupported("Foreign or stale FILE read plan"));
        }
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(Error::Unsupported("FILE revision exhausted"))?;
        if let Some(read) = &plan.read {
            descriptors.commit_read(read).map_err(Error::Errno)?;
        }
        self.revision = revision;
        self.eof = plan.eof;
        self.error = plan.error;
        self.orientation = plan.orientation;
        Ok(())
    }
    pub fn seek(&mut self, descriptors: &mut Descriptors, offset: i64, whence: i32) -> Result<()> {
        self.live()?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(Error::Unsupported("FILE revision exhausted"))?;
        descriptors
            .seek_file(&self.token, offset, whence)
            .map_err(Error::Errno)?;
        self.revision = revision;
        self.eof = false;
        Ok(())
    }
    pub fn tell(&self, descriptors: &Descriptors) -> Result<i64> {
        self.live()?;
        i64::try_from(
            descriptors
                .file_position(&self.token)
                .map_err(Error::Errno)?,
        )
        .map_err(|_| Error::Errno(84))
    }
    pub fn eof(&self) -> Result<bool> {
        self.live()?;
        Ok(self.eof)
    }
    pub fn error(&self) -> Result<bool> {
        self.live()?;
        Ok(self.error)
    }
    pub fn clearerr(&mut self) -> Result<()> {
        self.live()?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(Error::Unsupported("FILE revision exhausted"))?;
        self.eof = false;
        self.error = false;
        Ok(())
    }
    pub fn descriptor(&self) -> Result<u32> {
        self.live()?;
        Ok(self.token.descriptor())
    }
    pub fn close(&mut self, descriptors: &mut Descriptors) -> Result<()> {
        self.live()?;
        let error = descriptors.close_file(&self.token);
        self.closed = true;
        if error == 0 {
            Ok(())
        } else {
            Err(Error::Errno(error))
        }
    }
    pub fn metadata(&self, address: u64) -> Result<[u8; 312]> {
        self.live()?;
        let buffer = address
            .checked_add(119)
            .ok_or(Error::Unsupported("FILE address overflow"))?;
        let mut bytes = [0; 312];
        bytes[..8].copy_from_slice(&buffer.to_le_bytes());
        bytes[16..18].copy_from_slice(
            &(6u16 | if self.eof { 0x20 } else { 0 } | if self.error { 0x40 } else { 0 })
                .to_le_bytes(),
        );
        bytes[18..20].copy_from_slice(&(self.token.descriptor() as i16).to_le_bytes());
        bytes[24..32].copy_from_slice(&buffer.to_le_bytes());
        bytes[32..36].copy_from_slice(&1i32.to_le_bytes());
        if self.orientation {
            bytes[172..176].copy_from_slice(&(-1i32).to_le_bytes());
        }
        Ok(bytes)
    }
}
pub fn readonly_mode(mode: &[u8]) -> Result<()> {
    match mode {
        b"r" | b"rb" => Ok(()),
        b"" => Err(Error::Errno(22)),
        _ if !matches!(mode[0], b'r' | b'w' | b'a') => Err(Error::Errno(22)),
        _ => Err(Error::Unsupported(
            "FILE mode requires unsupported access policy",
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Files;
    fn setup() -> (Descriptors, Stream) {
        let mut files = Files::default();
        files.register(b"/app0/data", b"abcde".to_vec()).unwrap();
        let mut descriptors = Descriptors::default();
        let token = descriptors.open_file(&files, b"/app0/data").unwrap();
        (descriptors, Stream::new(token).unwrap())
    }
    #[test]
    fn partial_elements_eof_and_shared_seek() {
        let (mut d, mut s) = setup();
        let p = s.read_plan(&d, 2, 3).unwrap();
        assert_eq!(p.bytes(), b"abcde");
        assert_eq!(p.elements, 2);
        assert_eq!(s.tell(&d), Ok(0));
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(s.tell(&d), Ok(5));
        assert_eq!(s.eof(), Ok(true));
        s.seek(&mut d, -2, 2).unwrap();
        let p = s.read_plan(&d, 1, 2).unwrap();
        assert_eq!(p.bytes(), b"de");
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(s.eof(), Ok(false));
        let p = s.read_plan(&d, u64::MAX, 2).unwrap();
        assert_eq!(p.errno, Some(22));
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(s.error(), Ok(true));
        s.seek(&mut d, 0, 0).unwrap();
        assert_eq!(s.error(), Ok(true));
        s.clearerr().unwrap();
        assert_eq!(s.error(), Ok(false));
    }
    #[test]
    fn sticky_eof_raw_descriptor_and_metadata() {
        let (mut d, mut s) = setup();
        let p = s.read_plan(&d, 1, 6).unwrap();
        s.commit_read(&mut d, &p).unwrap();
        d.seek_file(&s.token, 0, 0).unwrap();
        let p = s.read_plan(&d, 1, 1).unwrap();
        assert!(p.bytes().is_empty());
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(s.tell(&d), Ok(0));
        s.clearerr().unwrap();
        let p = s.read_plan(&d, 1, 1).unwrap();
        assert_eq!(p.bytes(), b"a");
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(d.close_file(&s.token), 0);
        let p = s.read_plan(&d, 1, 1).unwrap();
        assert_eq!(p.errno, Some(9));
        s.commit_read(&mut d, &p).unwrap();
        assert_eq!(s.error(), Ok(true));
        let bytes = s.metadata(0x1_0000_0000).unwrap();
        assert_eq!(u16::from_le_bytes(bytes[16..18].try_into().unwrap()), 0x46);
        assert_eq!(i32::from_le_bytes(bytes[172..176].try_into().unwrap()), -1);
        assert!(bytes[48..80].iter().all(|b| *b == 0));
    }
    #[test]
    fn stale_foreign_and_retired() {
        let (mut d, mut s) = setup();
        let p = s.read_plan(&d, 1, 1).unwrap();
        d.seek_file(&s.token, 2, 0).unwrap();
        assert!(s.commit_read(&mut d, &p).is_err());
        assert_eq!(s.tell(&d), Ok(2));
        let (mut other, mut t) = setup();
        let p = s.read_plan(&d, 1, 1).unwrap();
        assert!(t.commit_read(&mut other, &p).is_err());
        s.close(&mut d).unwrap();
        assert!(s.read_plan(&d, 1, 1).is_err());
    }
}

pub const BASE: u64 = 0x7ffd_0000_0000;
pub const SLOT_SIZE: usize = 312;
pub const SLOTS: usize = 128;
pub const ARENA_SIZE: usize = SLOT_SIZE * SLOTS;
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Open,
    Read,
    Seek,
    Tell,
    Close,
    Fileno,
    Eof,
    Error,
    Clearerr,
}
#[derive(Default)]
pub struct Store {
    streams: Vec<Stream>,
}
fn text(
    memory: &crate::guest_memory::Memory<'_>,
    address: u64,
    limit: usize,
) -> crate::Result<Vec<u8>> {
    let mut out = Vec::new();
    for i in 0..limit {
        let b = memory.load(
            address
                .checked_add(i as u64)
                .ok_or("FILE string address overflow")?,
            1,
        )? as u8;
        if b == 0 {
            return Ok(out);
        }
        out.push(b);
    }
    Err("FILE string exceeds bound".into())
}
fn diagnostic(error: Error) -> String {
    format!("Unsupported FILE operation: {error:?}")
}
impl Store {
    pub fn call(
        &mut self,
        kernel: &mut crate::kernel::Kernel,
        memory: &mut crate::guest_memory::Memory<'_>,
        operation: Operation,
        args: [u64; 4],
        guard: impl Fn(&crate::guest_memory::Memory<'_>, u64, usize) -> crate::Result<()>,
    ) -> crate::Result<u64> {
        let errno_address = kernel.errno_address();
        let errno_guard = |memory: &crate::guest_memory::Memory<'_>| -> crate::Result<()> {
            guard(memory, errno_address, 4)?;
            memory.validate_write(errno_address, 4)?;
            if errno_address < BASE + ARENA_SIZE as u64
                && BASE < errno_address.checked_add(4).ok_or("FILE errno overflow")?
            {
                return Err("FILE errno overlaps FILE arena".into());
            }
            Ok(())
        };
        let fail = |memory: &mut crate::guest_memory::Memory<'_>,
                    errno: u32,
                    result: u64|
         -> crate::Result<u64> {
            errno_guard(memory)?;
            memory.store(errno_address, errno as u64, 4)?;
            Ok(result)
        };
        if matches!(operation, Operation::Open) {
            let mode = text(memory, args[1], 32)?;
            match readonly_mode(&mode) {
                Ok(()) => {}
                Err(Error::Errno(e)) => return fail(memory, e, 0),
                Err(e) => return Err(diagnostic(e)),
            }
            let path = text(memory, args[0], 1024)?;
            let slot = self
                .streams
                .iter()
                .position(|stream| stream.closed)
                .unwrap_or(self.streams.len());
            if slot == SLOTS {
                return fail(memory, 24, 0);
            }
            let address = BASE + (slot * SLOT_SIZE) as u64;
            guard(memory, address, SLOT_SIZE)?;
            memory.validate_write(address, SLOT_SIZE)?;
            if memory.read(address, SLOT_SIZE)?.iter().any(|b| *b != 0) {
                return Err("Unused FILE slot was modified".into());
            }
            let token = match kernel.descriptors.open_file(&kernel.files, &path) {
                Ok(token) => token,
                Err(e) => return fail(memory, e, 0),
            };
            let stream = match Stream::new(token.clone()) {
                Ok(stream) => stream,
                Err(error) => {
                    kernel.descriptors.close_file(&token);
                    return Err(diagnostic(error));
                }
            };
            let metadata = stream.metadata(address).map_err(diagnostic)?;
            memory.write(address, &metadata)?;
            if slot == self.streams.len() {
                self.streams.push(stream);
            } else {
                self.streams[slot] = stream;
            }
            return Ok(address);
        }
        let address = if matches!(operation, Operation::Read) {
            args[3]
        } else {
            args[0]
        };
        let index = address
            .checked_sub(BASE)
            .filter(|o| o % SLOT_SIZE as u64 == 0)
            .map(|o| o / SLOT_SIZE as u64)
            .ok_or("Unknown FILE identity")?;
        let stream = self
            .streams
            .get_mut(index as usize)
            .ok_or("Unknown FILE identity")?;
        if memory.read(address, SLOT_SIZE)? != stream.metadata(address).map_err(diagnostic)? {
            return Err("Unsupported or modified FILE metadata".into());
        }
        let mut update = false;
        let result = match operation {
            Operation::Read => {
                let plan = stream
                    .read_plan(&kernel.descriptors, args[1], args[2])
                    .map_err(diagnostic)?;
                if !plan.bytes().is_empty() {
                    let end = args[0]
                        .checked_add(plan.bytes().len() as u64)
                        .ok_or("FILE read output overflow")?;
                    if args[0] < BASE + ARENA_SIZE as u64 && BASE < end {
                        return Err("FILE read output aliases FILE arena".into());
                    }
                    guard(memory, args[0], plan.bytes().len())?;
                    memory.validate_write(args[0], plan.bytes().len())?;
                }
                guard(memory, address, SLOT_SIZE)?;
                memory.validate_write(address, SLOT_SIZE)?;
                if plan.errno.is_some() {
                    errno_guard(memory)?;
                }
                let result = plan.elements;
                let errno = plan.errno;
                // Commit can reject a stale plan before any guest byte changes.
                // All subsequent copies were fully guarded above.
                stream
                    .commit_read(&mut kernel.descriptors, &plan)
                    .map_err(diagnostic)?;
                if !plan.bytes().is_empty() {
                    memory.write(args[0], plan.bytes())?;
                }
                if let Some(e) = errno {
                    memory.store(errno_address, e as u64, 4)?;
                }
                update = true;
                result
            }
            Operation::Tell => match stream.tell(&kernel.descriptors) {
                Ok(value) => value as u64,
                Err(Error::Errno(e)) => return fail(memory, e, u64::MAX),
                Err(e) => return Err(diagnostic(e)),
            },
            Operation::Fileno => stream.descriptor().map_err(diagnostic)? as u64,
            Operation::Eof => u64::from(stream.eof().map_err(diagnostic)?),
            Operation::Error => u64::from(stream.error().map_err(diagnostic)?),
            Operation::Clearerr => {
                guard(memory, address, SLOT_SIZE)?;
                memory.validate_write(address, SLOT_SIZE)?;
                stream.clearerr().map_err(diagnostic)?;
                update = true;
                0
            }
            Operation::Seek => {
                guard(memory, address, SLOT_SIZE)?;
                memory.validate_write(address, SLOT_SIZE)?;
                match stream.seek(
                    &mut kernel.descriptors,
                    args[1] as i64,
                    args[2] as u32 as i32,
                ) {
                    Ok(()) => {
                        update = true;
                        0
                    }
                    Err(Error::Errno(e)) => return fail(memory, e, u32::MAX as u64),
                    Err(e) => return Err(diagnostic(e)),
                }
            }
            Operation::Close => {
                guard(memory, address, SLOT_SIZE)?;
                memory.validate_write(address, SLOT_SIZE)?;
                if kernel.descriptors.file_position(&stream.token).is_err() {
                    errno_guard(memory)?;
                }
                let result = stream.close(&mut kernel.descriptors);
                memory.write(address, &[0; SLOT_SIZE])?;
                return match result {
                    Ok(()) => Ok(0),
                    Err(Error::Errno(e)) => fail(memory, e, u32::MAX as u64),
                    Err(e) => Err(diagnostic(e)),
                };
            }
            Operation::Open => unreachable!(),
        };
        if update {
            memory.write(address, &stream.metadata(address).map_err(diagnostic)?)?;
        }
        Ok(result)
    }
}
#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::{guest_memory::Memory, kernel::Kernel};
    #[test]
    fn reuse_slots_and_live_limit() {
        let mut k = Kernel::new(4096, 0x2000).unwrap();
        k.files.register(b"/app0/data", b"abc".to_vec()).unwrap();
        let mut arena = vec![0; ARENA_SIZE];
        let mut strings = [0u8; 64];
        strings[..11].copy_from_slice(b"/app0/data\0");
        strings[32..35].copy_from_slice(b"rb\0");
        let mut errno = [0; 4];
        let mut m = Memory::default();
        m.add(BASE, &mut arena, true, false).unwrap();
        m.add(0x1000, &mut strings, false, false).unwrap();
        m.add(0x2000, &mut errno, true, false).unwrap();
        let mut store = Store::default();
        for _ in 0..SLOTS * 3 {
            let address = store
                .call(
                    &mut k,
                    &mut m,
                    Operation::Open,
                    [0x1000, 0x1020, 0, 0],
                    |_, _, _| Ok(()),
                )
                .unwrap();
            assert_eq!(address, BASE);
            assert_eq!(
                store
                    .call(
                        &mut k,
                        &mut m,
                        Operation::Close,
                        [address, 0, 0, 0],
                        |_, _, _| Ok(())
                    )
                    .unwrap(),
                0
            );
        }
        for i in 0..SLOTS {
            assert_eq!(
                store
                    .call(
                        &mut k,
                        &mut m,
                        Operation::Open,
                        [0x1000, 0x1020, 0, 0],
                        |_, _, _| Ok(())
                    )
                    .unwrap(),
                BASE + (i * SLOT_SIZE) as u64
            );
        }
        assert_eq!(
            store
                .call(
                    &mut k,
                    &mut m,
                    Operation::Open,
                    [0x1000, 0x1020, 0, 0],
                    |_, _, _| Ok(())
                )
                .unwrap(),
            0
        );
        assert_eq!(m.load(0x2000, 4).unwrap(), 24);
    }
}
