//! Ordered printf output vectors: FreeBSD printfcommon NIOV=8, PADSIZE=16.
//! Guest sources are read by the sink at flush time, not when queued. This
//! preserves source visibility across intervening argument fetches and %n.
const NIOV: usize = 8;
const PADSIZE: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum Vector {
    Guest { address: u64, length: usize },
    Owned(Vec<u8>),
    Pad { byte: u8, length: usize },
}
impl Vector {
    pub fn len(&self) -> usize {
        match self {
            Self::Guest { length, .. } | Self::Pad { length, .. } => *length,
            Self::Owned(bytes) => bytes.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    Sink(E),
    Limit,
    Poisoned,
}

/// Borrowed source batch. Guest bytes remain unread until the target consumes it.
#[derive(Clone, Copy)]
pub struct Batch<'a> {
    vectors: &'a [Option<Vector>],
    residual: usize,
}
impl Batch<'_> {
    pub fn vectors(&self) -> impl Iterator<Item = &Vector> {
        self.vectors
            .iter()
            .map(|v| v.as_ref().expect("queued output vector"))
    }
    pub fn residual(&self) -> usize {
        self.residual
    }
    pub fn submit<E>(&self, mut sink: impl FnMut(&Vector) -> Result<(), E>) -> Result<(), E> {
        for vector in self.vectors().filter(|v| !v.is_empty()) {
            sink(vector)?;
        }
        Ok(())
    }
}

pub struct Output {
    vectors: [Option<Vector>; NIOV],
    queued: usize,
    total: usize,
    limit: usize,
    poisoned: bool,
}
impl Output {
    pub fn new(limit: usize) -> Self {
        Self {
            vectors: std::array::from_fn(|_| None),
            queued: 0,
            total: 0,
            limit,
            poisoned: false,
        }
    }
    /// Logical bytes queued, including vectors already flushed successfully.
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn print_batch<E>(
        &mut self,
        vector: Vector,
        mut sink: impl FnMut(Batch<'_>) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let Some(total) = self
            .total
            .checked_add(vector.len())
            .filter(|n| *n <= self.limit)
        else {
            self.poisoned = true;
            return Err(Error::Limit);
        };
        self.total = total;
        self.vectors[self.queued] = Some(vector);
        self.queued += 1;
        if self.queued == NIOV {
            self.flush_batch(&mut sink)?;
        }
        Ok(())
    }
    pub fn pad_batch<E>(
        &mut self,
        byte: u8,
        count: usize,
        mut sink: impl FnMut(Batch<'_>) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let mut remaining = count;
        while remaining != 0 {
            let length = remaining.min(PADSIZE);
            self.print_batch(Vector::Pad { byte, length }, &mut sink)?;
            remaining -= length;
        }
        Ok(())
    }
    pub fn print<E>(
        &mut self,
        vector: Vector,
        mut sink: impl FnMut(&Vector) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        self.print_batch(vector, |batch| batch.submit(&mut sink))
    }
    pub fn pad<E>(
        &mut self,
        byte: u8,
        count: usize,
        mut sink: impl FnMut(&Vector) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        self.pad_batch(byte, count, |batch| batch.submit(&mut sink))
    }
    pub fn flush_batch<E>(
        &mut self,
        mut sink: impl FnMut(Batch<'_>) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let residual = self.vectors[..self.queued]
            .iter()
            .map(|v| v.as_ref().expect("queued output vector").len())
            .sum();
        if residual != 0 {
            if let Err(error) = sink(Batch {
                vectors: &self.vectors[..self.queued],
                residual,
            }) {
                self.poisoned = true;
                return Err(Error::Sink(error));
            }
        }
        for vector in &mut self.vectors[..self.queued] {
            *vector = None;
        }
        self.queued = 0;
        Ok(())
    }
    pub fn flush<E>(
        &mut self,
        mut sink: impl FnMut(&Vector) -> Result<(), E>,
    ) -> Result<(), Error<E>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        for index in 0..self.queued {
            let vector = self.vectors[index].as_ref().expect("queued output vector");
            // __sprint clears vectors without calling __sfvwrite if residual
            // bytes are zero. Zero-length vectors never require guest access.
            let result = if vector.is_empty() {
                Ok(())
            } else {
                sink(vector)
            };
            if let Err(error) = result {
                self.poisoned = true;
                return Err(Error::Sink(error));
            }
            self.vectors[index] = None;
        }
        self.queued = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_boundary_preserves_vectors_residual_and_deferred_guest_source() {
        let mut output = Output::new(100);
        let mut guest = *b"a";
        let mut traces = Vec::new();
        for _ in 0..7 {
            output
                .print_batch(
                    Vector::Guest {
                        address: 0,
                        length: 1,
                    },
                    |_| -> Result<(), ()> { panic!("early batch") },
                )
                .unwrap();
        }
        guest[0] = b'z';
        output
            .pad_batch(b' ', 33, |batch| {
                traces.push((
                    batch.residual(),
                    batch.vectors().map(Vector::len).collect::<Vec<_>>(),
                ));
                let mut bytes = Vec::new();
                batch.submit(|v| {
                    if let Vector::Guest { address, length } = v {
                        bytes.extend_from_slice(
                            &guest[*address as usize..*address as usize + length],
                        );
                    }
                    Ok::<_, ()>(())
                })?;
                assert_eq!(bytes, b"zzzzzzz");
                Ok::<_, ()>(())
            })
            .unwrap();
        output
            .flush_batch(|batch| {
                traces.push((batch.residual(), batch.vectors().map(Vector::len).collect()));
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(
            traces,
            [(23, vec![1, 1, 1, 1, 1, 1, 1, 16]), (17, vec![16, 1])]
        );
    }
    #[test]
    fn zero_residual_batches_skip_submission_and_partial_failure_never_replays() {
        let mut output = Output::new(100);
        for _ in 0..8 {
            output
                .print_batch(
                    Vector::Guest {
                        address: u64::MAX,
                        length: 0,
                    },
                    |_| -> Result<(), ()> { panic!("empty residual") },
                )
                .unwrap();
        }
        for byte in 0..3 {
            output
                .print_batch(Vector::Owned(vec![byte]), |_| Ok::<_, i32>(()))
                .unwrap();
        }
        let mut observed = Vec::new();
        assert_eq!(
            output.flush_batch(|batch| batch.submit(|vector| {
                let Vector::Owned(bytes) = vector else {
                    unreachable!()
                };
                if bytes[0] == 1 {
                    return Err(7);
                }
                observed.push(bytes[0]);
                Ok(())
            })),
            Err(Error::Sink(7))
        );
        assert_eq!(observed, [0]);
        assert_eq!(
            output.flush_batch(|_| panic!("replay")),
            Err::<(), Error<i32>>(Error::Poisoned)
        );
        assert_eq!(output.total(), 3);
    }
    #[test]
    fn eighth_vector_flushes_and_padding_keeps_source_chunk_sizes() {
        let mut output = Output::new(1024);
        let mut lengths = Vec::new();
        for _ in 0..7 {
            output
                .print(Vector::Owned(vec![1]), |v| {
                    lengths.push(v.len());
                    Ok::<_, ()>(())
                })
                .unwrap();
        }
        assert!(lengths.is_empty());
        output
            .pad(b' ', 33, |v| {
                lengths.push(v.len());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(lengths, [1, 1, 1, 1, 1, 1, 1, 16]);
        output
            .flush(|v| {
                lengths.push(v.len());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(&lengths[8..], &[16, 1]);
        assert_eq!(output.total(), 40);
    }
    #[test]
    fn guest_source_is_observed_at_flush_time() {
        let mut output = Output::new(8);
        output
            .print(
                Vector::Guest {
                    address: 1,
                    length: 1,
                },
                |_| panic!("premature read"),
            )
            .unwrap_or_else(|_: Error<()>| unreachable!());
        let mut guest = [0, b'a'];
        guest[1] = b'z'; // An intervening guest-visible write.
        let mut result = Vec::new();
        output
            .flush(|vector| {
                let Vector::Guest { address, length } = vector else {
                    panic!("guest vector")
                };
                result.extend_from_slice(&guest[*address as usize..*address as usize + length]);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(result, b"z");
    }
    #[test]
    fn failed_flush_never_replays_already_written_vectors() {
        let mut output = Output::new(8);
        for n in 0..3 {
            output
                .print(Vector::Owned(vec![n]), |_| Ok::<_, ()>(()))
                .unwrap();
        }
        let mut calls = 0;
        assert_eq!(
            output.flush(|_| {
                calls += 1;
                if calls == 2 { Err(7) } else { Ok(()) }
            }),
            Err(Error::Sink(7))
        );
        assert_eq!(
            output.flush(|_| {
                calls += 1;
                Ok::<_, i32>(())
            }),
            Err(Error::Poisoned)
        );
        assert_eq!(calls, 2);
        assert_eq!(
            output.print(Vector::Owned(vec![4]), |_| Ok::<_, i32>(())),
            Err(Error::Poisoned)
        );
    }
    #[test]
    fn zero_vectors_need_no_sink_access_and_limit_cannot_be_retried() {
        let mut output = Output::new(0);
        for _ in 0..8 {
            output
                .print(
                    Vector::Guest {
                        address: u64::MAX,
                        length: 0,
                    },
                    |_| panic!("zero read"),
                )
                .unwrap_or_else(|_: Error<()>| unreachable!());
        }
        assert_eq!(
            output.print(Vector::Owned(vec![1]), |_| Ok::<_, ()>(())),
            Err(Error::Limit)
        );
        assert_eq!(output.flush(|_| Ok::<_, ()>(())), Err(Error::Poisoned));
    }
}
