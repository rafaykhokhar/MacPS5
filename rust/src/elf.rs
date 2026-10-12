use crate::Result;

#[derive(Debug, Clone)]
pub struct Segment {
    pub address: u64,
    pub offset: usize,
    pub file_size: usize,
    pub memory_size: u64,
    pub flags: u32,
}

pub struct Elf<'a> {
    bytes: &'a [u8],
    pub entry: u64,
    pub segments: Vec<Segment>,
}

fn read<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N]> {
    let end = offset.checked_add(N).ok_or("ELF offset overflow")?;
    bytes
        .get(offset..end)
        .ok_or_else(|| String::from("Truncated ELF field"))?
        .try_into()
        .map_err(|_| "Invalid ELF field width".into())
}
fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(read(bytes, offset)?))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(read(bytes, offset)?))
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(read(bytes, offset)?))
}

impl<'a> Elf<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() < 64 {
            return Err("Truncated ELF header".into());
        }
        if bytes[..7] != *b"\x7fELF\x02\x01\x01"
            || u16_at(bytes, 16)? != 3
            || u16_at(bytes, 18)? != 62
        {
            return Err("Requires little-endian x86-64 ET_DYN ELF".into());
        }
        if u32_at(bytes, 20)? != 1 || u16_at(bytes, 52)? != 64 || u16_at(bytes, 54)? != 56 {
            return Err("Unsupported ELF header layout".into());
        }
        let table = usize::try_from(u64_at(bytes, 32)?).map_err(|_| "ELF table offset overflow")?;
        let count = usize::from(u16_at(bytes, 56)?);
        if count == 0 || count == 0xffff {
            return Err("Unsupported ELF program count".into());
        }
        let end = table
            .checked_add(count.checked_mul(56).ok_or("ELF table overflow")?)
            .ok_or("ELF table overflow")?;
        if end > bytes.len() {
            return Err("ELF program table out of bounds".into());
        }
        let mut segments: Vec<Segment> = Vec::new();
        for index in 0..count {
            let pos = table + index * 56;
            if u32_at(bytes, pos)? != 1 {
                continue;
            }
            let offset =
                usize::try_from(u64_at(bytes, pos + 8)?).map_err(|_| "ELF file offset overflow")?;
            let address = u64_at(bytes, pos + 16)?;
            let file_size =
                usize::try_from(u64_at(bytes, pos + 32)?).map_err(|_| "ELF file size overflow")?;
            let memory_size = u64_at(bytes, pos + 40)?;
            if file_size as u64 > memory_size
                || offset
                    .checked_add(file_size)
                    .is_none_or(|v| v > bytes.len())
            {
                return Err("ELF segment outside file or memory extent".into());
            }
            let segment_end = address
                .checked_add(memory_size)
                .ok_or("ELF virtual range overflow")?;
            if memory_size != 0
                && segments.iter().any(|s| {
                    s.memory_size != 0
                        && address < s.address + s.memory_size
                        && s.address < segment_end
                })
            {
                return Err("Overlapping ELF load segments".into());
            }
            segments.push(Segment {
                address,
                offset,
                file_size,
                memory_size,
                flags: u32_at(bytes, pos + 4)?,
            });
        }
        let image = Self {
            bytes,
            entry: u64_at(bytes, 24)?,
            segments,
        };
        image.code(image.entry, 1)?;
        Ok(image)
    }
    pub fn code(&self, address: u64, limit: usize) -> Result<&'a [u8]> {
        if limit == 0 {
            return Err("Code read length must be positive".into());
        }
        for segment in &self.segments {
            let Some(delta) = address.checked_sub(segment.address) else {
                continue;
            };
            if segment.flags & 1 == 0 || delta >= segment.file_size as u64 {
                continue;
            }
            let delta = delta as usize;
            let start = segment.offset + delta;
            let count = limit.min(segment.file_size - delta);
            return Ok(&self.bytes[start..start + count]);
        }
        Err(format!("No file-backed executable address at {address:#x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Vec<u8> {
        let mut b = vec![0; 128];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (p, v) in [(16, 3u16), (18, 62), (52, 64), (54, 56), (56, 1)] {
            b[p..p + 2].copy_from_slice(&v.to_le_bytes());
        }
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        for (p, v) in [
            (24, 0x1000u64),
            (32, 64),
            (72, 120),
            (80, 0x1000),
            (96, 8),
            (104, 8),
        ] {
            b[p..p + 8].copy_from_slice(&v.to_le_bytes());
        }
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[68..72].copy_from_slice(&5u32.to_le_bytes());
        b[120] = 0xc3;
        b
    }
    #[test]
    fn maps_virtual_address_to_file() {
        let b = fixture();
        let elf = Elf::parse(&b).unwrap();
        assert_eq!(elf.code(0x1000, 1).unwrap(), [0xc3]);
        assert_eq!(elf.code(0x1007, 15).unwrap().len(), 1);
        assert!(elf.code(0x1008, 1).is_err());
    }
    #[test]
    fn rejects_every_truncation() {
        let b = fixture();
        for end in 0..b.len() {
            assert!(Elf::parse(&b[..end]).is_err(), "{end}");
        }
    }
    #[test]
    fn rejects_overflow_and_bad_layout() {
        for (pos, value) in [
            (32, u64::MAX),
            (72, u64::MAX),
            (80, u64::MAX),
            (96, 9),
            (104, 7),
        ] {
            let mut b = fixture();
            b[pos..pos + 8].copy_from_slice(&value.to_le_bytes());
            assert!(Elf::parse(&b).is_err());
        }
        let mut b = fixture();
        b[68] = 4;
        assert!(Elf::parse(&b).is_err());
    }
}
