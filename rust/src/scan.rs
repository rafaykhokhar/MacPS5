//! Bounded C-locale scanf planning, based on FreeBSD releng/11.4 vfscanf.c.
//! Integer fields use its512-byte cap and intmax conversion then destination
//! narrowing. Binary64 floating fields use source-ordered gdtoa effects;
//! binary32, long-double, wide and positional assignments remain unsupported.
//! Input/format/va-list aliasing with outputs is rejected by the guest planner;
//! committing a snapshot in those cases would change sequential scanf behavior.
use crate::{guest_memory::Memory, Result};
pub const LIMIT: usize = 65_536;
const ARG_LIMIT: usize = 256;
#[derive(Debug)]
pub struct Write {
    pub address: u64,
    pub bytes: Vec<u8>,
}
#[derive(Debug)]
pub struct Plan {
    pub result: i32,
    pub writes: Vec<Write>,
    pub errno: Option<u32>,
    /// Newly raised sticky SSE flags; commit only after all output guards.
    pub mxcsr_flags: u32,
    /// Guest planner's input/format/va-list/argument storage. An aggregated
    /// errno write must not alias these ranges or any planned write.
    pub protected_ranges: Vec<(u64, usize)>,
}
fn space(c: u8) -> bool {
    matches!(c, 9..=13 | 32)
}
fn scanset(format: &[u8], at: &mut usize) -> Result<[bool; 256]> {
    let invert = format.get(*at) == Some(&b'^');
    if invert {
        *at += 1;
    }
    let mut set = [invert; 256];
    let mut c = *format.get(*at).ok_or("Unterminated scanf scanset")?;
    *at += 1;
    loop {
        set[c as usize] = !invert;
        loop {
            let next = *format.get(*at).ok_or("Unterminated scanf scanset")?;
            *at += 1;
            if next == b']' {
                return Ok(set);
            }
            if next == b'-' {
                let end = *format.get(*at).ok_or("Unterminated scanf scanset")?;
                if end == b']' || end < c {
                    c = b'-';
                    break;
                }
                if end == c {
                    return Err("Equal-endpoint scanf range unsupported".into());
                }
                *at += 1;
                for b in c + 1..=end {
                    set[b as usize] = !invert;
                }
                c = end;
            } else {
                c = next;
                break;
            }
        }
    }
}
/// Input/format exclude their NUL terminators. next supplies output pointers in
/// ABI order; suppressed fields do not consume an argument. No writes occur.
pub fn scan(input: &[u8], format: &[u8], next: &mut impl FnMut() -> Result<u64>) -> Result<Plan> {
    scan_with_rounding(input, format, next, None)
}
/// `rounding` is supplied only after the caller validates guest FP controls.
/// Non-floating and suppressed fields need no FP controls and cause no effects.
pub fn scan_with_rounding(
    input: &[u8],
    format: &[u8],
    next: &mut impl FnMut() -> Result<u64>,
    rounding: Option<crate::float_parse::Rounding>,
) -> Result<Plan> {
    if input.len() > LIMIT || format.len() > LIMIT {
        return Err("Scanf input/format exceeds prototype limit".into());
    }
    let mut plan = Plan {
        result: 0,
        writes: Vec::new(),
        errno: None,
        mxcsr_flags: 0,
        protected_ranges: Vec::new(),
    };
    let (mut at, mut pos, mut conversions) = (0, 0, 0usize);
    let mut total = 0usize;
    while at < format.len() {
        let c = format[at];
        at += 1;
        if space(c) {
            while input.get(pos).is_some_and(|c| space(*c)) {
                pos += 1;
            }
            continue;
        }
        if c != b'%' || format.get(at) == Some(&b'%') {
            if c == b'%' {
                at += 1;
            }
            if pos == input.len() {
                if conversions == 0 {
                    plan.result = -1;
                }
                return Ok(plan);
            }
            if input[pos] != c {
                return Ok(plan);
            }
            pos += 1;
            continue;
        }
        let suppress = format.get(at) == Some(&b'*');
        if suppress {
            at += 1;
        }
        let mut width = 0usize;
        while let Some(c) = format.get(at).filter(|c| c.is_ascii_digit()) {
            width = width
                .checked_mul(10)
                .and_then(|v| v.checked_add((c - b'0') as usize))
                .filter(|v| *v <= LIMIT)
                .ok_or("Scanf width exceeds prototype limit")?;
            at += 1;
        }
        let mut length = 4usize;
        let mut modifier = false;
        let mut double = false;
        if let Some(c) = format.get(at) {
            match c {
                b'h' => {
                    modifier = true;
                    at += 1;
                    length = 2;
                    if format.get(at) == Some(&b'h') {
                        at += 1;
                        length = 1;
                    }
                }
                b'l' => {
                    modifier = true;
                    at += 1;
                    length = 8;
                    double = true;
                    if format.get(at) == Some(&b'l') {
                        at += 1;
                        double = false;
                    }
                }
                b'j' | b'z' | b't' | b'q' => {
                    modifier = true;
                    length = 8;
                    at += 1;
                }
                _ => {}
            }
        }
        let conversion = *format.get(at).ok_or("Incomplete scanf conversion")?;
        at += 1;
        let floating = b"aAeEfFgG".contains(&conversion);
        if !b"diouxXpcs[n".contains(&conversion) && !floating {
            return Err(format!(
                "Unsupported scanf conversion {}",
                conversion as char
            ));
        }
        if modifier && b"cs[".contains(&conversion) {
            return Err("Wide scanf conversion unsupported".into());
        }
        if floating && modifier && !double {
            return Err("Unsupported scanf floating length modifier".into());
        }
        if conversion == b'p' {
            if modifier {
                return Err("Modified scanf pointer unsupported".into());
            }
            length = 8;
        }
        let set = if conversion == b'[' {
            Some(scanset(format, &mut at)?)
        } else {
            None
        };
        if conversion == b'n' {
            if !suppress {
                total = total
                    .checked_add(length)
                    .filter(|v| *v <= LIMIT)
                    .ok_or("Scanf output limit")?;
                if plan.writes.len() >= ARG_LIMIT {
                    return Err("Scanf argument limit".into());
                }
                plan.writes.push(Write {
                    address: next()?,
                    bytes: (pos as u64).to_le_bytes()[..length].to_vec(),
                });
            }
            continue;
        }
        if !b"c[".contains(&conversion) {
            while input.get(pos).is_some_and(|c| space(*c)) {
                pos += 1;
            }
        }
        if pos == input.len() {
            if conversions == 0 {
                plan.result = -1;
            }
            return Ok(plan);
        }
        let start = pos;
        let bytes = if b"cs[".contains(&conversion) {
            // FreeBSD GETARG occurs before string/scanset conversion, even if no match.
            let address = if suppress { None } else { Some(next()?) };
            let count = if width == 0 {
                if conversion == b'c' {
                    1
                } else {
                    LIMIT
                }
            } else {
                width
            };
            while pos < input.len() && pos - start < count {
                let b = input[pos];
                if conversion == b's' && space(b) {
                    break;
                }
                if set.as_ref().is_some_and(|s| !s[b as usize]) {
                    break;
                }
                pos += 1;
            }
            if pos == start {
                return Ok(plan);
            }
            if let Some(address) = address {
                let mut bytes = input[start..pos].to_vec();
                if conversion != b'c' {
                    bytes.push(0);
                }
                total = total
                    .checked_add(bytes.len())
                    .filter(|v| *v <= LIMIT)
                    .ok_or("Scanf output limit")?;
                if plan.writes.len() >= ARG_LIMIT {
                    return Err("Scanf argument limit".into());
                }
                plan.writes.push(Write { address, bytes });
                plan.result += 1;
            }
            None
        } else if floating {
            let count = crate::scan_float::token_len(&input[pos..], width);
            if count == 0 {
                return Ok(plan);
            }
            pos += count;
            if suppress {
                None
            } else {
                if !double {
                    return Err("Scanf binary32 policy/effects pending".into());
                }
                let rounding =
                    rounding.ok_or("Floating scanf requires supported guest controls")?;
                let effects =
                    crate::float_parse::effects::parse_effects(&input[start..pos], rounding)
                        .map_err(|e| format!("Scanf floating conversion: {e:?}"))?;
                plan.mxcsr_flags |= effects.mxcsr_flags;
                if let Some(errno) = effects.parsed.errno {
                    plan.errno = Some(errno as u32);
                }
                Some(effects.parsed.bits.to_le_bytes().to_vec())
            }
        } else {
            let count = if width == 0 { 512 } else { width.min(512) };
            let mut token = input[pos..(pos + count).min(input.len())].to_vec();
            token.push(0);
            let mut memory = Memory::default();
            memory.add(0x1000, &mut token, false, false)?;
            let base = match conversion {
                b'i' => 0,
                b'o' => 8,
                b'x' | b'X' | b'p' => 16,
                _ => 10,
            };
            let value = crate::integer_parse::parse(
                &memory,
                0x1000,
                base,
                matches!(conversion, b'd' | b'i'),
            )?;
            if value.end_offset == 0 {
                return Ok(plan);
            }
            pos += value.end_offset as usize;
            if suppress {
                None
            } else {
                if value.errno.is_some() {
                    plan.errno = value.errno;
                }
                Some(value.bits.to_le_bytes()[..length].to_vec())
            }
        };
        if let Some(bytes) = bytes {
            if plan.writes.len() >= ARG_LIMIT {
                return Err("Scanf argument limit".into());
            }
            total += bytes.len();
            if total > LIMIT {
                return Err("Scanf output limit".into());
            }
            plan.writes.push(Write {
                address: next()?,
                bytes,
            });
            plan.result += 1;
        }
        conversions += 1;
    }
    Ok(plan)
}
fn text(memory: &Memory<'_>, address: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for i in 0..LIMIT {
        let b = memory.load(
            address
                .checked_add(i as u64)
                .ok_or("Scanf string address overflow")?,
            1,
        )? as u8;
        if b == 0 {
            return Ok(bytes);
        }
        bytes.push(b);
    }
    Err("Scanf requires bounded NUL-terminated strings".into())
}
fn overlaps(address: u64, len: usize, other: u64, count: usize) -> Result<bool> {
    let end = address
        .checked_add(len as u64)
        .ok_or("Scanf output address overflow")?;
    let last = other
        .checked_add(count as u64)
        .ok_or("Scanf protected address overflow")?;
    Ok(address < last && other < end)
}
/// SysV AMD64 va_list: gp_offset:u32, fp_offset:u32, overflow_arg_area:u64,
/// reg_save_area:u64. All scanf variadic arguments are pointers, including float
/// destinations. GP slots advance by8 up to48, then overflow slots advance by8.
/// The supplied va_list is updated as FreeBSD vsscanf passes it without va_copy.
/// Caller must preguard every write and live return frame before committing;
/// errno, if any, is an additional guarded output managed by the executor.
pub fn guest_plan(memory: &Memory<'_>, source: u64, format: u64, va: u64) -> Result<Plan> {
    guest_plan_with_rounding(memory, source, format, va, None)
}
pub fn guest_plan_with_rounding(
    memory: &Memory<'_>,
    source: u64,
    format: u64,
    va: u64,
    rounding: Option<crate::float_parse::Rounding>,
) -> Result<Plan> {
    let input = text(memory, source)?;
    let fmt = text(memory, format)?;
    let (mut gp, mut stack, mut save) = (0u32, 0u64, 0u64);
    let (mut original_gp, mut original_stack) = (0u32, 0u64);
    let mut protected = vec![(source, input.len() + 1), (format, fmt.len() + 1)];
    let mut calls = 0;
    let mut next = || {
        if calls == 0 {
            let bytes = memory.read(va, 24)?;
            gp = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            stack = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
            save = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
            if gp > 48 || gp % 8 != 0 {
                return Err("Invalid scanf va_list GP offset".into());
            }
            original_gp = gp;
            original_stack = stack;
            protected.push((va, 24));
        }
        calls += 1;
        if calls > ARG_LIMIT {
            return Err("Scanf argument limit".into());
        }
        let pointer = if gp < 48 {
            let p = save
                .checked_add(gp as u64)
                .ok_or("Scanf saved argument overflow")?;
            gp += 8;
            p
        } else {
            let p = stack;
            stack = stack
                .checked_add(8)
                .ok_or("Scanf stack argument overflow")?;
            p
        };
        protected.push((pointer, 8));
        memory.load(pointer, 8)
    };
    let mut plan = scan_with_rounding(&input, &fmt, &mut next, rounding)?;
    for write in &plan.writes {
        for &(address, count) in &protected {
            if overlaps(write.address, write.bytes.len(), address, count)? {
                return Err("Scanf output aliases input/format/argument storage".into());
            }
        }
    }
    if gp != original_gp {
        for (index, &(address, count)) in protected.iter().enumerate() {
            if index == 2 {
                continue;
            } // intentional va header update
            if overlaps(va, 4, address, count)? {
                return Err("Scanf va_list cursor aliases input/format/argument storage".into());
            }
        }
        plan.writes.push(Write {
            address: va,
            bytes: gp.to_le_bytes().to_vec(),
        });
    }
    if stack != original_stack {
        for (index, &(address, count)) in protected.iter().enumerate() {
            if index == 2 {
                continue;
            }
            if overlaps(
                va.checked_add(8).ok_or("Scanf va_list overflow")?,
                8,
                address,
                count,
            )? {
                return Err("Scanf va_list cursor aliases input/format/argument storage".into());
            }
        }
        plan.writes.push(Write {
            address: va.checked_add(8).ok_or("Scanf va_list overflow")?,
            bytes: stack.to_le_bytes().to_vec(),
        });
    }
    plan.protected_ranges = protected;
    Ok(plan)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn double_fields_rollback_rounding_and_sticky_effects() {
        use crate::float_parse::Rounding;
        let mut next = || Ok(0x2000);
        let p =
            scan_with_rounding(b"1e+", b"%lf%c%n", &mut next, Some(Rounding::NearestEven)).unwrap();
        assert_eq!(p.result, 2);
        assert_eq!(p.writes[0].bytes, 0x3ff0000000000000u64.to_le_bytes());
        assert_eq!(p.writes[1].bytes, b"e");
        assert_eq!(p.writes[2].bytes, 2u32.to_le_bytes());
        let p =
            scan_with_rounding(b"0.1 1e-400", b"%lf %le", &mut next, Some(Rounding::Down)).unwrap();
        assert_eq!((p.result, p.errno, p.mxcsr_flags), (2, Some(34), 0x30));
        assert_eq!(p.writes[0].bytes, 0x3fb9999999999999u64.to_le_bytes());
        assert_eq!(p.writes[1].bytes, (1u64 << 63).to_le_bytes());
        assert!(scan(b"0.1", b"%lf", &mut next).is_err());
        assert!(scan_with_rounding(b"0.1", b"%f", &mut next, Some(Rounding::NearestEven)).is_err());
    }
    #[test]
    fn suppressed_floats_have_no_conversion_effects_or_argument() {
        let mut calls = 0;
        let p = scan(b"1e400", b"%*f%n", &mut || {
            calls += 1;
            Ok(0x1000)
        })
        .unwrap();
        assert_eq!((p.result, p.errno, p.mxcsr_flags, calls), (0, None, 0, 1));
        assert_eq!(p.writes[0].bytes, 5u32.to_le_bytes());
        let p = scan(b"1e+", b"%*2lf%c", &mut || Ok(0x1000)).unwrap();
        assert_eq!((p.result, p.mxcsr_flags), (1, 0));
        assert_eq!(p.writes[0].bytes, b"e");
    }
    #[test]
    fn cursor_cannot_alias_argument_storage() {
        let mut input = *b"1\0";
        let mut format = *b"%d\0";
        let mut va = [0u8; 24];
        va[16..].copy_from_slice(&0x2000u64.to_le_bytes());
        let mut memory = Memory::default();
        memory.add(0x1000, &mut input, false, false).unwrap();
        memory.add(0x1100, &mut format, false, false).unwrap();
        memory.add(0x2000, &mut va, true, false).unwrap();
        assert!(guest_plan(&memory, 0x1000, 0x1100, 0x2000)
            .unwrap_err()
            .contains("cursor aliases"));
    }
    fn run(input: &[u8], fmt: &[u8]) -> Plan {
        let mut n = 0;
        scan(input, fmt, &mut || {
            n += 1;
            Ok(0x1000 + n * 16)
        })
        .unwrap()
    }
    #[test]
    fn integer_widths_bases_suppression_count() {
        let p = run(
            b" -129 65535 0xff 077 -1 0xabc",
            b"%hhd %hu %i %o %llu %p%n",
        );
        assert_eq!(p.result, 6);
        let expected = [
            vec![127],
            vec![255, 255],
            255u32.to_le_bytes().to_vec(),
            63u32.to_le_bytes().to_vec(),
            u64::MAX.to_le_bytes().to_vec(),
            0xabcu64.to_le_bytes().to_vec(),
            29u32.to_le_bytes().to_vec(),
        ];
        for (w, e) in p.writes.iter().zip(expected) {
            assert_eq!(w.bytes, e);
        }
        let p = run(b"99 7", b"%*d%d");
        assert_eq!(p.result, 1);
        assert_eq!(p.writes[0].address, 0x1010);
        assert_eq!(p.writes[0].bytes, 7u32.to_le_bytes());
        let p = run(b"9223372036854775808", b"%jd");
        assert_eq!(p.errno, Some(34));
        assert_eq!(p.writes[0].bytes, i64::MAX.to_le_bytes());
        let p = run(b"0xg", b"%i%n");
        assert_eq!(p.result, 1);
        assert_eq!(p.writes[1].bytes, 1u32.to_le_bytes());
        let p = run(&vec![b'9'; 600], b"%u%n");
        assert_eq!(p.writes[1].bytes, 512u32.to_le_bytes());
        assert_eq!(p.errno, Some(34));
    }
    #[test]
    fn strings_scansets_partial_char_and_eof() {
        let p = run(b"abc-def Z!", b"%3[a-z]%*1c%3s %2c%n");
        assert_eq!(p.result, 3);
        assert_eq!(p.writes[0].bytes, b"abc\0");
        assert_eq!(p.writes[1].bytes, b"def\0");
        assert_eq!(p.writes[2].bytes, b"Z!");
        assert_eq!(p.writes[3].bytes, 10u32.to_le_bytes());
        assert_eq!(run(b"x", b"%3c").writes[0].bytes, b"x");
        assert_eq!(run(b"", b"%d").result, -1);
        assert_eq!(run(b"x", b"%d").result, 0);
        assert_eq!(run(b"1", b"%*d%d").result, 0);
        assert_eq!(run(b"", b"%n%d").result, -1);
        assert_eq!(run(b"1 x", b"%d%d").result, 1);
        assert_eq!(run(b"]-abc!", b"%[]a-c-]").writes[0].bytes, b"]-abc\0");
        assert_eq!(run(b"abc!", b"%[^!]").writes[0].bytes, b"abc\0");
        assert_eq!(run(b"abcde!", b"%[a-c-e]").writes[0].bytes, b"abcde\0");
        assert_eq!(run(b"!", b"%[a-z]").result, 0);
        for fmt in [
            b"%f".as_slice(),
            b"%lf",
            b"%ls",
            b"%2$d",
            b"%[a-a]",
            b"%999999999999c",
        ] {
            assert!(scan(b"1", fmt, &mut || Ok(1)).is_err());
        }
    }
    #[test]
    fn guarded_guest_varargs_register_stack_and_alias_boundary() {
        let mut source = *b"1 2\0";
        let mut format = *b"%d%d\0";
        let mut va = [0u8; 24];
        va[..4].copy_from_slice(&40u32.to_le_bytes());
        va[8..16].copy_from_slice(&0x4000u64.to_le_bytes());
        va[16..].copy_from_slice(&0x3000u64.to_le_bytes());
        let mut saved = [0u8; 48];
        saved[40..].copy_from_slice(&0x5000u64.to_le_bytes());
        let mut stack = 0x5004u64.to_le_bytes();
        let mut m = Memory::default();
        m.add(0x1000, &mut source, false, false).unwrap();
        m.add(0x1100, &mut format, false, false).unwrap();
        m.add(0x2000, &mut va, true, false).unwrap();
        m.add(0x3000, &mut saved, false, false).unwrap();
        m.add(0x4000, &mut stack, false, false).unwrap();
        let p = guest_plan(&m, 0x1000, 0x1100, 0x2000).unwrap();
        assert_eq!(p.result, 2);
        assert_eq!(p.writes[0].address, 0x5000);
        assert_eq!(p.writes[1].address, 0x5004);
        assert_eq!(p.writes[2].bytes, 48u32.to_le_bytes());
        assert_eq!(p.writes[3].bytes, 0x4008u64.to_le_bytes());
        assert!(guest_plan(&m, u64::MAX, 0x1100, 0x2000).is_err());
        drop(m);
        saved[40..].copy_from_slice(&0x1000u64.to_le_bytes());
        let mut m = Memory::default();
        m.add(0x1000, &mut source, false, false).unwrap();
        m.add(0x1100, &mut format, false, false).unwrap();
        m.add(0x2000, &mut va, true, false).unwrap();
        m.add(0x3000, &mut saved, false, false).unwrap();
        m.add(0x4000, &mut stack, false, false).unwrap();
        assert!(guest_plan(&m, 0x1000, 0x1100, 0x2000).is_err());
    }
}
