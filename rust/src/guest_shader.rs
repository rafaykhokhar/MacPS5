//! Guarded snapshots of the 96-byte AGC shader object described by SceShaders.
//! Metadata pointers are guest addresses, never host pointers. Reading metadata
//! does not implement its resource, semantic or special-register behavior.
use crate::{
    Result, guest_memory::Memory, pm4::State, rdna_metal::RenderStage, render_plan::Registry,
};

pub struct Registration {
    pub header_address: u64,
    pub code_address: u64,
    pub stage: RenderStage,
    pub registers: State,
    pub header: Vec<u8>,
    pub words: Vec<u32>,
}
fn field(bytes: &[u8], offset: usize, size: usize) -> u64 {
    bytes[offset..offset + size]
        .iter()
        .enumerate()
        .fold(0, |v, (i, b)| v | (u64::from(*b) << (i * 8)))
}
fn range(base: u64, size: usize, pointer: u64, count: u64, element: usize) -> Result<usize> {
    if count == 0 {
        return Ok(0);
    }
    let offset = pointer
        .checked_sub(base)
        .ok_or("Shader metadata precedes header")?;
    if offset > size as u64 || count > (size as u64 - offset) / element as u64 {
        return Err("Shader metadata outside header snapshot".into());
    }
    Ok(offset as usize)
}
/// Validate all supported object-layout pointers and snapshot header/code before
/// mutating any shader registry. Unknown shader stages are rejected.
pub fn read(memory: &Memory<'_>, address: u64) -> Result<Registration> {
    if address == 0 {
        return Err("Null shader header".into());
    }
    let fixed = memory.read(address, 96)?;
    if field(fixed, 0, 4) != 0x34333231 || field(fixed, 4, 4) != 0x18 {
        return Err("Unsupported AGC shader magic or version".into());
    }
    let header_size = field(fixed, 64, 4) as usize;
    let code_size = field(fixed, 68, 4) as usize;
    let code_address = field(fixed, 16, 8);
    if !(96..=65536).contains(&header_size)
        || code_size == 0
        || code_size > 16384
        || code_size % 4 != 0
        || code_address == 0
        || code_address & 255 != 0
    {
        return Err("Invalid AGC header or code range".into());
    }
    let stage = match fixed[90] {
        1 => RenderStage::Fragment,
        2 => RenderStage::Vertex,
        _ => return Err("Unsupported AGC shader stage".into()),
    };
    let header = memory.read(address, header_size)?.to_vec();
    // Rust's exclusive mapping ownership prevents concurrent writes during this
    // read. Retain a separate owned snapshot for subsequent guest mutations.
    let mut registers = State::default();
    for (pointer_field, count_field, bank) in [
        (32, 92, &mut registers.shader),
        (24, 91, &mut registers.context),
    ] {
        let count = field(&header, count_field, 1);
        let offset = range(
            address,
            header_size,
            field(&header, pointer_field, 8),
            count,
            8,
        )?;
        for i in 0..count as usize {
            let register = field(&header, offset + i * 8, 4) as u32;
            if register > 0xffff {
                return Err("Unsupported shader metadata register index".into());
            }
            bank.insert(register, field(&header, offset + i * 8 + 4, 4) as u32);
        }
    }
    range(
        address,
        header_size,
        field(&header, 48, 8),
        field(&header, 80, 4),
        4,
    )?;
    range(
        address,
        header_size,
        field(&header, 56, 8),
        field(&header, 86, 2),
        4,
    )?;
    let special = field(&header, 40, 8);
    if special != 0 {
        range(address, header_size, special, 1, 48)?;
    }
    let user = field(&header, 8, 8);
    if user != 0 {
        let offset = range(address, header_size, user, 1, 56)?;
        range(
            address,
            header_size,
            field(&header, offset, 8),
            field(&header, offset + 44, 2),
            2,
        )?;
        for i in 0..4 {
            range(
                address,
                header_size,
                field(&header, offset + 8 + i * 8, 8),
                field(&header, offset + 46 + i * 2, 2),
                2,
            )?;
        }
    }
    let code = memory.read(code_address, code_size)?;
    let words = code
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    Ok(Registration {
        header_address: address,
        code_address,
        stage,
        registers,
        header,
        words,
    })
}
impl Registration {
    /// Register only the explicit raw-VGPR/SGPR test pipeline. Metadata requiring
    /// resource binding, register application, scratch or semantics is rejected;
    /// callers must implement those features before accepting richer objects.
    pub fn register_simple(&self, registry: &mut Registry) -> Result<()> {
        if self.header.len() < 96 {
            return Err("Truncated shader registration snapshot".into());
        }
        if !self.registers.shader.is_empty()
            || !self.registers.context.is_empty()
            || field(&self.header, 8, 8) != 0
            || field(&self.header, 40, 8) != 0
            || field(&self.header, 24, 8) != 0
            || field(&self.header, 32, 8) != 0
            || field(&self.header, 48, 8) != 0
            || field(&self.header, 56, 8) != 0
            || field(&self.header, 72, 4) != 0
            || field(&self.header, 76, 4) != 0
            || field(&self.header, 80, 4) != 0
            || field(&self.header, 84, 2) != 0
            || field(&self.header, 86, 2) != 0
            || field(&self.header, 88, 2) != 0
            || field(&self.header, 91, 1) != 0
            || field(&self.header, 92, 1) != 0
        {
            return Err("Shader metadata exceeds explicit raw-register render pipeline".into());
        }
        registry.register(self.code_address, self.stage, &self.words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn put(bytes: &mut [u8], offset: usize, value: u64, size: usize) {
        bytes[offset..offset + size].copy_from_slice(&value.to_le_bytes()[..size]);
    }
    fn header() -> Vec<u8> {
        let mut bytes = vec![0; 256];
        for (offset, value, size) in [
            (0, 0x34333231, 4),
            (4, 0x18, 4),
            (16, 0x20000, 8),
            (64, 256, 4),
            (68, 12, 4),
            (90, 2, 1),
        ] {
            put(&mut bytes, offset, value, size);
        }
        bytes
    }
    fn code() -> Vec<u8> {
        [0xf80008cf_u32, 0x03020100, 0xbf810000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }
    #[test]
    fn immutable_snapshots_and_bounded_metadata() {
        let mut header = header();
        let mut code = code();
        // SH register metadata, semantics, specials and user-data resources all
        // reside in the bounded header; parsing must preserve their raw bytes.
        for (offset, value, size) in [
            (32, 0x10060, 8),
            (92, 1, 1),
            (96, 0xc8, 4),
            (100, 0x200, 4),
            (48, 0x10068, 8),
            (80, 1, 4),
            (56, 0x1006c, 8),
            (86, 1, 2),
            (40, 0x10070, 8),
            (8, 0x100a0, 8),
            (160, 0x100d8, 8),
            (204, 1, 2),
            (168, 0x100da, 8),
            (206, 1, 2),
        ] {
            put(&mut header, offset, value, size);
        }
        let mut memory = Memory::default();
        memory.add(0x10000, &mut header, true, false).unwrap();
        memory.add(0x20000, &mut code, true, false).unwrap();
        let registration = read(&memory, 0x10000).unwrap();
        assert_eq!(registration.registers.shader.get(&0xc8), Some(&0x200));
        assert!(
            registration
                .register_simple(&mut Registry::default())
                .is_err()
        );
        memory
            .write(0x20000, &0xffffffff_u32.to_le_bytes())
            .unwrap();
        memory.write(0x10000, &0_u32.to_le_bytes()).unwrap();
        assert_eq!(registration.words[0], 0xf80008cf);
        assert_eq!(field(&registration.header, 0, 4), 0x34333231);
    }
    #[test]
    fn rejects_bad_headers_and_metadata_without_registering() {
        for (offset, value, size) in [
            (0, 0, 4),
            (4, 0x19, 4),
            (64, 95, 4),
            (64, 65537, 4),
            (68, 0, 4),
            (68, 13, 4),
            (68, 16388, 4),
            (16, 0x20001, 8),
            (16, 0x30000, 8),
            (90, 3, 1),
            (64, 257, 4),
        ] {
            let mut header = header();
            put(&mut header, offset, value, size);
            let mut code = code();
            let mut memory = Memory::default();
            memory.add(0x10000, &mut header, false, false).unwrap();
            memory.add(0x20000, &mut code, false, false).unwrap();
            assert!(read(&memory, 0x10000).is_err(), "field {offset}");
        }
        for (pointer, count, count_size) in [(32, 92, 1), (24, 91, 1), (48, 80, 4), (56, 86, 2)] {
            let mut header = header();
            put(&mut header, pointer, 0x100ff, 8);
            put(&mut header, count, 1, count_size);
            let mut code = code();
            let mut memory = Memory::default();
            memory.add(0x10000, &mut header, false, false).unwrap();
            memory.add(0x20000, &mut code, false, false).unwrap();
            assert!(read(&memory, 0x10000).is_err());
        }
        let mut header = header();
        let mut code = code();
        let mut memory = Memory::default();
        memory.add(0x10000, &mut header, false, false).unwrap();
        memory.add(0x20000, &mut code, false, false).unwrap();
        assert!(read(&memory, 0).is_err());
        assert!(read(&memory, u64::MAX).is_err());
        read(&memory, 0x10000)
            .unwrap()
            .register_simple(&mut Registry::default())
            .unwrap();
    }

    #[test]
    fn resource_requirements_cannot_be_silently_registered() {
        for (offset, size) in [(72, 4), (76, 4), (84, 2), (88, 2)] {
            let mut header = header();
            put(&mut header, offset, 1, size);
            let mut code = code();
            let mut memory = Memory::default();
            memory.add(0x10000, &mut header, false, false).unwrap();
            memory.add(0x20000, &mut code, false, false).unwrap();
            let registration = read(&memory, 0x10000).unwrap();
            assert!(
                registration
                    .register_simple(&mut Registry::default())
                    .is_err()
            );
        }
        // A bounded outer user-data object cannot authorize an out-of-bounds
        // nested resource array, including when its pointer underflows the base.
        for pointer in [0xffff, 0x100ff, u64::MAX] {
            let mut header = header();
            put(&mut header, 8, 0x10060, 8);
            put(&mut header, 96, pointer, 8);
            put(&mut header, 96 + 44, 1, 2);
            let mut code = code();
            let mut memory = Memory::default();
            memory.add(0x10000, &mut header, false, false).unwrap();
            memory.add(0x20000, &mut code, false, false).unwrap();
            assert!(read(&memory, 0x10000).is_err());
        }
    }
}
