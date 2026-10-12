//! Explicit read-only guest files. No host filesystem lookup or path forwarding.
//! ABI: ps5-payload-dev/sdk include/freebsd/sys/stat.h (FreeBSD 11.4 amd64),
//! corroborated by AnyPS5/core/libs/SceTypes.hpp::FileStat. Serialize fields
//! explicitly: host struct stat has a different layout on Apple Silicon.
//! https://raw.githubusercontent.com/ps5-payload-dev/sdk/master/include/freebsd/sys/stat.h
//! https://raw.githubusercontent.com/ps5-payload-dev/sdk/master/include/freebsd/sys/_types.h
use crate::{Result, guest_memory::Memory};
use std::{collections::BTreeMap, rc::Rc};

pub const STAT_SIZE: usize = 120;
const PATH_MAX: usize = 1024;
const MAX_FILES: usize = 256;
const MAX_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRACE_RECORDS: usize = 256;
const MAX_TRACE_BYTES: usize = 65536;
struct File {
    bytes: Rc<[u8]>,
    inode: u32,
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
#[derive(Default)]
pub struct Files {
    files: BTreeMap<Vec<u8>, File>,
    content_bytes: usize,
    tracing: bool,
    trace: StatTrace,
    trace_bytes: usize,
}
impl Files {
    /// Register actual owned contents under an exact canonical absolute guest
    /// path. Regular files only; no cwd, symlinks, directories or host metadata.
    pub fn register(&mut self, path: &[u8], bytes: Vec<u8>) -> Result<()> {
        if !canonical(path) || self.files.contains_key(path) || self.files.len() >= MAX_FILES {
            return Err("Invalid, duplicate or excessive guest file registration".into());
        }
        let total = self
            .content_bytes
            .checked_add(bytes.len())
            .ok_or("Guest file budget overflow")?;
        if total > MAX_CONTENT_BYTES {
            return Err("Guest file content budget exceeded".into());
        }
        let inode = self.files.len() as u32 + 1;
        self.files.insert(
            path.to_vec(),
            File {
                bytes: bytes.into(),
                inode,
            },
        );
        self.content_bytes = total;
        Ok(())
    }
    /// Shared immutable backing; opening a descriptor never copies file contents.
    /// Kept behind this API so a later explicit mounted-file backend can supply
    /// bounded read-at chunks without changing stream/descriptor cursor ownership.
    pub(crate) fn contents(&self, path: &[u8]) -> std::result::Result<Rc<[u8]>, u32> {
        self.files.get(path).map(|f| f.bytes.clone()).ok_or(2)
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
        self.content_bytes
    }
    pub fn stat(
        &mut self,
        user: &mut Memory<'_>,
        path_address: u64,
        output: u64,
    ) -> std::result::Result<u64, u32> {
        let path = read_path(user, path_address)?;
        let result = self.stat_path(user, &path, output);
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
        let file = self.files.get(path).ok_or(2u32)?;
        let mut record = [0u8; STAT_SIZE];
        // A deterministic virtual regular-file identity, with read permissions,
        // one link, zero owner/timestamps, and 512-byte allocation units.
        record[0..4].copy_from_slice(&1u32.to_le_bytes());
        record[4..8].copy_from_slice(&file.inode.to_le_bytes());
        record[8..10].copy_from_slice(&0o100444u16.to_le_bytes());
        record[10..12].copy_from_slice(&1u16.to_le_bytes());
        record[72..80].copy_from_slice(&(file.bytes.len() as i64).to_le_bytes());
        record[80..88].copy_from_slice(&(file.bytes.len().div_ceil(512) as i64).to_le_bytes());
        record[88..92].copy_from_slice(&512u32.to_le_bytes());
        Ok(record)
    }
    fn stat_path(
        &self,
        user: &mut Memory<'_>,
        path: &[u8],
        output: u64,
    ) -> std::result::Result<u64, u32> {
        // This namespace intentionally has no implicit cwd/path normalization.
        // Non-canonical paths cannot name a registered file.
        let record = self.stat_record(path)?;
        if output == 0 {
            return Err(14);
        }
        // Memory::write validates the entire range before touching any byte.
        user.write(output, &record).map_err(|_| 14u32)?;
        Ok(0)
    }
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
        assert_eq!(u32::from_le_bytes(record[4..8].try_into().unwrap()), 1);
        assert_eq!(
            u16::from_le_bytes(record[8..10].try_into().unwrap()),
            0o100444
        );
        assert_eq!(u16::from_le_bytes(record[10..12].try_into().unwrap()), 1);
        assert_eq!(i64::from_le_bytes(record[72..80].try_into().unwrap()), 513);
        assert_eq!(i64::from_le_bytes(record[80..88].try_into().unwrap()), 2);
        assert_eq!(u32::from_le_bytes(record[88..92].try_into().unwrap()), 512);
        assert!(record[12..72].iter().all(|b| *b == 0));
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
        files.content_bytes = MAX_CONTENT_BYTES;
        assert!(files.register(b"/over", vec![1]).is_err());
        files.content_bytes = 1;
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
