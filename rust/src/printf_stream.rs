//! Owned, non-reentrant __sbprintf rendering adapter. The caller selects eligible
//! FILEs, validates/prepares/locks the original FILE, and handles its error flags.
//! This module models only the fixed temporary buffer and ordered rendering.
//! It does not implement ordinary FILE buffering, custom guest write callbacks,
//! normal exit flushing, or an allocating vasprintf intermediate.
//!
//! Source: FreeBSD releng/11.4 lib/libc/stdio/vfprintf.c (__sbprintf).
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vfprintf.c
//! Guest vectors are resolved at batch traversal time. The callback receives
//! borrowed bytes synchronously and must not retain them or reenter guest code.
//! Ok(n) commits exactly that prefix; Err commits no bytes for that call. Earlier
//! accepted calls remain observable on every later error. No invented errno is
//! assigned for a zero write, and capture policy failures are not stream errors.
use crate::{
    guest_memory::Memory,
    printf_buffer::{Error, Kind},
    printf_output::{Batch, Vector},
    printf_render::{self, HexContext, Options, Target},
    printf_stream_buffer::{self, StreamBuffer},
};

#[derive(Debug, PartialEq, Eq)]
pub enum WriteError {
    /// Actual stream failure. None means preserve the caller's existing errno.
    Stream { errno: Option<u32> },
    /// Runtime/capture policy failure; do not synthesize FILE error or errno.
    Policy(String),
}
fn write_error(error: printf_stream_buffer::Error<WriteError>) -> Error {
    match error {
        printf_stream_buffer::Error::Backend(WriteError::Stream { errno }) => {
            Kind::Stream(errno).into()
        }
        printf_stream_buffer::Error::Backend(WriteError::Policy(message)) => {
            Kind::Runtime(message).into()
        }
        printf_stream_buffer::Error::ZeroWrite => Kind::Stream(None).into(),
        other => Kind::Runtime(format!("Printf stream backend invariant: {other:?}")).into(),
    }
}
struct StreamTarget<'m, 'a, 'b, B> {
    memory: &'m mut Memory<'a>,
    buffer: StreamBuffer,
    backend: &'b mut B,
}
impl<'a, B: FnMut(&[u8]) -> Result<usize, WriteError>> Target<'a> for StreamTarget<'_, 'a, '_, B> {
    type Ranges = [(u64, usize); 0];
    fn memory(&mut self) -> &mut Memory<'a> {
        self.memory
    }
    fn private_ranges(&self) -> Result<Self::Ranges, Error> {
        // The fake FILE buffer is host-owned and never exposed as guest storage.
        // Wide/gdtoa allocations remain separately guarded by the renderer.
        Ok([])
    }
    fn validate_external_write(&self, address: u64, length: usize) -> Result<(), Error> {
        self.memory.validate_write(address, length)?;
        Ok(())
    }
    fn submit_batch(&mut self, batch: Batch<'_>) -> Result<(), Error> {
        batch.submit(|vector| {
            let padding;
            let bytes = match vector {
                Vector::Guest { address, length } => self.memory.read(*address, *length)?,
                Vector::Owned(bytes) => bytes,
                Vector::Pad { byte, length } => {
                    padding = [*byte; 16];
                    padding.get(..*length).ok_or_else(|| {
                        Kind::Runtime("Printf padding vector exceeds source PADSIZE".into())
                    })?
                }
            };
            self.buffer
                .write_vector(bytes, self.backend)
                .map_err(write_error)
        })
    }
}
/// Render and then flush the fake FILE only on success. The returned count is
/// logical formatted bytes, including bytes buffered when %n executed. Caller
/// owns errno/FILE updates and must retain cleanup diagnostics on any failure.
/// Backend callbacks cannot access mutable guest memory or suspend/reenter.
pub fn run(
    memory: &mut Memory<'_>,
    format: u64,
    va: u64,
    options: Options,
    hex: Option<HexContext<'_>>,
    guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
    mut backend: impl FnMut(&[u8]) -> Result<usize, WriteError>,
) -> Result<usize, Error> {
    let mut target = StreamTarget {
        memory,
        buffer: StreamBuffer::new(),
        backend: &mut backend,
    };
    match printf_render::render_target(&mut target, format, va, options, hex, guard) {
        Ok(count) => {
            target.buffer.finish(target.backend).map_err(write_error)?;
            Ok(count)
        }
        Err(error) => {
            target.buffer.discard();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    const BASE: u64 = 0x1000;
    const FMT: u64 = 0x1100;
    const VA: u64 = 0x2100;
    const REGS: u64 = 0x2200;
    const DATA: u64 = 0x2500;
    fn setup(format: &[u8], gp: &[u64], fp: &[u64]) -> Vec<u8> {
        let mut bytes = vec![0; 16384];
        bytes[0x100..0x100 + format.len()].copy_from_slice(format);
        bytes[0x1104..0x1108].copy_from_slice(&48u32.to_le_bytes());
        bytes[0x1108..0x1110].copy_from_slice(&(REGS + 176).to_le_bytes());
        bytes[0x1110..0x1118].copy_from_slice(&REGS.to_le_bytes());
        for (i, value) in gp.iter().enumerate() {
            bytes[0x1200 + i * 8..0x1208 + i * 8].copy_from_slice(&value.to_le_bytes());
        }
        for (i, value) in fp.iter().enumerate() {
            bytes[0x1230 + i * 16..0x1238 + i * 16].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }
    fn guard(_: &Memory<'_>, _: u64, _: usize) -> crate::Result<()> {
        Ok(())
    }
    #[test]
    fn count_is_logical_and_count_alias_changes_deferred_text_before_final_flush() {
        use std::cell::Cell;
        let mut data = setup(b"abc%hhn:%u%n\0", &[FMT, 12, DATA], &[]);
        let mut memory = Memory::default();
        memory.add(BASE, &mut data, true, false).unwrap();
        let calls = Cell::new(0);
        let mut output = Vec::new();
        let count = run(
            &mut memory,
            FMT,
            VA,
            Options::default(),
            None,
            |_, address, _| {
                if address == FMT || address == DATA {
                    assert_eq!(calls.get(), 0);
                }
                Ok(())
            },
            |bytes| {
                calls.set(calls.get() + 1);
                output.extend_from_slice(bytes);
                Ok(bytes.len())
            },
        )
        .unwrap();
        assert_eq!(count, 6);
        assert_eq!(output, b"\x03bc:12");
        assert_eq!(calls.get(), 1);
        assert_eq!(memory.load(DATA, 4).unwrap(), 6);
    }
    #[test]
    fn vector_boundaries_control_direct_write_vs_exact_buffer_fill() {
        use std::cell::Cell;
        for split in [false, true] {
            let mut data = setup(
                if split { b"%s%s%n" } else { b"%s%n" },
                if split {
                    &[DATA, DATA + 513, DATA + 2048]
                } else {
                    &[DATA, DATA + 2048]
                },
                &[],
            );
            data[0x1500..0x1500 + 1024].fill(b'x');
            if split {
                data[0x1500 + 512] = 0;
                data[0x1500 + 1025] = 0;
                data[0x1500 + 1024] = b'x';
            }
            let mut memory = Memory::default();
            memory.add(BASE, &mut data, true, false).unwrap();
            let calls = Cell::new(0);
            let mut lengths = Vec::new();
            let n = run(
                &mut memory,
                FMT,
                VA,
                Options::default(),
                None,
                |_, address, _| {
                    if address == DATA + 2048 {
                        assert_eq!(calls.get(), usize::from(!split));
                    }
                    Ok(())
                },
                |bytes| {
                    calls.set(calls.get() + 1);
                    lengths.push(bytes.len());
                    assert!(bytes.iter().all(|&b| b == b'x'));
                    Ok(bytes.len())
                },
            )
            .unwrap();
            assert_eq!(n, 1024);
            assert_eq!(lengths, [1024]);
        }
    }
    #[test]
    fn render_failure_discards_pending_tail_but_preserves_written_prefix() {
        let mut data = setup(b"%s%Q\0", &[DATA], &[]);
        data[0x1500..0x1500 + 1025].fill(b'x');
        let mut memory = Memory::default();
        memory.add(BASE, &mut data, true, false).unwrap();
        let mut output = Vec::new();
        let error = run(
            &mut memory,
            FMT,
            VA,
            Options::default(),
            None,
            guard,
            |bytes| {
                output.extend_from_slice(bytes);
                Ok(bytes.len())
            },
        )
        .unwrap_err();
        assert!(matches!(error.kind, Kind::Runtime(_)));
        assert_eq!(output, vec![b'x'; 1024]);
    }
    #[test]
    fn final_flush_short_writes_and_typed_failures_preserve_accepted_prefix() {
        for mode in 0..4 {
            let mut data = setup(b"abcdef\0", &[], &[]);
            let mut memory = Memory::default();
            memory.add(BASE, &mut data, true, false).unwrap();
            let mut output = Vec::new();
            let error = run(
                &mut memory,
                FMT,
                VA,
                Options::default(),
                None,
                guard,
                |bytes| {
                    if output.is_empty() {
                        output.extend_from_slice(&bytes[..2]);
                        return Ok(2);
                    }
                    assert_eq!(bytes, b"cdef");
                    match mode {
                        0 => Ok(0),
                        1 => Err(WriteError::Stream { errno: Some(5) }),
                        2 => Err(WriteError::Policy("capture budget".into())),
                        _ => Ok(bytes.len() + 1),
                    }
                },
            )
            .unwrap_err();
            assert_eq!(output, b"ab");
            match mode {
                0 => assert_eq!(error.kind, Kind::Stream(None)),
                1 => assert_eq!(error.kind, Kind::Stream(Some(5))),
                2 => assert_eq!(error.kind, Kind::Runtime("capture budget".into())),
                _ => assert!(matches!(error.kind, Kind::Runtime(_))),
            }
        }
    }
    #[test]
    fn hex_effects_and_temporary_cleanup_survive_backend_failure() {
        let mut data = setup(b"%ls%.4096a%n\0", &[DATA + 3000, DATA], &[3]);
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut memory = Memory::default();
        memory.add(BASE, &mut data, true, false).unwrap();
        memory
            .add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        memory.attach_owned(&mut owner).unwrap();
        memory.store(DATA, 99, 4).unwrap();
        memory.store(DATA + 3000, b'w' as u64, 2).unwrap();
        let mut output = Vec::new();
        let error = run(
            &mut memory,
            FMT,
            VA,
            Options::default(),
            Some(HexContext {
                state,
                retained: &mut retained,
                mxcsr: &mut control,
            }),
            guard,
            |bytes| {
                if output.is_empty() {
                    output.extend_from_slice(bytes);
                    Ok(bytes.len())
                } else {
                    Err(WriteError::Stream { errno: None })
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, Kind::Stream(None));
        assert_eq!(output.len(), 1024);
        assert_eq!(&output[..5], b"w0x1.");
        assert_eq!(memory.load(DATA, 4).unwrap(), 99); // later %n was never reached
        assert_eq!(control, 0x1f82); // source scaling raised DE before output
        assert!(retained.current().is_none());
        assert_eq!(state.owned_ranges().count(), 1); // heap-backed digit buffer was freed
        assert!(error.cleanup.is_none());
        assert!(error.output_cleanup.is_none());
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0); // both wide and gdtoa heap scratch released
    }
}
