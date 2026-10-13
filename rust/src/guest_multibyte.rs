/*-
 * Copyright 2013 Garrett D'Amore <garrett@damore.org>
 * Copyright 2010 Nexenta Systems, Inc.  All rights reserved.
 * Copyright (c) 2002-2004 Tim J. Robbins. All rights reserved.
 * Copyright (c) 1993
 *	The Regents of the University of California.  All rights reserved.
 *
 * This code is derived from software contributed to Berkeley by
 * Paul Borman at Krystal Technologies.
 *
 * Copyright (c) 2011 The FreeBSD Foundation
 * All rights reserved.
 * Portions of this software were developed by David Chisnall
 * under sponsorship from the FreeBSD Foundation.
 *
 * Redistribution and use in source and binary forms, with or without
 * modification, are permitted provided that the following conditions
 * are met:
 * 1. Redistributions of source code must retain the above copyright
 *    notice, this list of conditions and the following disclaimer.
 * 2. Redistributions in binary form must reproduce the above copyright
 *    notice, this list of conditions and the following disclaimer in the
 *    documentation and/or other materials provided with the distribution.
 * 4. Neither the name of the University nor the names of its contributors
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
//! Guest C/POSIX locale restartable conversion, using the FreeBSD `none` policy.
//! Policy source: https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/locale/none.c
//! Wrappers: same directory's mbrtowc.c, wcrtomb.c and mbsrtowcs.c.
//! ABI: SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a,
//! https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/_types.h
//! and include/freebsd/sys/_types.h: unsigned16 wchar_t, 128-byte/alignment8
//! mbstate_t. These stateless backends do not access supplied or implicit state.
//! No host locale, UTF-8, surrogate decoding, or measured Sony-libc claim.
//! Guard/budget faults reject the entire plan. Actual accessed restrict-qualified
//! input/output overlaps are explicit boundaries, not invented copy semantics.
use crate::{Result, guest_memory::Memory};
pub const UNIT_LIMIT: u64 = 65_536;
pub const EILSEQ: u32 = 86;
pub const INCOMPLETE: u64 = u64::MAX - 1;

#[derive(Debug)]
pub struct Plan {
    pub destination: u64,
    pub bytes: Vec<u8>,
    pub source_update: Option<(u64, u64)>,
    pub value: u64,
    pub errno: Option<u32>,
}
impl Plan {
    fn empty(destination: u64, value: u64) -> Self {
        Self {
            destination,
            bytes: Vec::new(),
            source_update: None,
            value,
            errno: None,
        }
    }
    /// Caller must guard every returned range against live return frames, and
    /// validate optional errno separately, before committing any bytes.
    pub fn write_ranges(&self) -> impl Iterator<Item = (u64, usize)> + '_ {
        (!self.bytes.is_empty())
            .then_some((self.destination, self.bytes.len()))
            .into_iter()
            .chain(self.source_update.map(|(address, _)| (address, 8)))
    }
    pub fn validate(&self, memory: &Memory<'_>) -> Result<()> {
        for (address, length) in self.write_ranges() {
            memory.validate_write(address, length)?;
        }
        Ok(())
    }
    /// Validate both output and source pointer before the first write. Given
    /// exclusive Memory access and unchanged mappings, subsequent writes cannot
    /// introduce a new bounds/protection failure.
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<()> {
        self.validate(memory)?;
        if !self.bytes.is_empty() {
            memory.write(self.destination, &self.bytes)?;
        }
        if let Some((address, value)) = self.source_update {
            memory.store(address, value, 8)?;
        }
        Ok(())
    }
}
fn reject_overlap(a: u64, alen: u64, b: u64, blen: u64) -> Result<()> {
    if alen == 0 || blen == 0 {
        return Ok(());
    }
    let ae = a.checked_add(alen).ok_or("Multibyte range overflow")?;
    let be = b.checked_add(blen).ok_or("Multibyte range overflow")?;
    if a < be && b < ae {
        return Err("Overlapping restrict-qualified multibyte storage unsupported".into());
    }
    Ok(())
}
/// `state` is intentionally ignored in the C locale, even on reset and errors.
pub fn mbrtowc(
    memory: &Memory<'_>,
    destination: u64,
    source: u64,
    length: u64,
    _state: u64,
) -> Result<Plan> {
    if source == 0 {
        return Ok(Plan::empty(destination, 0));
    }
    if length == 0 {
        return Ok(Plan::empty(destination, INCOMPLETE));
    }
    let byte = memory.load(source, 1)? as u8;
    let mut p = Plan::empty(destination, u64::from(byte != 0));
    if destination != 0 {
        reject_overlap(destination, 2, source, 1)?;
        p.bytes
            .try_reserve_exact(2)
            .map_err(|_| "Cannot allocate wide conversion output")?;
        p.bytes.extend(u16::from(byte).to_le_bytes());
    }
    Ok(p)
}
/// FreeBSD releng/11.4 lib/libc/locale/mbtowc.c maps an incomplete
/// restartable conversion to int -1 and EILSEQ. The C backend is stateless;
/// a future stateful locale must own a separate implicit mbtowc state.
pub fn mbtowc(memory: &Memory<'_>, destination: u64, source: u64, length: u64) -> Result<Plan> {
    let mut plan = mbrtowc(memory, destination, source, length, 0)?;
    if plan.value == INCOMPLETE {
        plan.value = u32::MAX as u64;
        plan.errno = Some(EILSEQ);
    } else {
        plan.value = plan.value as u32 as u64;
    }
    Ok(plan)
}
pub fn wcrtomb(destination: u64, wide: u64, _state: u64) -> Result<Plan> {
    let mut p = Plan::empty(destination, 1);
    if destination == 0 {
        return Ok(p);
    }
    let unit = wide as u16;
    if unit > 255 {
        p.value = u64::MAX;
        p.errno = Some(EILSEQ);
        return Ok(p);
    }
    p.bytes
        .try_reserve_exact(1)
        .map_err(|_| "Cannot allocate byte conversion output")?;
    p.bytes.push(unit as u8);
    Ok(p)
}
pub fn mbsrtowcs(
    memory: &Memory<'_>,
    destination: u64,
    source_slot: u64,
    length: u64,
    _state: u64,
) -> Result<Plan> {
    let source = memory.load(source_slot, 8)?;
    let mut p = Plan::empty(destination, 0);
    let mut consumed = 0;
    loop {
        if destination != 0 && consumed == length {
            p.value = consumed;
            p.source_update = Some((
                source_slot,
                source
                    .checked_add(consumed)
                    .ok_or("Multibyte source pointer overflow")?,
            ));
            break;
        }
        if consumed == UNIT_LIMIT {
            return Err("Multibyte source scan exceeds bounded unit limit".into());
        }
        let address = source
            .checked_add(consumed)
            .ok_or("Multibyte source address overflow")?;
        let byte = memory.load(address, 1)? as u8;
        consumed += 1;
        if destination != 0 {
            p.bytes
                .try_reserve(2)
                .map_err(|_| "Cannot allocate wide string output")?;
            p.bytes.extend(u16::from(byte).to_le_bytes());
        }
        if byte == 0 {
            p.value = consumed - 1;
            if destination != 0 {
                p.source_update = Some((source_slot, 0));
            }
            break;
        }
    }
    if destination != 0 {
        reject_overlap(destination, p.bytes.len() as u64, source, consumed)?;
        reject_overlap(destination, p.bytes.len() as u64, source_slot, 8)?;
        reject_overlap(source_slot, 8, source, consumed)?;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    #[test]
    fn legacy_conversion_reset_and_incomplete_are_distinct() {
        let memory = crate::guest_memory::Memory::default();
        let reset = super::mbtowc(&memory, u64::MAX, 0, 0).unwrap();
        assert_eq!((reset.value, reset.errno), (0, None));
        assert!(reset.bytes.is_empty());
        let incomplete = super::mbtowc(&memory, u64::MAX, u64::MAX, 0).unwrap();
        assert_eq!(
            (incomplete.value, incomplete.errno),
            (u32::MAX as u64, Some(86))
        );
        assert!(incomplete.bytes.is_empty());
        assert!(super::mbtowc(&memory, u64::MAX, u64::MAX, 1).is_err());
    }
    use super::*;
    const S: u64 = 0x1000;
    const D: u64 = 0x2000;
    const P: u64 = 0x3000;
    #[test]
    fn all_bytes_reset_zero_length_and_ignored_state() {
        let mut input: Vec<u8> = (0..=255).collect();
        let mut output = [0xa5; 4];
        let mut m = Memory::default();
        m.add(S, &mut input, false, false).unwrap();
        m.add(D, &mut output, true, false).unwrap();
        for byte in 0..=255u64 {
            let p = mbrtowc(&m, D, S + byte, u64::MAX, u64::MAX).unwrap();
            assert_eq!(p.value, u64::from(byte != 0));
            assert_eq!(p.errno, None);
            p.commit(&mut m).unwrap();
            assert_eq!(m.load(D, 2).unwrap(), byte);
            assert_eq!(m.load(D + 2, 2).unwrap(), 0xa5a5);
            let p = mbrtowc(&m, 0, S + byte, 1, u64::MAX).unwrap();
            assert!(p.bytes.is_empty());
            assert_eq!(p.value, u64::from(byte != 0));
        }
        assert_eq!(mbrtowc(&m, u64::MAX, 0, 0, u64::MAX).unwrap().value, 0);
        assert_eq!(
            mbrtowc(&m, u64::MAX, u64::MAX, 0, u64::MAX).unwrap().value,
            INCOMPLETE
        );
        assert!(mbrtowc(&m, D, u64::MAX, 1, 0).is_err());
    }
    #[test]
    fn wide_argument_is_low16_and_errno_precedes_destination() {
        for byte in 0..=255u64 {
            let p = wcrtomb(D, byte | 0xffff_0000, u64::MAX).unwrap();
            assert_eq!(p.bytes, [byte as u8]);
            assert_eq!(p.value, 1);
            assert_eq!(p.errno, None);
        }
        for wide in [256, 0xd800, 0xffff] {
            let p = wcrtomb(u64::MAX, wide, u64::MAX).unwrap();
            assert!(p.bytes.is_empty());
            assert_eq!(p.value, u64::MAX);
            assert_eq!(p.errno, Some(EILSEQ));
            assert_eq!(wcrtomb(0, wide, u64::MAX).unwrap().value, 1);
        }
        assert_eq!(wcrtomb(D, 0x1_0000, 0).unwrap().bytes, [0]);
    }
    #[test]
    fn string_count_pointer_terminator_and_full_width_lengths() {
        let mut input = [0x41, 0x80, 0xff, 0];
        let mut output = [0xa5; 10];
        let mut pointer = S.to_le_bytes();
        let mut m = Memory::default();
        m.add(S, &mut input, false, false).unwrap();
        m.add(D, &mut output, true, false).unwrap();
        m.add(P, &mut pointer, true, false).unwrap();
        for cap in [0, 1, 3, 4, u32::MAX as u64 + 1, u64::MAX] {
            m.store(P, S, 8).unwrap();
            let p = mbsrtowcs(&m, D, P, cap, u64::MAX).unwrap();
            assert_eq!(p.value, cap.min(3));
            p.commit(&mut m).unwrap();
            assert_eq!(m.load(P, 8).unwrap(), if cap > 3 { 0 } else { S + cap });
        }
        assert_eq!(m.read(D, 8).unwrap(), [0x41, 0, 0x80, 0, 0xff, 0, 0, 0]);
        assert_eq!(m.load(D + 8, 2).unwrap(), 0xa5a5);
        m.store(P, S, 8).unwrap();
        for cap in [0, u64::MAX] {
            let p = mbsrtowcs(&m, 0, P, cap, u64::MAX).unwrap();
            assert_eq!(p.value, 3);
            assert_eq!(p.source_update, None);
            p.commit(&mut m).unwrap();
            assert_eq!(m.load(P, 8).unwrap(), S);
        }
    }
    #[test]
    fn every_output_is_validated_before_any_write() {
        let mut input = [65, 0];
        let mut output = [0xa5; 4];
        let mut pointer = S.to_le_bytes();
        let mut m = Memory::default();
        m.add(S, &mut input, false, false).unwrap();
        m.add(D, &mut output, true, false).unwrap();
        m.add(P, &mut pointer, false, false).unwrap();
        let p = mbsrtowcs(&m, D, P, 2, 0).unwrap();
        assert!(p.commit(&mut m).is_err());
        assert_eq!(m.read(D, 4).unwrap(), [0xa5; 4]);
        assert_eq!(m.load(P, 8).unwrap(), S);
        let p = mbsrtowcs(&m, u64::MAX, P, 0, 0).unwrap();
        assert_eq!(p.value, 0);
        assert!(p.bytes.is_empty());
        assert!(p.commit(&mut m).is_err());
        let p = mbsrtowcs(&m, 0, P, 0, 0).unwrap();
        p.commit(&mut m).unwrap();
        assert_eq!(p.value, 1);
        let p = mbrtowc(&m, D + 3, S, 1, 0).unwrap();
        assert!(p.commit(&mut m).is_err());
        assert_eq!(m.read(D, 4).unwrap(), [0xa5; 4]);
    }
    #[test]
    fn bounds_and_restrict_overlaps_are_explicit_without_partial_updates() {
        let mut input = vec![65; UNIT_LIMIT as usize];
        let mut output = vec![0xa5; UNIT_LIMIT as usize * 2];
        let mut pointer = S.to_le_bytes();
        let mut m = Memory::default();
        m.add(S, &mut input, true, false).unwrap();
        m.add(0x20000, &mut output, true, false).unwrap();
        m.add(0x50000, &mut pointer, true, false).unwrap();
        let p = mbsrtowcs(&m, 0x20000, 0x50000, UNIT_LIMIT, 0).unwrap();
        assert_eq!(p.value, UNIT_LIMIT);
        assert_eq!(p.source_update, Some((0x50000, S + UNIT_LIMIT)));
        assert!(mbsrtowcs(&m, 0, 0x50000, 0, 0).is_err());
        assert!(mbsrtowcs(&m, 0x20000, 0x50000, UNIT_LIMIT + 1, 0).is_err());
        assert!(mbsrtowcs(&m, S, 0x50000, 2, 0).is_err());
        assert!(mbsrtowcs(&m, 0x50000, 0x50000, 1, 0).is_err());
        assert!(mbrtowc(&m, S, S, 1, 0).is_err());
        assert_eq!(m.load(0x50000, 8).unwrap(), S);
        assert_eq!(m.read(0x20000, 4).unwrap(), [0xa5; 4]);
    }
}
