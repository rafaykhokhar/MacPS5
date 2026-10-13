//! Fixed temporary buffer for FreeBSD11.4 __sbprintf's fully buffered fake FILE.
//! This is not ordinary stdout buffering, guest FILE state, or a formatter.
//! vfprintf.c __sbprintf allocates BUFSIZ (1024 for the pinned SDK) on its stack.
//! fvwrite.c126–143: fill-and-flush only when incoming length EXCEEDS remaining
//! room; an exact fill stays buffered. Empty buffers send >=BUFSIZ directly.
//! fflush.c118–130 retries only the unaccepted suffix after a short write.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/fvwrite.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/fflush.c
//!
//! Call write_vector once per ordered source vector, retaining its boundary.
//! No re-batching, guest allocation, eager read of future vectors, or Drop flush.
//! Callback Ok(n) must mean exactly that prefix was committed; Err(e) must mean
//! no bytes were committed by that particular callback. Successful prior writes
//! remain visible after any later failure. Invalid callback counts poison this
//! model; they cannot undo an incorrectly implemented backend's effects.
pub const CAPACITY: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    Finished,
    Failed,
    Discarded,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Backend(E),
    ZeroWrite,
    InvalidCount,
    Closed(State),
    CountOverflow,
}
pub struct StreamBuffer {
    bytes: [u8; CAPACITY],
    used: usize,
    accepted: u64,
    state: State,
}
impl Default for StreamBuffer {
    fn default() -> Self {
        Self::new()
    }
}
impl StreamBuffer {
    pub fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            used: 0,
            accepted: 0,
            state: State::Open,
        }
    }
    pub fn pending_len(&self) -> usize {
        self.used
    }
    pub fn accepted(&self) -> u64 {
        self.accepted
    }
    pub fn state(&self) -> State {
        self.state
    }
    fn open<E>(&self) -> Result<(), Error<E>> {
        if self.state == State::Open {
            Ok(())
        } else {
            Err(Error::Closed(self.state))
        }
    }
    fn send<E>(
        accepted: &mut u64,
        bytes: &[u8],
        write: &mut impl FnMut(&[u8]) -> Result<usize, E>,
    ) -> Result<usize, Error<E>> {
        // Check the maximum possible contribution before an irreversible call.
        accepted
            .checked_add(bytes.len() as u64)
            .ok_or(Error::CountOverflow)?;
        let n = write(bytes).map_err(Error::Backend)?;
        if n == 0 {
            return Err(Error::ZeroWrite);
        }
        if n > bytes.len() {
            return Err(Error::InvalidCount);
        }
        *accepted += n as u64;
        Ok(n)
    }
    fn flush<E>(
        &mut self,
        write: &mut impl FnMut(&[u8]) -> Result<usize, E>,
    ) -> Result<(), Error<E>> {
        let count = self.used;
        self.used = 0;
        let mut offset = 0;
        while offset < count {
            offset += Self::send(&mut self.accepted, &self.bytes[offset..count], write)?;
        }
        Ok(())
    }
    fn vector<E>(
        &mut self,
        mut bytes: &[u8],
        write: &mut impl FnMut(&[u8]) -> Result<usize, E>,
    ) -> Result<(), Error<E>> {
        while !bytes.is_empty() {
            let room = CAPACITY - self.used;
            if self.used != 0 && bytes.len() > room {
                self.bytes[self.used..].copy_from_slice(&bytes[..room]);
                self.used = CAPACITY;
                self.flush(write)?;
                bytes = &bytes[room..];
            } else if bytes.len() >= CAPACITY {
                // Here the buffer is empty (a nonempty full/exceeded buffer
                // took the preceding branch). Advance only accepted bytes;
                // the next iteration may buffer a sub-BUFSIZ remainder.
                let n = Self::send(&mut self.accepted, &bytes[..CAPACITY], write)?;
                bytes = &bytes[n..];
            } else {
                self.bytes[self.used..self.used + bytes.len()].copy_from_slice(bytes);
                self.used += bytes.len();
                break;
            }
        }
        Ok(())
    }
    pub fn write_vector<E>(
        &mut self,
        bytes: &[u8],
        write: &mut impl FnMut(&[u8]) -> Result<usize, E>,
    ) -> Result<(), Error<E>> {
        self.open()?;
        if let Err(e) = self.vector(bytes, write) {
            self.used = 0;
            self.state = State::Failed;
            return Err(e);
        }
        Ok(())
    }
    /// Only after rendering succeeded. FreeBSD __sbprintf skips its final flush
    /// when __vfprintf returns a rendering error. Successful finish is single-use.
    pub fn finish<E>(
        &mut self,
        write: &mut impl FnMut(&[u8]) -> Result<usize, E>,
    ) -> Result<(), Error<E>> {
        self.open()?;
        if let Err(e) = self.flush(write) {
            self.state = State::Failed;
            return Err(e);
        }
        self.state = State::Finished;
        Ok(())
    }
    /// Rendering failed: pending local bytes die with the fake FILE. Never
    /// flush them, and never erase the count of already accepted backend bytes.
    pub fn discard(&mut self) {
        self.used = 0;
        if self.state == State::Open {
            self.state = State::Discarded;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn payload(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 29 + i / 251) % 256) as u8).collect()
    }
    #[test]
    fn empty_and_single_vector_boundary_traces() {
        for (n, before, after, pending) in [
            (0, vec![], vec![], 0),
            (1, vec![], vec![1], 1),
            (1023, vec![], vec![1023], 1023),
            (1024, vec![1024], vec![1024], 0),
            (1025, vec![1024], vec![1024, 1], 1),
            (2048, vec![1024, 1024], vec![1024, 1024], 0),
        ] {
            let data = payload(n);
            let mut model = StreamBuffer::new();
            let mut trace = Vec::new();
            let mut output = Vec::new();
            model
                .write_vector(&data, &mut |b: &[u8]| {
                    trace.push(b.len());
                    output.extend_from_slice(b);
                    Ok::<_, ()>(b.len())
                })
                .unwrap();
            assert_eq!(trace, before);
            assert_eq!(model.pending_len(), pending);
            model
                .finish(&mut |b: &[u8]| {
                    trace.push(b.len());
                    output.extend_from_slice(b);
                    Ok::<_, ()>(b.len())
                })
                .unwrap();
            assert_eq!(trace, after);
            assert_eq!(output, data);
            assert_eq!(model.accepted(), n as u64);
            assert_eq!(model.state(), State::Finished);
        }
    }
    #[test]
    fn partial_exact_fill_waits_until_next_nonempty_vector() {
        for initial in [1, 17, 512, 1023] {
            let mut model = StreamBuffer::new();
            let mut trace = Vec::new();
            let mut output = Vec::new();
            let data = payload(2049);
            let mut send = |b: &[u8]| {
                trace.push(b.len());
                output.extend_from_slice(b);
                Ok::<_, ()>(b.len())
            };
            model.write_vector(&data[..initial], &mut send).unwrap();
            model.write_vector(&data[initial..1024], &mut send).unwrap();
            model.write_vector(&[], &mut send).unwrap();
            assert_eq!(model.accepted(), 0);
            assert_eq!(model.pending_len(), 1024);
            model.write_vector(&data[1024..], &mut send).unwrap();
            model.finish(&mut send).unwrap();
            assert_eq!(trace, vec![1024, 1024, 1]);
            assert_eq!(output, data);
        }
    }
    #[test]
    fn partial_exceed_and_ordered_vector_boundaries() {
        for initial in [1, 512, 1023] {
            for extra in [1, 1024, 1025, 2048] {
                let data = payload(1024 + extra);
                let mut m = StreamBuffer::new();
                let mut trace = Vec::new();
                let mut output = Vec::new();
                let mut send = |b: &[u8]| {
                    trace.push(b.len());
                    output.extend_from_slice(b);
                    Ok::<_, ()>(b.len())
                };
                m.write_vector(&data[..initial], &mut send).unwrap();
                m.write_vector(&data[initial..], &mut send).unwrap();
                m.finish(&mut send).unwrap();
                let mut expected = vec![1024];
                expected.extend(std::iter::repeat_n(1024, extra / 1024));
                if extra % 1024 != 0 {
                    expected.push(extra % 1024);
                }
                assert_eq!(trace, expected);
                assert_eq!(output, data);
            }
        }
        let mut m = StreamBuffer::new();
        let mut trace = Vec::new();
        let mut send = |b: &[u8]| {
            trace.push(b.len());
            Ok::<_, ()>(b.len())
        };
        m.write_vector(&[1; 512], &mut send).unwrap();
        m.write_vector(&[2; 512], &mut send).unwrap();
        assert_eq!(m.accepted(), 0);
        m.finish(&mut send).unwrap();
        assert_eq!(trace, [1024]);
    }
    #[test]
    fn every_short_direct_write_buffers_only_unaccepted_suffix() {
        let data = payload(1024);
        for short in 1..1024 {
            let mut m = StreamBuffer::new();
            let mut output = Vec::new();
            m.write_vector(&data, &mut |b: &[u8]| {
                assert_eq!(b, &data);
                output.extend_from_slice(&b[..short]);
                Ok::<_, ()>(short)
            })
            .unwrap();
            assert_eq!(m.accepted(), short as u64);
            assert_eq!(m.pending_len(), 1024 - short);
            m.finish(&mut |b: &[u8]| {
                assert_eq!(b, &data[short..]);
                output.extend_from_slice(b);
                Ok::<_, ()>(b.len())
            })
            .unwrap();
            assert_eq!(output, data);
        }
    }
    #[test]
    fn every_short_flush_retries_exact_suffix() {
        let data = payload(1023);
        for short in 1..1023 {
            let mut m = StreamBuffer::new();
            m.write_vector(&data, &mut |_| panic!("buffered"))
                .unwrap_or_else(|_: Error<()>| panic!());
            let mut first = true;
            let mut output = Vec::new();
            m.finish(&mut |b: &[u8]| {
                let n = if first {
                    first = false;
                    assert_eq!(b, &data);
                    short
                } else {
                    assert_eq!(b, &data[short..]);
                    b.len()
                };
                output.extend_from_slice(&b[..n]);
                Ok::<_, ()>(n)
            })
            .unwrap();
            assert_eq!(output, data);
        }
    }
    #[test]
    fn errors_at_every_byte_retain_prefix_and_never_retry() {
        let data = payload(2049);
        for failure in 0..data.len() {
            for zero in [false, true] {
                let mut m = StreamBuffer::new();
                let mut output = Vec::new();
                let mut send = |b: &[u8]| {
                    if output.len() == failure {
                        if zero { Ok(0) } else { Err(7) }
                    } else {
                        assert_eq!(b[0], data[output.len()]);
                        output.push(b[0]);
                        Ok(1)
                    }
                };
                let result = m
                    .write_vector(&data, &mut send)
                    .and_then(|()| m.finish(&mut send));
                assert_eq!(
                    result,
                    Err(if zero {
                        Error::ZeroWrite
                    } else {
                        Error::Backend(7)
                    })
                );
                assert_eq!(output, &data[..failure]);
                assert_eq!(m.accepted(), failure as u64);
                assert_eq!(m.pending_len(), 0);
                assert_eq!(m.state(), State::Failed);
                assert!(matches!(
                    m.finish(&mut |_| panic!("must not retry")),
                    Err(Error::<()>::Closed(State::Failed))
                ));
                m.discard();
                assert_eq!(m.accepted(), failure as u64);
            }
        }
    }
    #[test]
    fn rendering_failure_discards_pending_without_flushing_prefix() {
        let data = payload(1025);
        let mut m = StreamBuffer::new();
        let mut output = Vec::new();
        m.write_vector(&data, &mut |b: &[u8]| {
            output.extend_from_slice(b);
            Ok::<_, ()>(b.len())
        })
        .unwrap();
        m.discard();
        assert_eq!(output, &data[..1024]);
        assert_eq!(m.accepted(), 1024);
        assert_eq!(m.pending_len(), 0);
        assert!(matches!(
            m.finish(&mut |_| panic!("discarded")),
            Err(Error::<()>::Closed(State::Discarded))
        ));
    }
    #[test]
    fn invalid_backend_and_count_overflow_poison_without_more_calls() {
        let mut m = StreamBuffer::new();
        assert_eq!(
            m.write_vector(&[0; 1024], &mut |_| Ok::<_, ()>(1025)),
            Err(Error::InvalidCount)
        );
        assert_eq!(m.accepted(), 0);
        assert_eq!(m.state(), State::Failed);
        let mut m = StreamBuffer::new();
        m.accepted = u64::MAX - 1023;
        assert_eq!(
            m.write_vector(&[0; 1024], &mut |_| panic!("overflow preflight")),
            Err(Error::<()>::CountOverflow)
        );
        assert_eq!(m.state(), State::Failed);
    }
}
