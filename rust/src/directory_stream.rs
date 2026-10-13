//! Owned opaque DIR profile over virtual directory descriptors.
//!
//! Handles use private 64-byte metadata and a 1024-byte batch allocation. These
//! are explicit prototype choices, not the console's private DIR layout or
//! getpagesize-dependent libc allocation sizes. No host directory is accessed.
//! readdir returns guest pointers into retained batches; a refill overwrites
//! previous records. Namespace changes invalidate further descriptor refills,
//! while already buffered records remain readable. Detached-directory refill
//! remains unsupported. No fdopendir/telldir/seekdir/locking ABI is implemented.
//!
//! Source lifecycle: FreeBSD releng/11.4 lib/libc/gen/{opendir,readdir,closedir}.c.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/closedir.c
//! closedir consumes DIR storage before closing its descriptor. A stale fd
//! therefore consumes this store entry and returns EBADF without closing a
//! newly reused fd. A raw stale DIR pointer reused for a new allocation cannot
//! be distinguished by the C pointer ABI; using a closed DIR is outside it.
use crate::{
    Result,
    descriptors::{Descriptors, DirectoryDescriptorToken},
    guest_memory::{Memory, RuntimeOwner},
    kernel::Files,
};

const HANDLE_BYTES: usize = 64;
const BATCH_BYTES: usize = 1024;
const MAX_STREAMS: usize = 256;
struct Allocation {
    address: u64,
    extent: usize,
    backing: usize,
}
impl Allocation {
    fn validate(&self, memory: &Memory<'_>, owner: &RuntimeOwner) -> Result<()> {
        if memory.runtime_heap_allocation_size(self.address, owner)? != self.extent
            || memory.read(self.address, self.extent)?.as_ptr() as usize != self.backing
        {
            return Err("Directory allocation identity changed".into());
        }
        memory.validate_runtime_write(self.address, self.extent, owner)
    }
}
struct Entry {
    handle: Allocation,
    buffer: Allocation,
    metadata: [u8; HANDLE_BYTES],
    descriptor: DirectoryDescriptorToken,
    directory: crate::kernel::DirectoryToken,
    offset: usize,
    used: usize,
}
impl Entry {
    fn validate(&self, memory: &Memory<'_>, owner: &RuntimeOwner) -> Result<()> {
        self.handle.validate(memory, owner)?;
        self.buffer.validate(memory, owner)?;
        if memory.read(self.handle.address, HANDLE_BYTES)? != self.metadata {
            return Err("Directory handle metadata changed".into());
        }
        if self.offset > self.used || self.used > BATCH_BYTES {
            return Err("Invalid directory batch cursor".into());
        }
        Ok(())
    }
}
pub struct Store {
    owner: RuntimeOwner,
    entries: Vec<Entry>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            owner: RuntimeOwner::new(),
            entries: Vec::new(),
        }
    }
}
impl Store {
    fn allocate(
        &self,
        memory: &mut Memory<'_>,
        size: usize,
    ) -> Result<std::result::Result<Allocation, u32>> {
        let address = match memory.malloc(size as u64) {
            Ok(address) => address,
            Err(errno) => return Ok(Err(errno)),
        };
        if let Err(error) = memory.mark_runtime_heap(address, &self.owner) {
            memory.free(address)?;
            return Err(error);
        }
        let prepared = (|| {
            let extent = memory.runtime_heap_allocation_size(address, &self.owner)?;
            let backing = memory.read(address, extent)?.as_ptr() as usize;
            let allocation = Allocation {
                address,
                extent,
                backing,
            };
            allocation.validate(memory, &self.owner)?;
            Ok(allocation)
        })();
        match prepared {
            Ok(allocation) => Ok(Ok(allocation)),
            Err(error) => {
                memory.runtime_free(address, &self.owner)?;
                Err(error)
            }
        }
    }
    pub fn open(
        &mut self,
        memory: &mut Memory<'_>,
        files: &Files,
        descriptors: &mut Descriptors,
        path: &[u8],
        cwd: Option<&[u8]>,
    ) -> Result<std::result::Result<u64, u32>> {
        if self.entries.len() == MAX_STREAMS {
            return Err("Directory stream table limit reached".into());
        }
        self.entries
            .try_reserve(1)
            .map_err(|_| "Cannot reserve directory stream entry")?;
        let descriptor = match descriptors.open_directory(files, path, cwd)? {
            Ok(token) => token,
            Err(errno) => return Ok(Err(errno)),
        };
        let directory = descriptors
            .directory_backing(&descriptor)
            .expect("New directory descriptor")
            .clone();
        let mut handle = None;
        let mut buffer = None;
        let prepared: Result<std::result::Result<[u8; HANDLE_BYTES], u32>> = (|| {
            handle = Some(match self.allocate(memory, HANDLE_BYTES)? {
                Ok(allocation) => allocation,
                Err(errno) => return Ok(Err(errno)),
            });
            buffer = Some(match self.allocate(memory, BATCH_BYTES)? {
                Ok(allocation) => allocation,
                Err(errno) => return Ok(Err(errno)),
            });
            let mut metadata = [0; HANDLE_BYTES];
            metadata[..8].copy_from_slice(b"MacDIR01");
            metadata[8..16].copy_from_slice(&u64::from(descriptor.descriptor()).to_le_bytes());
            metadata[16..24].copy_from_slice(&buffer.as_ref().unwrap().address.to_le_bytes());
            memory.runtime_write(handle.as_ref().unwrap().address, &metadata, &self.owner)?;
            Ok(Ok(metadata))
        })();
        let metadata = match prepared {
            Ok(Ok(metadata)) => metadata,
            failure => {
                if let Some(buffer) = buffer {
                    memory.runtime_free(buffer.address, &self.owner)?;
                }
                if let Some(handle) = handle {
                    memory.runtime_free(handle.address, &self.owner)?;
                }
                if descriptors.close_directory(&descriptor) != 0 {
                    return Err("Directory open rollback lost descriptor ownership".into());
                }
                return match failure {
                    Ok(Err(errno)) => Ok(Err(errno)),
                    Err(error) => Err(error),
                    _ => unreachable!(),
                };
            }
        };
        let handle = handle.unwrap();
        let address = handle.address;
        self.entries.push(Entry {
            handle,
            buffer: buffer.unwrap(),
            metadata,
            descriptor,
            directory,
            offset: 0,
            used: 0,
        });
        Ok(Ok(address))
    }
    pub fn read(
        &mut self,
        memory: &mut Memory<'_>,
        files: &Files,
        descriptors: &Descriptors,
        handle: u64,
    ) -> Result<std::result::Result<u64, u32>> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.handle.address == handle)
            .ok_or("Unknown directory stream handle")?;
        entry.validate(memory, &self.owner)?;
        if !files.owns_directory_token(&entry.directory) {
            return Err("Directory stream belongs to another namespace".into());
        }
        if entry.offset == entry.used {
            if descriptors.directory_backing(&entry.descriptor).is_err() {
                return Ok(Err(9));
            }
            let plan =
                descriptors.preview_directory_read(files, &entry.descriptor, BATCH_BYTES as u64)?;
            descriptors.validate_directory_read(files, &plan)?;
            // No guest execution or callbacks between validation, owned copy,
            // and cursor commit. Both allocations were validated above.
            memory.runtime_write(entry.buffer.address, plan.bytes(), &self.owner)?;
            descriptors.commit_directory_read(files, &plan)?;
            entry.used = plan.bytes().len();
            entry.offset = 0;
            if entry.used == 0 {
                return Ok(Ok(0));
            }
        }
        let address = entry.buffer.address + entry.offset as u64;
        let record = memory.read(address, entry.used - entry.offset)?;
        if record.len() < 8 {
            return Err("Short owned directory record".into());
        }
        let length = usize::from(u16::from_le_bytes([record[4], record[5]]));
        let name_length = usize::from(record[7]);
        if length < 12
            || length % 4 != 0
            || length > record.len()
            || name_length == 0
            || 8 + name_length >= length
            || record[8 + name_length] != 0
        {
            return Err("Invalid owned directory record".into());
        }
        entry.offset += length;
        Ok(Ok(address))
    }
    /// Caller may guard errno storage before teardown only when failure is
    /// expected. Call close without intervening descriptor mutation.
    pub fn close_error(&self, descriptors: &Descriptors, handle: u64) -> Result<Option<u32>> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.handle.address == handle)
            .ok_or("Unknown directory stream handle")?;
        Ok(descriptors.directory_backing(&entry.descriptor).err())
    }
    pub fn close(
        &mut self,
        memory: &mut Memory<'_>,
        descriptors: &mut Descriptors,
        handle: u64,
    ) -> Result<std::result::Result<(), u32>> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.handle.address == handle)
            .ok_or("Unknown directory stream handle")?;
        let entry = &self.entries[index];
        entry.validate(memory, &self.owner)?;
        // Capture validity before freeing; never close a reused descriptor.
        let valid_descriptor = descriptors.directory_backing(&entry.descriptor).is_ok();
        memory.runtime_free(entry.buffer.address, &self.owner)?;
        memory.runtime_free(entry.handle.address, &self.owner)?;
        let entry = self.entries.swap_remove(index);
        if !valid_descriptor {
            return Ok(Err(9));
        }
        match descriptors.close_directory(&entry.descriptor) {
            0 => Ok(Ok(())),
            errno => Ok(Err(errno)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    fn files() -> Files {
        let mut files = Files::default();
        files.register_directory(b"/save", 0o777, false).unwrap();
        files
    }
    fn name(memory: &Memory<'_>, address: u64) -> Vec<u8> {
        let length = memory.load(address + 7, 1).unwrap() as usize;
        memory.read(address + 8, length).unwrap().to_vec()
    }
    #[test]
    fn batches_preserve_pointers_and_refill_only_when_exhausted() {
        let mut files = files();
        for n in 0..100 {
            files
                .register(format!("/save/item{n:03}").as_bytes(), vec![])
                .unwrap();
        }
        let mut mappings = OwnedMappings::default();
        let mut memory = Memory::default();
        memory.attach_owned(&mut mappings).unwrap();
        let mut descriptors = Descriptors::default();
        let mut store = Store::default();
        let handle = store
            .open(&mut memory, &files, &mut descriptors, b"/save", None)
            .unwrap()
            .unwrap();
        let first = store
            .read(&mut memory, &files, &descriptors, handle)
            .unwrap()
            .unwrap();
        assert_eq!(name(&memory, first), b".");
        let token = store.entries[0].descriptor.clone();
        let first_batch_end = descriptors.directory_position(&token).unwrap();
        assert!(first_batch_end > 2 && first_batch_end < 102);
        let first_record = memory.read(first, 12).unwrap().to_vec();
        let second = store
            .read(&mut memory, &files, &descriptors, handle)
            .unwrap()
            .unwrap();
        assert_eq!(second, first + 12);
        assert_eq!(name(&memory, second), b"..");
        assert_eq!(memory.read(first, 12).unwrap(), first_record);
        assert_eq!(
            descriptors.directory_position(&token).unwrap(),
            first_batch_end
        );
        let mut names = vec![b".".to_vec(), b"..".to_vec()];
        for _ in 2..102 {
            let address = store
                .read(&mut memory, &files, &descriptors, handle)
                .unwrap()
                .unwrap();
            names.push(name(&memory, address));
        }
        assert_eq!(names.len(), 102);
        for n in 0..100 {
            assert_eq!(names[n + 2], format!("item{n:03}").as_bytes());
        }
        for _ in 0..2 {
            assert_eq!(
                store
                    .read(&mut memory, &files, &descriptors, handle)
                    .unwrap(),
                Ok(0)
            );
        }
        assert!(memory.write(handle, &[0]).is_err());
        assert!(memory.write(first, &[0]).is_err());
        assert!(memory.free(handle).is_err());
        assert!(memory.free(first).is_err());
        assert_eq!(store.close_error(&descriptors, handle).unwrap(), None);
        assert_eq!(
            store.close(&mut memory, &mut descriptors, handle).unwrap(),
            Ok(())
        );
        assert!(memory.read(handle, 1).is_err());
        assert!(memory.read(first, 1).is_err());
        assert!(store.close(&mut memory, &mut descriptors, handle).is_err());
    }
    #[test]
    fn cached_entries_survive_fd_close_but_refill_reports_ebadf_and_teardown_keeps_reuse() {
        let files = files();
        let mut mappings = OwnedMappings::default();
        let mut memory = Memory::default();
        memory.attach_owned(&mut mappings).unwrap();
        let mut descriptors = Descriptors::default();
        let mut store = Store::default();
        let handle = store
            .open(&mut memory, &files, &mut descriptors, b"/save", None)
            .unwrap()
            .unwrap();
        store
            .read(&mut memory, &files, &descriptors, handle)
            .unwrap()
            .unwrap();
        let old = store.entries[0].descriptor.clone();
        assert_eq!(descriptors.close_directory(&old), 0);
        let replacement = descriptors
            .open_directory(&files, b"/save", None)
            .unwrap()
            .unwrap();
        assert_eq!(old.descriptor(), replacement.descriptor());
        let cached = store
            .read(&mut memory, &files, &descriptors, handle)
            .unwrap()
            .unwrap();
        assert_eq!(name(&memory, cached), b"..");
        assert_eq!(
            store
                .read(&mut memory, &files, &descriptors, handle)
                .unwrap(),
            Err(9)
        );
        assert_eq!(store.close_error(&descriptors, handle).unwrap(), Some(9));
        assert_eq!(
            store.close(&mut memory, &mut descriptors, handle).unwrap(),
            Err(9)
        );
        assert!(descriptors.directory_backing(&replacement).is_ok());
        assert!(memory.read(handle, 1).is_err());
    }
    #[test]
    fn opening_rolls_back_descriptor_and_both_allocation_stages() {
        let files = files();
        let mut descriptors = Descriptors::default();
        let mut store = Store::default();
        let mut no_heap = Memory::default();
        assert_eq!(
            store
                .open(&mut no_heap, &files, &mut descriptors, b"/save", None)
                .unwrap(),
            Err(12)
        );
        assert!(descriptors.directory_token(3).is_err());
        let mut mappings = OwnedMappings::default();
        {
            let mut memory = Memory::default();
            memory.attach_owned(&mut mappings).unwrap();
            // First handle succeeds; claiming the second allocation encounters
            // an active write barrier and must release both blocks plus fd.
            memory.set_write_barriers(vec![(0x400000040, 1)]);
            assert!(
                store
                    .open(&mut memory, &files, &mut descriptors, b"/save", None)
                    .is_err()
            );
            assert!(descriptors.directory_token(3).is_err());
            assert!(store.entries.is_empty());
            memory.set_write_barriers(Vec::new());
            let handle = store
                .open(&mut memory, &files, &mut descriptors, b"/save", None)
                .unwrap()
                .unwrap();
            assert_eq!(handle, 0x400000000);
            store
                .close(&mut memory, &mut descriptors, handle)
                .unwrap()
                .unwrap();
        }
        assert_eq!(mappings.bytes_allocated(), 0);
    }
    #[test]
    fn owned_mapping_frame_guards_and_foreign_stores_are_atomic() {
        let files = files();
        let mut mappings = OwnedMappings::default();
        let mut memory = Memory::default();
        memory.attach_owned(&mut mappings).unwrap();
        let mut descriptors = Descriptors::default();
        let mut store = Store::default();
        let handle = store
            .open(&mut memory, &files, &mut descriptors, b"/save", None)
            .unwrap()
            .unwrap();
        let buffer = store.entries[0].buffer.address;
        let token = store.entries[0].descriptor.clone();
        memory.set_write_barriers(vec![(buffer + 1023, 1)]);
        assert!(
            store
                .read(&mut memory, &files, &descriptors, handle)
                .is_err()
        );
        assert_eq!(descriptors.directory_position(&token).unwrap(), 0);
        assert!(store.close(&mut memory, &mut descriptors, handle).is_err());
        assert!(memory.read(handle, 64).is_ok());
        assert!(descriptors.directory_backing(&token).is_ok());
        memory.set_write_barriers(Vec::new());
        assert!(
            Store::default()
                .read(&mut memory, &files, &descriptors, handle)
                .is_err()
        );
        assert!(
            store
                .read(&mut memory, &Files::default(), &descriptors, handle)
                .is_err()
        );
        let mut other_memory = Memory::default();
        assert!(
            store
                .read(&mut other_memory, &files, &descriptors, handle)
                .is_err()
        );
        assert_eq!(descriptors.directory_position(&token).unwrap(), 0);
        store
            .close(&mut memory, &mut descriptors, handle)
            .unwrap()
            .unwrap();
    }
    #[test]
    fn cached_namespace_records_survive_mutation_until_refill_and_mappings_survive_resume() {
        let mut files = files();
        let mut mappings = OwnedMappings::default();
        let mut descriptors = Descriptors::default();
        let mut store = Store::default();
        let handle;
        {
            let mut memory = Memory::default();
            memory.attach_owned(&mut mappings).unwrap();
            handle = store
                .open(&mut memory, &files, &mut descriptors, b"/save", None)
                .unwrap()
                .unwrap();
            store
                .read(&mut memory, &files, &descriptors, handle)
                .unwrap()
                .unwrap();
        }
        files.register(b"/save/new", vec![]).unwrap();
        let mut memory = Memory::default();
        memory.attach_owned(&mut mappings).unwrap();
        let cached = store
            .read(&mut memory, &files, &descriptors, handle)
            .unwrap()
            .unwrap();
        assert_eq!(name(&memory, cached), b"..");
        assert!(
            store
                .read(&mut memory, &files, &descriptors, handle)
                .unwrap_err()
                .contains("changed")
        );
        store
            .close(&mut memory, &mut descriptors, handle)
            .unwrap()
            .unwrap();
    }
}
