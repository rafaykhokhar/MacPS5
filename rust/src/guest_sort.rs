//! In-place max-heapsort with explicit guest comparison and swap handoffs.
//! qsort does not specify comparator order; this is not FreeBSD's quicksort order.
//! Guest comparisons receive actual array records, never host/scratch pointers.
//! Memory guards, all live return-frame protection, and atomic pair swaps belong
//! to the runtime. Do not acknowledge a swap until its bytes have been committed.
use crate::{
    Result,
    guest_compare::{self, Event, Gate, Kind, Request},
};
#[derive(Clone, Copy)]
enum Resume {
    Build(u64),
    Extract(u64),
}
#[derive(Clone, Copy)]
enum Phase {
    Build(u64),
    Extract(u64),
    Sift {
        root: u64,
        end: u64,
        resume: Resume,
    },
    Children {
        root: u64,
        child: u64,
        end: u64,
        resume: Resume,
    },
    Parent {
        root: u64,
        child: u64,
        end: u64,
        resume: Resume,
    },
    SwapSift {
        root: u64,
        child: u64,
        end: u64,
        resume: Resume,
    },
    SwapExtract {
        end: u64,
    },
    Done,
}
pub struct Sort {
    gate: Gate,
    base: u64,
    size: u64,
    count: u64,
    phase: Phase,
}
impl Sort {
    pub fn new(base: u64, count: u64, size: u64) -> Result<Self> {
        if count > 1 {
            guest_compare::extent(base, count, size)?;
        }
        Ok(Self {
            gate: Gate::new(),
            base,
            size,
            count,
            phase: if count <= 1 {
                Phase::Done
            } else {
                Phase::Build(count / 2)
            },
        })
    }
    fn address(&self, index: u64) -> u64 {
        self.base + index * self.size
    }
    fn resume(&mut self, resume: Resume) {
        self.phase = match resume {
            Resume::Build(n) => Phase::Build(n),
            Resume::Extract(n) => Phase::Extract(n),
        };
    }
    // Unlike an Iterator, progression requires acknowledging external guest effects.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Event> {
        self.gate.idle()?;
        loop {
            match self.phase {
                Phase::Done => return Ok(Event::Done(0)),
                Phase::Build(0) => self.phase = Phase::Extract(self.count),
                Phase::Build(next) => {
                    self.phase = Phase::Sift {
                        root: next - 1,
                        end: self.count,
                        resume: Resume::Build(next - 1),
                    }
                }
                Phase::Extract(end) if end <= 1 => self.phase = Phase::Done,
                Phase::Extract(end) => self.phase = Phase::SwapExtract { end },
                Phase::Sift { root, end, resume } => {
                    let child = root * 2 + 1;
                    if child >= end {
                        self.resume(resume);
                    } else if child + 1 < end {
                        self.phase = Phase::Children {
                            root,
                            child,
                            end,
                            resume,
                        };
                    } else {
                        self.phase = Phase::Parent {
                            root,
                            child,
                            end,
                            resume,
                        };
                    }
                }
                Phase::Children { child, .. } => {
                    return Ok(Event::Compare {
                        request: self.gate.issue(Kind::Compare)?,
                        left: self.address(child),
                        right: self.address(child + 1),
                    });
                }
                Phase::Parent { root, child, .. } => {
                    return Ok(Event::Compare {
                        request: self.gate.issue(Kind::Compare)?,
                        left: self.address(root),
                        right: self.address(child),
                    });
                }
                Phase::SwapSift { root, child, .. } => {
                    return Ok(Event::Swap {
                        request: self.gate.issue(Kind::Swap)?,
                        left: self.address(root),
                        right: self.address(child),
                        size: self.size,
                    });
                }
                Phase::SwapExtract { end } => {
                    return Ok(Event::Swap {
                        request: self.gate.issue(Kind::Swap)?,
                        left: self.base,
                        right: self.address(end - 1),
                        size: self.size,
                    });
                }
            }
        }
    }
    pub fn compare(&mut self, request: Request, result: i32) -> Result<()> {
        self.gate.validate(&request, Kind::Compare)?;
        let next = match self.phase {
            Phase::Children {
                root,
                child,
                end,
                resume,
            } => Phase::Parent {
                root,
                child: child + u64::from(result < 0),
                end,
                resume,
            },
            Phase::Parent {
                root,
                child,
                end,
                resume,
            } => {
                if result < 0 {
                    Phase::SwapSift {
                        root,
                        child,
                        end,
                        resume,
                    }
                } else {
                    match resume {
                        Resume::Build(n) => Phase::Build(n),
                        Resume::Extract(n) => Phase::Extract(n),
                    }
                }
            }
            _ => return Err("Comparator result without comparison phase".into()),
        };
        self.gate.accept(request, Kind::Compare)?;
        self.phase = next;
        Ok(())
    }
    /// Validate BEFORE mutating guest bytes. Holding exclusive access to this
    /// engine and making no intervening call makes the following acknowledgment
    /// infallible; guards/scratch allocation must complete before either write.
    pub fn validate_swap(&self, request: &Request) -> Result<()> {
        self.gate.validate(request, Kind::Swap)?;
        if !matches!(
            self.phase,
            Phase::SwapSift { .. } | Phase::SwapExtract { .. }
        ) {
            return Err("Swap outside swap phase".into());
        }
        Ok(())
    }
    pub fn swapped(&mut self, request: Request) -> Result<()> {
        self.validate_swap(&request)?;
        let next = match self.phase {
            Phase::SwapSift {
                child, end, resume, ..
            } => Phase::Sift {
                root: child,
                end,
                resume,
            },
            Phase::SwapExtract { end } => Phase::Sift {
                root: 0,
                end: end - 1,
                resume: Resume::Extract(end - 1),
            },
            _ => unreachable!("validated swap phase"),
        };
        self.gate.accept(request, Kind::Swap)?;
        self.phase = next;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn run(mut rows: Vec<Vec<u8>>, bad: Option<i32>) -> Vec<Vec<u8>> {
        let count = rows.len();
        let size = rows.first().map_or(3, Vec::len) as u64;
        let mut sort = Sort::new(0x1_0000_0001, count as u64, size).unwrap();
        let mut events = 0;
        loop {
            match sort.next().unwrap() {
                Event::Compare {
                    request,
                    left,
                    right,
                } => {
                    events += 1;
                    let a = ((left - 0x1_0000_0001) / size) as usize;
                    let b = ((right - 0x1_0000_0001) / size) as usize;
                    let cmp = bad.unwrap_or_else(|| match rows[a].cmp(&rows[b]) {
                        std::cmp::Ordering::Less => i32::MIN,
                        std::cmp::Ordering::Equal => 0,
                        std::cmp::Ordering::Greater => i32::MAX,
                    });
                    assert!(sort.next().is_err());
                    sort.compare(request, cmp).unwrap();
                }
                Event::Swap {
                    request,
                    left,
                    right,
                    size: actual,
                } => {
                    events += 1;
                    assert_eq!(actual, size);
                    sort.validate_swap(&request).unwrap();
                    assert!(sort.next().is_err()); // failed/pending handoff never reissues comparison
                    rows.swap(
                        ((left - 0x1_0000_0001) / size) as usize,
                        ((right - 0x1_0000_0001) / size) as usize,
                    );
                    sort.swapped(request).unwrap();
                }
                Event::Done(_) => break,
            }
            assert!(events <= 128 * (count + 1));
        }
        rows
    }
    #[test]
    fn independent_order_and_permutation() {
        let mut seed = 0x123456789abcdefu64;
        for count in 0..130 {
            for size in [1, 3, 8, 31] {
                let rows: Vec<Vec<u8>> = (0..count)
                    .map(|_| {
                        (0..size)
                            .map(|_| {
                                seed ^= seed << 13;
                                seed ^= seed >> 7;
                                seed ^= seed << 17;
                                (seed % 17) as u8
                            })
                            .collect()
                    })
                    .collect();
                let mut expected = rows.clone();
                expected.sort();
                assert_eq!(run(rows.clone(), None), expected);
                expected.reverse();
                let mut ascending = expected.clone();
                ascending.reverse();
                assert_eq!(run(expected, None), ascending);
                for bad in [i32::MIN, 0, i32::MAX] {
                    let mut actual = run(rows.clone(), Some(bad));
                    actual.sort();
                    assert_eq!(actual, ascending);
                }
            }
        }
    }
    #[test]
    fn checked_extents_and_trivial_no_access() {
        for count in [0, 1] {
            assert!(matches!(
                Sort::new(u64::MAX, count, u64::MAX)
                    .unwrap()
                    .next()
                    .unwrap(),
                Event::Done(0)
            ));
        }
        for (base, n, size) in [
            (u64::MAX, 2, 1),
            (0, u64::MAX, 2),
            (0, 2, 0),
            (0, 2, 65537),
            (0, 1_048_577, 1),
            (0, 2048, 65536),
        ] {
            assert!(Sort::new(base, n, size).is_err());
        }
        assert!(Sort::new(u64::MAX - 6, 2, 3).is_ok());
    }
    #[test]
    fn rejected_swap_guard_keeps_same_request_without_comparison_replay() {
        let base = 0x1_0000_0001;
        let mut sort = Sort::new(base, 2, 3).unwrap();
        let Event::Compare {
            request,
            left,
            right,
        } = sort.next().unwrap()
        else {
            panic!("expected heap comparison");
        };
        assert_eq!((left, right), (base, base + 3));
        sort.compare(request, i32::MIN).unwrap();
        let Event::Swap {
            request,
            left,
            right,
            size,
        } = sort.next().unwrap()
        else {
            panic!("comparison must produce pending swap");
        };
        assert_ne!(left, right);
        assert_eq!(size, 3);
        for address in [left, right] {
            assert!(address >= base && address + size <= base + 6);
        }
        let mut bytes = [1, 2, 3, 4, 5, 6];
        sort.validate_swap(&request).unwrap();
        // A runtime rejects its output guard here. It retains this opaque
        // request and both original elements; the engine cannot reissue it.
        assert!(sort.next().is_err());
        assert_eq!(bytes, [1, 2, 3, 4, 5, 6]);
        sort.validate_swap(&request).unwrap();
        let original = bytes;
        bytes[..3].copy_from_slice(&original[3..]);
        bytes[3..].copy_from_slice(&original[..3]);
        sort.swapped(request).unwrap();
        assert_eq!(bytes, [4, 5, 6, 1, 2, 3]);
        // Heap construction has completed; extraction is a new swap event,
        // not another execution of the comparator whose result was consumed.
        assert!(matches!(sort.next().unwrap(), Event::Swap { .. }));
    }
    #[test]
    fn deep_heap_and_maximum_record_size() {
        let rows: Vec<Vec<u8>> = (0..4097u32)
            .rev()
            .map(|n| n.to_be_bytes().to_vec())
            .collect();
        let mut expected = rows.clone();
        expected.sort();
        assert_eq!(run(rows, None), expected);
        let rows = vec![vec![2; 65_536], vec![1; 65_536]];
        assert_eq!(run(rows, None), vec![vec![1; 65_536], vec![2; 65_536]]);
    }
}
