//! Explicit process-owned captured stdout, separate from kernel diagnostic logs.
//! Primary success ABI: FreeBSD11.4 lib/libc/stdio/puts.c returns '\n' (10),
//! not a byte count, after writing the string and newline to stdout:
//! https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/stdio/puts.c
//! Real stream errors return EOF; this capture-only subset has no FILE backend.
//! Guard, allocation and policy-limit failures are explicit runtime diagnostics,
//! not invented stream errno values. No host FILE or guest pointer is forwarded.
use crate::{Result, guest_memory::Memory};
pub const MAX_CAPACITY: usize = 1024 * 1024;
pub const STRING_LIMIT: usize = 65_536;
#[derive(Debug, Default)]
pub struct CapturedStdout {
    capacity: Option<usize>,
    bytes: Vec<u8>,
}
impl CapturedStdout {
    /// Configure once; callers drain explicitly to recover capture capacity.
    pub fn configure_capture(&mut self, capacity: usize) -> Result<()> {
        if self.capacity.is_some() || !(1..=MAX_CAPACITY).contains(&capacity) {
            return Err("Invalid or already configured stdout capture".into());
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
    pub fn puts(&mut self, memory: &Memory<'_>, source: u64) -> Result<i32> {
        let capacity = self
            .capacity
            .ok_or("Unsupported puts: stdout capture is not configured")?;
        if source == 0 {
            return Err("Null puts source".into());
        }
        let mut line = Vec::new();
        for offset in 0..STRING_LIMIT {
            let address = source
                .checked_add(offset as u64)
                .ok_or("Puts source address overflow")?;
            let byte = memory.load(address, 1)? as u8;
            line.try_reserve(1)
                .map_err(|_| "Cannot allocate bounded puts snapshot")?;
            if byte == 0 {
                line.push(b'\n');
                let length = self
                    .bytes
                    .len()
                    .checked_add(line.len())
                    .ok_or("Stdout capture size overflow")?;
                if length > capacity {
                    return Err("Stdout capture capacity exhausted; no output committed".into());
                }
                self.bytes
                    .try_reserve(line.len())
                    .map_err(|_| "Cannot allocate stdout capture storage")?;
                self.bytes.extend_from_slice(&line);
                return Ok(10);
            }
            line.push(byte);
        }
        Err("Puts source exceeds bounded string limit".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_capture_appends_bytes_and_newline_exactly_once() {
        let mut source = *b"abc\0\0\xff\n\0";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut source, true, false).unwrap();
        let mut output = CapturedStdout::default();
        assert!(output.puts(&memory, 0x1000).is_err());
        assert!(output.configure_capture(0).is_err());
        assert!(output.configure_capture(MAX_CAPACITY + 1).is_err());
        output.configure_capture(8).unwrap();
        assert!(output.configure_capture(8).is_err());
        assert_eq!(output.puts(&memory, 0x1000).unwrap(), 10);
        memory.write(0x1000, b"XYZ").unwrap();
        assert_eq!(output.bytes(), b"abc\n");
        assert_eq!(output.puts(&memory, 0x1004).unwrap(), 10);
        assert_eq!(output.puts(&memory, 0x1005).unwrap(), 10);
        assert_eq!(output.bytes(), b"abc\n\n\xff\n\n");
        assert!(output.puts(&memory, 0x1004).is_err());
        assert_eq!(output.take_bytes(), b"abc\n\n\xff\n\n");
        assert!(output.bytes().is_empty());
        assert_eq!(output.puts(&memory, 0x1000).unwrap(), 10);
        assert_eq!(output.bytes(), b"XYZ\n");
        assert!(CapturedStdout::default().bytes().is_empty());
    }
    #[test]
    fn invalid_unterminated_or_overcapacity_never_partially_commit() {
        let mut source = vec![65; STRING_LIMIT];
        let mut text = *b"ok\0too long\0";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut source, false, false).unwrap();
        memory.add(0x20000, &mut text, false, false).unwrap();
        let mut output = CapturedStdout::default();
        output.configure_capture(4).unwrap();
        output.puts(&memory, 0x20000).unwrap();
        for address in [0, 1, 0x1000, 0x10fff, u64::MAX, 0x20003] {
            assert!(output.puts(&memory, address).is_err());
            assert_eq!(output.bytes(), b"ok\n");
        }
        assert_eq!(memory.read(0x20000, 3).unwrap(), b"ok\0");
    }
}
