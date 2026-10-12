//! Bounded integer/string printf formatting. Build completely before committing.
use crate::strings::{Fault, LIMIT, Memory};
fn push(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Fault> {
    if bytes.len() > LIMIT - out.len() {
        return Err(Fault::Limit);
    }
    out.try_reserve(bytes.len())
        .map_err(|_| Fault::Allocation)?;
    out.extend_from_slice(bytes);
    Ok(())
}
fn repeat(out: &mut Vec<u8>, byte: u8, count: usize) -> Result<(), Fault> {
    if count > LIMIT - out.len() {
        return Err(Fault::Limit);
    }
    out.try_reserve(count).map_err(|_| Fault::Allocation)?;
    out.resize(out.len() + count, byte);
    Ok(())
}
fn text(memory: &impl Memory, address: u64, limit: usize, bounded: bool) -> Result<Vec<u8>, Fault> {
    let mut value = Vec::new();
    for offset in 0..limit {
        let c = memory.read(address.checked_add(offset as u64).ok_or(Fault::Read)?)?;
        if c == 0 {
            return Ok(value);
        }
        push(&mut value, &[c])?;
    }
    if bounded {
        Ok(value)
    } else {
        Err(Fault::Limit)
    }
}
fn number(text: &[u8], index: &mut usize) -> Result<usize, Fault> {
    let mut value = 0usize;
    while let Some(&c) = text.get(*index).filter(|c| c.is_ascii_digit()) {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((c - b'0') as usize))
            .filter(|v| *v <= LIMIT)
            .ok_or(Fault::Limit)?;
        *index += 1;
    }
    Ok(value)
}
pub fn render(
    memory: &impl Memory,
    address: u64,
    next: &mut impl FnMut() -> Result<u64, Fault>,
) -> Result<Vec<u8>, Fault> {
    let format = text(memory, address, LIMIT, false)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < format.len() {
        if format[i] != b'%' {
            push(&mut out, &format[i..i + 1])?;
            i += 1;
            continue;
        }
        i += 1;
        if format.get(i) == Some(&b'%') {
            push(&mut out, b"%")?;
            i += 1;
            continue;
        }
        let (mut left, mut plus, mut space, mut alternate, mut zero) =
            (false, false, false, false, false);
        while let Some(&c) = format.get(i) {
            match c {
                b'-' => left = true,
                b'+' => plus = true,
                b' ' => space = true,
                b'#' => alternate = true,
                b'0' => zero = true,
                _ => break,
            };
            i += 1;
        }
        let width = if format.get(i) == Some(&b'*') {
            i += 1;
            let n = next()? as i32 as i64;
            if n < 0 {
                left = true;
            }
            let n = n.unsigned_abs();
            if n > LIMIT as u64 {
                return Err(Fault::Limit);
            }
            n as usize
        } else {
            number(&format, &mut i)?
        };
        let mut precision = None;
        if format.get(i) == Some(&b'.') {
            i += 1;
            precision = if format.get(i) == Some(&b'*') {
                i += 1;
                let n = next()? as i32;
                if n < 0 { None } else { Some(n as usize) }
            } else {
                Some(number(&format, &mut i)?)
            };
            if precision.is_some_and(|n| n > LIMIT) {
                return Err(Fault::Limit);
            }
        }
        let mut bits = 32;
        let mut length = false;
        if format.get(i) == Some(&b'h') {
            length = true;
            i += 1;
            bits = 16;
            if format.get(i) == Some(&b'h') {
                bits = 8;
                i += 1;
            }
        } else if format.get(i).is_some_and(|c| b"ljzt".contains(c)) {
            length = true;
            bits = 64;
            let c = format[i];
            i += 1;
            if c == b'l' && format.get(i) == Some(&b'l') {
                i += 1;
            }
        }
        let conversion = *format.get(i).ok_or(Fault::Limit)?;
        i += 1;
        let mut digits = Vec::new();
        let mut prefix = Vec::new();
        let numeric = b"diuoxXp".contains(&conversion);
        match conversion {
            b's' => {
                if length {
                    return Err(Fault::Limit);
                }
                digits = text(
                    memory,
                    next()?,
                    precision.unwrap_or(LIMIT),
                    precision.is_some(),
                )?;
            }
            b'c' => {
                if length {
                    return Err(Fault::Limit);
                }
                push(&mut digits, &[next()? as u8])?;
            }
            _ if numeric => {
                if conversion == b'p' {
                    if length {
                        return Err(Fault::Limit);
                    }
                    bits = 64;
                }
                let mask = u64::MAX >> (64 - bits);
                let mut value = next()? & mask;
                let original = value;
                if conversion == b'd' || conversion == b'i' {
                    if value & (1u64 << (bits - 1)) != 0 {
                        prefix.push(b'-');
                        value = value.wrapping_neg() & mask;
                    } else if plus {
                        prefix.push(b'+');
                    } else if space {
                        prefix.push(b' ');
                    }
                }
                let base = match conversion {
                    b'o' => 8,
                    b'x' | b'X' | b'p' => 16,
                    _ => 10,
                };
                let alphabet = if conversion == b'X' {
                    b"0123456789ABCDEF"
                } else {
                    b"0123456789abcdef"
                };
                if value != 0 || precision != Some(0) {
                    loop {
                        digits.push(alphabet[(value % base) as usize]);
                        value /= base;
                        if value == 0 {
                            break;
                        }
                    }
                }
                digits.reverse();
                let zeros = precision.unwrap_or(0).saturating_sub(digits.len());
                if zeros > 0 {
                    let mut padded = Vec::new();
                    repeat(&mut padded, b'0', zeros)?;
                    push(&mut padded, &digits)?;
                    digits = padded;
                }
                if conversion == b'o' && alternate && digits.first() != Some(&b'0') {
                    digits.insert(0, b'0');
                }
                if conversion == b'p'
                    || alternate && original != 0 && (conversion == b'x' || conversion == b'X')
                {
                    prefix = if conversion == b'X' {
                        b"0X".to_vec()
                    } else {
                        b"0x".to_vec()
                    };
                }
            }
            _ => return Err(Fault::Limit),
        }
        let padding = width.saturating_sub(prefix.len() + digits.len());
        if left {
            push(&mut out, &prefix)?;
            push(&mut out, &digits)?;
            repeat(&mut out, b' ', padding)?;
        } else if zero && numeric && precision.is_none() {
            push(&mut out, &prefix)?;
            repeat(&mut out, b'0', padding)?;
            push(&mut out, &digits)?;
        } else {
            repeat(&mut out, b' ', padding)?;
            push(&mut out, &prefix)?;
            push(&mut out, &digits)?;
        }
    }
    Ok(out)
}
#[repr(C)]
pub struct Arguments {
    pub registers: [u64; 6],
    pub stack: u64,
}
pub fn sprintf(memory: &mut impl Memory, args: Arguments) -> Result<u64, Fault> {
    formatted(memory, args, 0)
}
/// mode 0=sprintf, 1=snprintf, 2=vsnprintf using the guest SysV va_list layout.
pub fn formatted(memory: &mut impl Memory, args: Arguments, mode: u64) -> Result<u64, Fault> {
    if mode > 2 {
        return Err(Fault::Limit);
    }
    let mut index = if mode == 0 { 2usize } else { 3 };
    let mut stack = args.stack.checked_add(8).ok_or(Fault::Read)?;
    let mut gp = 0u32;
    let mut save = 0u64;
    if mode == 2 {
        let mut va = [0u8; 24];
        memory.read_into(args.registers[3], &mut va)?;
        gp = u32::from_le_bytes(va[..4].try_into().map_err(|_| Fault::Read)?);
        if gp > 48 || gp % 8 != 0 {
            return Err(Fault::Limit);
        }
        stack = u64::from_le_bytes(va[8..16].try_into().map_err(|_| Fault::Read)?);
        save = u64::from_le_bytes(va[16..].try_into().map_err(|_| Fault::Read)?);
    }
    let mut next = || {
        if mode != 2 && index < 6 {
            let value = args.registers[index];
            index += 1;
            return Ok(value);
        }
        let mut bytes = [0u8; 8];
        if mode == 2 && gp < 48 {
            memory.read_into(save.checked_add(gp as u64).ok_or(Fault::Read)?, &mut bytes)?;
            gp += 8;
            return Ok(u64::from_le_bytes(bytes));
        }
        memory.read_into(stack, &mut bytes)?;
        stack = stack.checked_add(8).ok_or(Fault::Read)?;
        Ok(u64::from_le_bytes(bytes))
    };
    let mut output = render(
        memory,
        args.registers[if mode == 0 { 1 } else { 2 }],
        &mut next,
    )?;
    let length = output.len();
    let capacity = if mode == 0 {
        u64::MAX
    } else {
        args.registers[1]
    };
    if capacity != 0 {
        output.truncate((capacity - 1).min(output.len() as u64) as usize);
        output.try_reserve(1).map_err(|_| Fault::Allocation)?;
        output.push(0);
        memory.write(args.registers[0], &output)?;
    }
    Ok(length as u64)
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Mem(Vec<u8>);
    impl Memory for Mem {
        fn read(&self, address: u64) -> Result<u8, Fault> {
            self.0.get(address as usize).copied().ok_or(Fault::Read)
        }
        fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Fault> {
            let end = address.checked_add(data.len() as u64).ok_or(Fault::Write)?;
            self.0
                .get_mut(address as usize..end as usize)
                .ok_or(Fault::Write)?
                .copy_from_slice(data);
            Ok(())
        }
    }
    fn format(f: &[u8], values: &[u64]) -> Result<Vec<u8>, Fault> {
        let mut mem = f.to_vec();
        mem.push(0);
        let m = Mem(mem);
        let mut values = values.iter();
        render(&m, 0, &mut || values.next().copied().ok_or(Fault::Read))
    }
    #[test]
    fn integers_flags_and_lengths() {
        assert_eq!(
            format(
                b"%+08d|%#x|%#.0o|%.0u|%hhd|%hd|%lld",
                &[(-12i64) as u64, 42, 0, 0, 255, 65535, 1 << 63]
            )
            .unwrap(),
            b"-0000012|0x2a|0||-1|-1|-9223372036854775808"
        );
        assert_eq!(
            format(b"%*.*u|%zu|%p|%%", &[(-8i64) as u64, 4, 42, u64::MAX, 0]).unwrap(),
            b"0042    |18446744073709551615|0x0|%"
        );
    }
    #[test]
    fn rejects_unsupported_and_limits() {
        for fmt in [
            b"%n".as_slice(),
            b"%f",
            b"%ls",
            b"%1$d",
            b"%",
            b"%999999999999999999u",
        ] {
            assert_eq!(format(fmt, &[1]), Err(Fault::Limit));
        }
        assert_eq!(format(b"%*d", &[i32::MIN as u64, 1]), Err(Fault::Limit));
    }
    #[test]
    fn precision_bounds_string_reads() {
        let m = Mem(b"%.3s\0abc".to_vec());
        assert_eq!(render(&m, 0, &mut || Ok(5)).unwrap(), b"abc");
        let m = Mem(b"%.0s\0".to_vec());
        assert_eq!(render(&m, 0, &mut || Ok(u64::MAX)).unwrap(), b"");
    }
    #[test]
    fn register_stack_arguments_and_atomic_failure() {
        let mut m = Mem(vec![0xa5; 128]);
        m.0[..18].copy_from_slice(b"%u %u %u %u %u %u\0");
        m.0[40..48].copy_from_slice(&5u64.to_le_bytes());
        m.0[48..56].copy_from_slice(&6u64.to_le_bytes());
        let args = || Arguments {
            registers: [64, 0, 1, 2, 3, 4],
            stack: 32,
        };
        assert_eq!(sprintf(&mut m, args()), Ok(11));
        assert_eq!(&m.0[64..76], b"1 2 3 4 5 6\0");
        m.0[..3].copy_from_slice(b"%n\0");
        let before = m.0.clone();
        assert_eq!(sprintf(&mut m, args()), Err(Fault::Limit));
        assert_eq!(m.0, before);
    }
}
