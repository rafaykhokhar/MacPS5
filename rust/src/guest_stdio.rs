//! Owned unbuffered stderr subset; no host FILE pointers or buffered callbacks.
//! ABI pinned to SDK revision 10b539de8f2b6773a9a78caa7576a97dbd99b29a:
//! https://github.com/ps5-payload-dev/sdk/blob/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/stdio.h
//! LP64 __sFILE is 312 bytes; __mbstate_t is 128 bytes aligned to eight.
//! FreeBSD releng/11.4 libc/stdio/fflush.c establishes zero on completed flush.
//! Unsupported state/resource errors are diagnostics, not invented errno/EOF.
use crate::{Result, format, guest_memory::Memory, strings::Fault};
pub const FILE_ADDR: u64 = 0x7fff_0000_1000;
pub const STDERR_PTR_ADDR: u64 = 0x7fff_0000_1200;
pub const FILE_SIZE: usize = 312;
pub const MAX_CAPACITY: usize = 1024 * 1024;
/// Real unbuffered FILE metadata. Callback execution is unsupported; null slots
/// never become host pointers. The sole buffer is the FILE's owned _nbuf byte.
pub fn stderr_file_bytes() -> [u8; FILE_SIZE] {
    let mut bytes = [0; FILE_SIZE];
    let buffer = (FILE_ADDR + 119).to_le_bytes();
    bytes[0..8].copy_from_slice(&buffer);
    bytes[16..18].copy_from_slice(&0xau16.to_le_bytes());
    bytes[18..20].copy_from_slice(&2i16.to_le_bytes());
    bytes[24..32].copy_from_slice(&buffer);
    bytes[32..36].copy_from_slice(&1i32.to_le_bytes());
    bytes
}
pub fn stderr_pointer_bytes() -> [u8; 8] {
    FILE_ADDR.to_le_bytes()
}
#[derive(Debug, Default)]
pub struct GuestStdio {
    capacity: Option<usize>,
    bytes: Vec<u8>,
}
impl GuestStdio {
    pub fn configure_capture(&mut self, capacity: usize) -> Result<()> {
        if self.capacity.is_some() || !(1..=MAX_CAPACITY).contains(&capacity) {
            return Err("Invalid or already configured stderr capture".into());
        }
        self.capacity = Some(capacity);
        Ok(())
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
    fn validate(&self, memory: &Memory<'_>, stream: u64) -> Result<usize> {
        let capacity = self.capacity.ok_or("Stderr capture is not configured")?;
        if stream != FILE_ADDR {
            return Err("Unsupported guest FILE identity".into());
        }
        let expected = stderr_file_bytes();
        let mut actual = [0; FILE_SIZE];
        actual.copy_from_slice(memory.read(stream, FILE_SIZE)?);
        let orientation = i32::from_le_bytes(actual[172..176].try_into().unwrap());
        if orientation != 0 && orientation != -1 {
            return Err("Unsupported wide-oriented stderr FILE".into());
        }
        actual[172..176].fill(0);
        // Buffer content may change without changing the unbuffered invariant.
        actual[119] = 0;
        if actual != expected {
            return Err("Unsupported or corrupted stderr FILE state".into());
        }
        Ok(capacity)
    }
    /// Render all arguments before committing any capture bytes. Caller validates
    /// its return frame/target before invoking this irreversible stream operation.
    pub fn fprintf(
        &mut self,
        memory: &mut Memory<'_>,
        stream: u64,
        format_address: u64,
        next: &mut impl FnMut(&Memory<'_>) -> std::result::Result<u64, Fault>,
    ) -> Result<i32> {
        let capacity = self.validate(memory, stream)?;
        memory.validate_write(FILE_ADDR + 172, 4)?;
        if format_address == 0 {
            return Err("Null fprintf format".into());
        }
        let rendered = format::render(memory, format_address, &mut || next(memory))
            .map_err(|fault| format!("Unsupported or invalid fprintf formatting: {fault:?}"))?;
        let total = self
            .bytes
            .len()
            .checked_add(rendered.len())
            .ok_or("Stderr capture overflow")?;
        if total > capacity {
            return Err("Stderr capture capacity exhausted; no output committed".into());
        }
        self.bytes
            .try_reserve(rendered.len())
            .map_err(|_| "Cannot allocate stderr capture")?;
        memory.write(FILE_ADDR + 172, &(-1i32).to_le_bytes())?;
        self.bytes.extend_from_slice(&rendered);
        Ok(rendered.len() as i32)
    }
    /// NULL flushes all FILE streams owned here (currently just stderr). Every
    /// accepted byte is already committed; no buffered/callback flush is faked.
    pub fn fflush(&self, memory: &Memory<'_>, stream: u64) -> Result<i32> {
        self.validate(memory, if stream == 0 { FILE_ADDR } else { stream })?;
        Ok(0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_layout_and_observed_format() {
        let mut file = stderr_file_bytes();
        let mut pointer = stderr_pointer_bytes();
        let mut text = *b"\n\n%s\n%s\n\n\0SDL\0message\0";
        let mut memory = Memory::default();
        memory.add(FILE_ADDR, &mut file, true, false).unwrap();
        memory
            .add(STDERR_PTR_ADDR, &mut pointer, false, false)
            .unwrap();
        memory.add(0x1000, &mut text, false, false).unwrap();
        assert_eq!(memory.load(STDERR_PTR_ADDR, 8).unwrap(), FILE_ADDR);
        assert_eq!(memory.load(FILE_ADDR + 16, 2).unwrap(), 10);
        assert!(memory.write(STDERR_PTR_ADDR, &[0; 8]).is_err());
        let mut stdio = GuestStdio::default();
        assert!(stdio.fflush(&memory, 0).is_err());
        stdio.configure_capture(128).unwrap();
        let mut args = [0x100a, 0x100e].into_iter();
        assert_eq!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1000, &mut |_| args
                    .next()
                    .ok_or(Fault::Read))
                .unwrap(),
            15
        );
        assert_eq!(stdio.bytes(), b"\n\nSDL\nmessage\n\n");
        assert_eq!(stdio.fflush(&memory, 0).unwrap(), 0);
        assert_eq!(stdio.fflush(&memory, FILE_ADDR).unwrap(), 0);
        assert_eq!(stdio.take_bytes(), b"\n\nSDL\nmessage\n\n");
    }
    #[test]
    fn invalid_inputs_never_commit() {
        let mut file = stderr_file_bytes();
        let mut text = *b"ok\0%s\0%f\0toolong\0";
        let mut memory = Memory::default();
        memory.add(FILE_ADDR, &mut file, true, false).unwrap();
        memory.add(0x1000, &mut text, false, false).unwrap();
        let mut stdio = GuestStdio::default();
        stdio.configure_capture(2).unwrap();
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1003, &mut |_| Ok(u64::MAX))
                .is_err()
        );
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1006, &mut |_| Ok(0))
                .is_err()
        );
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1009, &mut |_| Ok(0))
                .is_err()
        );
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0, &mut |_| Ok(0))
                .is_err()
        );
        assert!(stdio.fflush(&memory, FILE_ADDR + 8).is_err());
        memory.write(FILE_ADDR + 12, &1i32.to_le_bytes()).unwrap();
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1000, &mut |_| Ok(0))
                .is_err()
        );
        assert!(stdio.fflush(&memory, 0).is_err());
        memory.write(FILE_ADDR + 12, &0i32.to_le_bytes()).unwrap();
        assert!(stdio.bytes().is_empty());
        assert_eq!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1000, &mut |_| Ok(0))
                .unwrap(),
            2
        );
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1000, &mut |_| Ok(0))
                .is_err()
        );
        assert_eq!(stdio.bytes(), b"ok");
    }
}
#[cfg(test)]
mod guarded_tests {
    use super::*;
    #[test]
    fn readonly_or_wide_file_and_argument_fault_are_atomic() {
        let mut file = stderr_file_bytes();
        let mut text = *b"%s\0ok\0";
        let mut memory = Memory::default();
        memory.add(FILE_ADDR, &mut file, false, false).unwrap();
        memory.add(0x1000, &mut text, false, false).unwrap();
        let mut stdio = GuestStdio::default();
        stdio.configure_capture(128).unwrap();
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1003, &mut |_| Ok(0))
                .is_err()
        );
        assert!(stdio.bytes().is_empty());
        drop(memory);
        let mut memory = Memory::default();
        memory.add(FILE_ADDR, &mut file, true, false).unwrap();
        memory.add(0x1000, &mut text, false, false).unwrap();
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1000, &mut |_| Err(Fault::Read))
                .is_err()
        );
        assert_eq!(memory.load(FILE_ADDR + 172, 4).unwrap(), 0);
        memory.write(FILE_ADDR + 172, &1i32.to_le_bytes()).unwrap();
        assert!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1003, &mut |_| Ok(0))
                .is_err()
        );
        assert!(stdio.bytes().is_empty());
        memory.write(FILE_ADDR + 172, &0i32.to_le_bytes()).unwrap();
        assert_eq!(
            stdio
                .fprintf(&mut memory, FILE_ADDR, 0x1003, &mut |_| Ok(0))
                .unwrap(),
            2
        );
        assert_eq!(memory.load(FILE_ADDR + 172, 4).unwrap(), u32::MAX as u64);
        assert_eq!(stdio.fflush(&memory, 0).unwrap(), 0);
    }
}
