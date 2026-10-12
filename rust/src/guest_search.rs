//! FreeBSD releng/11.4 libc/stdlib/bsearch.c midpoint/remaining-count traversal.
//! Array addresses are checked; only the guest comparator interprets key/records.
/*
 * Adapted to an explicit Rust callback state machine from FreeBSD releng/11.4
 * lib/libc/stdlib/bsearch.c:
 * https://github.com/freebsd/freebsd-src/blob/releng/11.4/lib/libc/stdlib/bsearch.c
 *
 * Copyright (c) 1990, 1993
 * The Regents of the University of California.  All rights reserved.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 3. Neither the name of the University nor the names of its contributors
 *    may be used to endorse or promote products derived from this software
 *    without specific prior written permission.
 *
 * THIS SOFTWARE IS PROVIDED BY THE REGENTS AND CONTRIBUTORS ``AS IS'' AND
 * ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
 * IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
 * ARE DISCLAIMED.  IN NO EVENT SHALL THE REGENTS OR CONTRIBUTORS BE LIABLE
 * FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
 * DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS
 * OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION)
 * HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT
 * LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY
 * OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF
 * SUCH DAMAGE.
 */
use crate::{
    Result,
    guest_compare::{self, Event, Gate, Kind, Request},
};
pub struct Search {
    gate: Gate,
    key: u64,
    base: u64,
    size: u64,
    remaining: u64,
    midpoint: u64,
    found: Option<u64>,
}
impl Search {
    pub fn new(key: u64, base: u64, count: u64, size: u64) -> Result<Self> {
        if count != 0 {
            guest_compare::extent(base, count, size)?;
        }
        Ok(Self {
            gate: Gate::new(),
            key,
            base,
            size,
            remaining: count,
            midpoint: 0,
            found: None,
        })
    }
    // Unlike an Iterator, progression requires acknowledging an external callback.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Event> {
        self.gate.idle()?;
        if let Some(found) = self.found {
            return Ok(Event::Done(found));
        }
        if self.remaining == 0 {
            return Ok(Event::Done(0));
        }
        self.midpoint = self.base + (self.remaining / 2) * self.size;
        Ok(Event::Compare {
            request: self.gate.issue(Kind::Compare)?,
            left: self.key,
            right: self.midpoint,
        })
    }
    pub fn compare(&mut self, request: Request, result: i32) -> Result<()> {
        self.gate.accept(request, Kind::Compare)?;
        if result == 0 {
            self.found = Some(self.midpoint);
        } else {
            if result > 0 {
                self.base = self.midpoint + self.size;
                self.remaining -= 1;
            }
            self.remaining /= 2;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_matches_independent_membership_and_terminates() {
        for count in 0..80u64 {
            for key in 0..90i64 {
                let values: Vec<i64> = (0..count).map(|i| (i / 3) as i64 * 2).collect();
                let mut search = Search::new(77, 0x1001, count, 9).unwrap();
                let mut calls = 0;
                loop {
                    match search.next().unwrap() {
                        Event::Compare {
                            request,
                            left,
                            right,
                        } => {
                            assert_eq!(left, 77);
                            calls += 1;
                            assert!(calls <= 8);
                            let value = values[((right - 0x1001) / 9) as usize];
                            search
                                .compare(
                                    request,
                                    if key < value {
                                        i32::MIN
                                    } else if key > value {
                                        i32::MAX
                                    } else {
                                        0
                                    },
                                )
                                .unwrap();
                        }
                        Event::Done(address) => {
                            assert_eq!(address != 0, values.contains(&key));
                            if address != 0 {
                                assert_eq!(values[((address - 0x1001) / 9) as usize], key);
                            }
                            break;
                        }
                        Event::Swap { .. } => panic!("search mutated array"),
                    }
                }
            }
        }
        assert!(matches!(
            Search::new(u64::MAX, u64::MAX, 0, 0)
                .unwrap()
                .next()
                .unwrap(),
            Event::Done(0)
        ));
        for (base, count, size) in [(u64::MAX, 1, 1), (0, u64::MAX, 2), (0, 1, 0)] {
            assert!(Search::new(0, base, count, size).is_err());
        }
    }
}
