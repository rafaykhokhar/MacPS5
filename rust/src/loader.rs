//! Owned guest ELF segments and transactional standard/SCE ELF64 RELA relocation.
//! Addresses remain guest integers; executable bytes are never called as host code.
use crate::{Result, elf::Elf};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug)]
pub struct Segment {
    pub address: u64,
    pub flags: u32,
    pub bytes: Vec<u8>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Import {
    pub name: String,
    pub address: u64,
    pub kind: u32,
    pub identity: crate::imports::Identity,
}
pub struct Image {
    pub(crate) file: Vec<u8>,
    pub segments: Vec<Segment>,
    pub bias: u64,
    pub entry: u64,
    dynamic: Option<(usize, usize)>,
    pub(crate) dynlib: Option<(usize, usize)>,
    file_segments: Vec<crate::elf::Segment>,
    pub dependencies: Vec<String>,
    pub unresolved: Vec<Import>,
    pub applied: usize,
    pub relocations: Vec<(u64, u64)>,
    pub(crate) relocated: bool,
    pub(crate) dynamic_tags: BTreeMap<u64, u64>,
    pub(crate) export_owners: crate::ownership::Exports,
}
fn span(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8]> {
    bytes
        .get(offset..offset.checked_add(size).ok_or("ELF range overflow")?)
        .ok_or_else(|| "ELF range outside file".into())
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        span(bytes, offset, 8)?.try_into().unwrap(),
    ))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        span(bytes, offset, 4)?.try_into().unwrap(),
    ))
}
impl Image {
    pub fn load(file: &[u8], bias: u64, budget: usize) -> Result<Self> {
        if file.len() > budget {
            return Err("ELF file exceeds loader budget".into());
        }
        let elf = Elf::parse(file)?;
        let table = usize::try_from(u64_at(file, 32)?).map_err(|_| "ELF table overflow")?;
        if table < 64 || u32_at(file, 48)? != 0 {
            return Err("Unsupported ELF header layout".into());
        }
        let count = u16::from_le_bytes(span(file, 56, 2)?.try_into().unwrap()) as usize;
        let mut dynamic = None;
        let mut dynlib = None;
        for index in 0..count {
            let p = table + index * 56;
            let kind = u32_at(file, p)?;
            let offset =
                usize::try_from(u64_at(file, p + 8)?).map_err(|_| "File offset overflow")?;
            let size = usize::try_from(u64_at(file, p + 32)?).map_err(|_| "File size overflow")?;
            span(file, offset, size)?;
            if [3, 7].contains(&kind) {
                return Err(
                    "Interpreter and TLS segments require additional loader support".into(),
                );
            }
            if kind == 0x61000000 && dynlib.replace((offset, size)).is_some() {
                return Err("Duplicate SCE dynamic data segment".into());
            }
            if kind == 2 {
                if dynamic.is_some() || size == 0 || size % 16 != 0 {
                    return Err("Invalid dynamic segment".into());
                }
                dynamic = Some((offset, size));
            }
            if kind == 1 {
                let alignment = u64_at(file, p + 48)?;
                let address = u64_at(file, p + 16)?;
                if alignment > 1
                    && (!alignment.is_power_of_two()
                        || address % alignment != offset as u64 % alignment
                        || bias % alignment != 0)
                {
                    return Err("Invalid segment alignment or load bias".into());
                }
            }
        }
        let mut used = file.len();
        let mut segments = Vec::new();
        for segment in &elf.segments {
            if segment.flags & !7 != 0 {
                return Err("Invalid segment permissions".into());
            }
            let size = usize::try_from(segment.memory_size).map_err(|_| "Segment size overflow")?;
            used = used.checked_add(size).ok_or("Loader budget overflow")?;
            if used > budget {
                return Err("ELF exceeds loader memory budget".into());
            }
            let address = bias
                .checked_add(segment.address)
                .ok_or("Guest load address overflow")?;
            address
                .checked_add(segment.memory_size)
                .ok_or("Guest segment end overflow")?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(size)
                .map_err(|_| "Cannot allocate guest segment")?;
            bytes.resize(size, 0);
            bytes[..segment.file_size].copy_from_slice(span(
                file,
                segment.offset,
                segment.file_size,
            )?);
            segments.push(Segment {
                address,
                flags: segment.flags,
                bytes,
            });
        }
        let entry = bias.checked_add(elf.entry).ok_or("Guest entry overflow")?;
        let file_segments = elf.segments;
        Ok(Self {
            file: file.to_vec(),
            segments,
            bias,
            entry,
            dynamic,
            dynlib,
            file_segments,
            dependencies: vec![],
            unresolved: vec![],
            applied: 0,
            relocations: Vec::new(),
            relocated: false,
            dynamic_tags: BTreeMap::new(),
            export_owners: crate::ownership::Exports::default(),
        })
    }
    pub(crate) fn file_at(&self, address: u64, size: usize) -> Result<&[u8]> {
        for s in &self.file_segments {
            if let Some(delta) = address.checked_sub(s.address) {
                if delta <= s.file_size as u64 && size as u64 <= s.file_size as u64 - delta {
                    return span(&self.file, s.offset + delta as usize, size);
                }
            }
        }
        Err("Metadata outside file-backed segment".into())
    }
    fn location(&self, address: u64, size: usize) -> Result<(usize, usize)> {
        for (index, s) in self.segments.iter().enumerate() {
            if let Some(delta) = address.checked_sub(s.address) {
                if delta <= s.bytes.len() as u64 && size as u64 <= s.bytes.len() as u64 - delta {
                    return Ok((index, delta as usize));
                }
            }
        }
        Err("Relocation outside guest segment".into())
    }
    pub fn relocate(&mut self, mut resolve: impl FnMut(&str) -> Option<u64>) -> Result<()> {
        self.relocate_scoped(|identity| resolve(&identity.name))
    }
    pub fn lifecycle(&self) -> Result<crate::lifecycle::Lifecycle> {
        if !self.relocated || !self.unresolved.is_empty() {
            return Err("Lifecycle requires resolved relocations".into());
        }
        crate::lifecycle::discover(self, &self.dynamic_tags)
    }
    pub fn relocate_scoped(
        &mut self,
        mut resolve: impl FnMut(&crate::imports::Identity) -> Option<u64>,
    ) -> Result<()> {
        if self.relocated {
            return Err("Image was already relocated".into());
        }
        let mut tags = BTreeMap::new();
        let mut needed = Vec::new();
        let mut module_records = Vec::new();
        let mut export_records = Vec::new();
        let mut library_records = Vec::new();
        if let Some((offset, size)) = self.dynamic {
            let mut terminated = false;
            for record in span(&self.file, offset, size)?.chunks_exact(16) {
                let tag = u64_at(record, 0)?;
                let value = u64_at(record, 8)?;
                if tag == 0 {
                    terminated = true;
                    break;
                }
                if tag == 1 {
                    needed.push(value);
                    continue;
                }
                if tag == 0x61000045 {
                    module_records.push(value);
                    continue;
                }
                if tag == 0x61000015 {
                    library_records.push((crate::ownership::EXPORT_LIB, value));
                    continue;
                }
                if [crate::ownership::MODULE_INFO, crate::ownership::EXPORT_LIB].contains(&tag) {
                    export_records.push((tag, value));
                    continue;
                }
                if [17, 18, 19, 35, 36, 37].contains(&tag) {
                    return Err("Unsupported relocation metadata family".into());
                }

                if tags.insert(tag, value).is_some() {
                    return Err("Duplicate dynamic tag".into());
                }
            }
            if !terminated {
                return Err("Unterminated dynamic table".into());
            }
        }
        let pairs = [
            (5, 0x61000035),
            (10, 0x61000037),
            (6, 0x61000039),
            (11, 0x6100003b),
            (7, 0x6100002f),
            (8, 0x61000031),
            (9, 0x61000033),
            (23, 0x61000029),
            (2, 0x6100002d),
            (20, 0x6100002b),
            (3, 0x61000027),
        ];
        for &(standard, sce) in &pairs {
            if tags.contains_key(&standard) && tags.contains_key(&sce) {
                return Err("Ambiguous standard/SCE dynamic tags".into());
            }
        }
        let sce_tag = |tag| {
            pairs
                .iter()
                .find(|(standard, _)| *standard == tag)
                .map(|(_, sce)| *sce)
                .unwrap_or(tag)
        };
        let present = |tag| tags.contains_key(&tag) || tags.contains_key(&sce_tag(tag));
        let get = |tag| {
            tags.get(&tag)
                .or_else(|| tags.get(&sce_tag(tag)))
                .copied()
                .ok_or_else(|| format!("Missing dynamic tag {tag}"))
        };
        let metadata = |tag, offset: u64, size: usize| -> Result<&[u8]> {
            let address = get(tag)?
                .checked_add(offset)
                .ok_or("Dynamic metadata address overflow")?;
            if tags.contains_key(&tag) {
                return self.file_at(address, size);
            }
            let (base, extent) = self.dynlib.ok_or("Missing SCE dynamic data segment")?;
            let offset = usize::try_from(address).map_err(|_| "Metadata offset overflow")?;
            if offset > extent || size > extent - offset {
                return Err("SCE metadata outside declared segment".into());
            }
            span(
                &self.file,
                base.checked_add(offset)
                    .ok_or("Metadata file offset overflow")?,
                size,
            )
        };
        let symbol_size = tags.get(&0x6100003f).copied();
        if tags.contains_key(&0x61000039) {
            let size = symbol_size.ok_or("Missing SCE symbol table size")?;
            if size == 0 || size % 24 != 0 || get(11)? != 24 {
                return Err("Invalid SCE symbol table size".into());
            }
            metadata(
                6,
                0,
                usize::try_from(size).map_err(|_| "Symbol table size overflow")?,
            )?;
        }
        let string = |offset: u64| -> Result<String> {
            let size = get(10)?;
            if offset >= size {
                return Err("Invalid dynamic string offset".into());
            }
            let bytes = metadata(
                5,
                offset,
                usize::try_from(size - offset).map_err(|_| "String size overflow")?,
            )?;
            let end = bytes
                .iter()
                .position(|b| *b == 0)
                .ok_or("Unterminated dynamic string")?;
            String::from_utf8(bytes[..end].to_vec())
                .map_err(|_| "Non-UTF8 import name unsupported".into())
        };
        let dependencies = needed.into_iter().map(string).collect::<Result<Vec<_>>>()?;
        let export_owners = crate::ownership::Exports::parse(&export_records, string)?;
        let import_owners = if library_records.is_empty() {
            None
        } else {
            library_records.extend(
                module_records
                    .iter()
                    .map(|&v| (crate::ownership::MODULE_INFO, v)),
            );
            Some(crate::ownership::Exports::parse(&library_records, string)?)
        };
        let mut modules = BTreeMap::new();
        for record in module_records {
            if modules
                .insert((record >> 48) as u16, string(record & 0xffffffff)?)
                .is_some()
            {
                return Err("Duplicate module ID".into());
            }
        }
        let mut patches = Vec::new();
        let mut unresolved = Vec::new();
        let mut targets = BTreeSet::new();
        for (pointer_tag, size_tag) in [(7, 8), (23, 2)] {
            if !present(pointer_tag) && !present(size_tag) {
                continue;
            }
            get(pointer_tag)?;
            let size = get(size_tag)?;
            if size % 24 != 0
                || (size > 0
                    && ((pointer_tag == 7 && get(9)? != 24)
                        || (pointer_tag == 23 && get(20)? != 7)))
            {
                return Err("Invalid RELA table layout".into());
            }
            let records = metadata(
                pointer_tag,
                0,
                usize::try_from(size).map_err(|_| "RELA size overflow")?,
            )?;
            for record in records.chunks_exact(24) {
                let offset = u64_at(record, 0)?;
                let info = u64_at(record, 8)?;
                let addend = u64_at(record, 16)?;
                let kind = info as u32;
                let symbol = info >> 32;
                if kind == 0 {
                    continue;
                }
                let address = self
                    .bias
                    .checked_add(offset)
                    .ok_or("Relocation address overflow")?;
                let location = self.location(address, 8)?;
                // Reject overlapping writes, not only identical starting addresses.
                for prior in address.saturating_sub(7)..=address {
                    if targets.contains(&prior) {
                        return Err("Overlapping relocation writes".into());
                    }
                }
                if targets
                    .range(address..address.checked_add(8).ok_or("Relocation end overflow")?)
                    .next()
                    .is_some()
                {
                    return Err("Overlapping relocation writes".into());
                }
                targets.insert(address);
                let value = if kind == 8 {
                    if symbol != 0 {
                        return Err("RELATIVE relocation has a symbol".into());
                    }
                    self.bias.wrapping_add(addend)
                } else {
                    if ![1, 6, 7].contains(&kind) {
                        return Err(format!("Unsupported relocation type {kind}"));
                    }
                    if get(11)? != 24 {
                        return Err("Unsupported symbol entry size".into());
                    }
                    let symbol_offset = symbol * 24;
                    if symbol_size
                        .is_some_and(|size| symbol_offset > size || 24 > size - symbol_offset)
                    {
                        return Err("Symbol index outside declared table".into());
                    }
                    let sym = metadata(6, symbol_offset, 24)?;

                    if [6, 10].contains(&(sym[4] & 15)) {
                        return Err("TLS and IFUNC symbols unsupported".into());
                    }
                    let section = u16::from_le_bytes(sym[6..8].try_into().unwrap());
                    let value = match section {
                        0 => {
                            let name = string(u32_at(sym, 0)? as u64)?;
                            let mut identity =
                                crate::imports::identify(&name, &modules, &dependencies)?;
                            if let Some(owners) = &import_owners {
                                if let Some(scope) = owners.identify(&name)? {
                                    identity.library = Some(scope.library);
                                    identity.module_version =
                                        Some((scope.module.major, scope.module.minor));
                                }
                            }
                            match resolve(&identity) {
                                Some(value) => value,
                                None => {
                                    unresolved.push(Import {
                                        name,
                                        address,
                                        kind,
                                        identity,
                                    });
                                    continue;
                                }
                            }
                        }
                        0xfff1 => u64_at(sym, 8)?,
                        0xff00.. => return Err("Unsupported special symbol section".into()),
                        _ => {
                            let address = self
                                .bias
                                .checked_add(u64_at(sym, 8)?)
                                .ok_or("Defined symbol overflow")?;
                            self.location(address, 0)?;
                            address
                        }
                    };
                    value.wrapping_add(if kind == 1 { addend } else { 0 })
                };
                patches.push((location, value));
            }
        }
        let relocations = patches
            .iter()
            .map(|&((segment, offset), value)| {
                (self.segments[segment].address + offset as u64, value)
            })
            .collect();
        for &((segment, offset), value) in &patches {
            self.segments[segment].bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        self.applied = patches.len();
        self.relocations = relocations;
        self.unresolved = unresolved;
        self.dependencies = dependencies;
        self.relocated = true;
        self.dynamic_tags = tags;
        self.export_owners = export_owners;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn put(b: &mut [u8], p: usize, v: u64) {
        b[p..p + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn fixture() -> Vec<u8> {
        let mut b = vec![0; 0x400];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        for (p, v) in [(16, 3u16), (18, 62), (52, 64), (54, 56), (56, 2)] {
            b[p..p + 2].copy_from_slice(&v.to_le_bytes());
        }
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        for (p, v) in [
            (24, 0x100),
            (32, 64),
            (64, 0x700000001),
            (72, 0),
            (80, 0),
            (96, 0x400),
            (104, 0x480),
            (112, 1),
            (120, 2),
            (128, 0x180),
            (136, 0x180),
            (152, 144),
            (160, 144),
            (168, 8),
        ] {
            put(&mut b, p, v);
        }
        b[0x100] = 0xc3;
        for (i, (tag, value)) in [
            (7, 0x280),
            (8, 48),
            (9, 24),
            (5, 0x220),
            (10, 16),
            (6, 0x240),
            (11, 24),
            (1, 1),
            (0, 0),
        ]
        .iter()
        .enumerate()
        {
            put(&mut b, 0x180 + i * 16, *tag);
            put(&mut b, 0x188 + i * 16, *value);
        }
        b[0x220..0x230].copy_from_slice(b"\0libtest\0symbol\0");
        b[0x240..0x244].copy_from_slice(&9u32.to_le_bytes());
        for (p, v) in [
            (0x280, 0x300),
            (0x288, 8),
            (0x290, 0x40),
            (0x298, 0x308),
            (0x2a0, 1),
            (0x2a8, u64::MAX - 3),
        ] {
            put(&mut b, p, v);
        }
        b
    }
    #[test]
    fn maps_bss_dependencies_and_signed_relocations() {
        let b = fixture();
        let mut image = Image::load(&b, 0x10000, 65536).unwrap();
        assert_eq!(image.entry, 0x10100);
        assert!(image.segments[0].bytes[0x400..].iter().all(|b| *b == 0));
        image
            .relocate(|name| {
                assert_eq!(name, "symbol");
                Some(0x90000)
            })
            .unwrap();
        assert_eq!(u64_at(&image.segments[0].bytes, 0x300).unwrap(), 0x10040);
        assert_eq!(u64_at(&image.segments[0].bytes, 0x308).unwrap(), 0x8fffc);
        assert_eq!(image.dependencies, ["libtest"]);
        assert_eq!(image.applied, 2);
        assert!(image.unresolved.is_empty());
        assert!(image.relocate(|_| None).is_err());
        assert_eq!(b, fixture());
    }
    #[test]
    #[allow(unsafe_code)]
    fn ffi_ownership_zero_resolution_and_callback_failure() {
        use crate::loader_ffi::*;
        use std::ffi::c_void;
        unsafe extern "C" fn resolver(context: *mut c_void, identity: Identity) -> Resolution {
            // SAFETY: test supplies live context and build supplies a live name slice.
            let name = identity.name;
            let state = unsafe { &mut *context.cast::<(u64, u64)>() };
            assert_eq!(
                unsafe { std::slice::from_raw_parts(name.data, name.size as usize) },
                b"symbol"
            );
            state.1 += 1;
            Resolution {
                value: 0,
                status: state.0,
            }
        }
        let b = fixture();
        let mut state = (2u64, 0u64);
        // SAFETY: input and exclusive callback context outlive each synchronous call;
        // all returned records are read before the single matching destruction.
        unsafe {
            let call = |state: &mut (u64, u64)| {
                native_ps5_image_build_v3(
                    b.as_ptr(),
                    b.len() as u64,
                    0x10000,
                    65536,
                    (state as *mut (u64, u64)).cast(),
                    Some(resolver),
                )
            };
            assert!(call(&mut state).is_null());
            assert_eq!(state.1, 1);
            state.0 = 0;
            let image = call(&mut state);
            assert!(!image.is_null());
            assert_eq!(native_ps5_image_count_v3(image, 0), 2);
            assert_eq!(native_ps5_image_count_v3(image, 1), 0);
            assert_eq!(native_ps5_image_count_v3(image, 2), 1);
            let patch = native_ps5_image_patch_v3(image, 1);
            assert_eq!((patch.address, patch.value), (0x10308, u64::MAX - 3));
            let dependency = native_ps5_image_dependency_v3(image, 0);
            assert_eq!(
                std::slice::from_raw_parts(dependency.data, dependency.size as usize),
                b"libtest"
            );
            native_ps5_image_delete_v3(image);
            state.0 = 1;
            let image = call(&mut state);
            assert!(!image.is_null());
            let entry = native_ps5_image_import_v3(image, 0);
            assert_eq!((entry.address, entry.kind), (0x10308, 1));
            assert_eq!(
                std::slice::from_raw_parts(
                    entry.identity.name.data,
                    entry.identity.name.size as usize
                ),
                b"symbol"
            );
            native_ps5_image_delete_v3(image);
        }
        assert_eq!(b, fixture());
    }
    #[test]
    fn symbol_relocation_kinds_and_defined_symbols() {
        for kind in [1, 6, 7] {
            for section in [0u16, 1, 0xfff1] {
                let mut b = fixture();
                put(&mut b, 0x2a0, kind);
                b[0x246..0x248].copy_from_slice(&section.to_le_bytes());
                put(&mut b, 0x248, 0x100);
                let mut image = Image::load(&b, 0x10000, 65536).unwrap();
                image.relocate(|_| Some(0x90000)).unwrap();
                let base: u64 = match section {
                    0 => 0x90000,
                    1 => 0x10100,
                    _ => 0x100,
                };
                assert_eq!(
                    u64_at(&image.segments[0].bytes, 0x308).unwrap(),
                    base.wrapping_sub(if kind == 1 { 4 } else { 0 })
                );
            }
        }
    }
    #[test]
    fn retains_missing_imports_without_guessing_addresses() {
        let mut image = Image::load(&fixture(), 0x10000, 65536).unwrap();
        image.relocate(|_| None).unwrap();
        assert_eq!(image.applied, 1);
        assert_eq!(
            image.unresolved,
            [Import {
                name: "symbol".into(),
                address: 0x10308,
                kind: 1,
                identity: crate::imports::identify("symbol", &BTreeMap::new(), &[]).unwrap()
            }]
        );
        assert_eq!(u64_at(&image.segments[0].bytes, 0x308).unwrap(), 0);
    }
    #[test]
    fn malformed_late_relocation_never_commits_earlier_patch() {
        for (offset, value) in [
            (0x298, 0x304),
            (0x298, 0x47c),
            (0x2a0, 37),
            (0x288, 0x100000008),
            (0x1a8, 23),
            (0x1b8, 0x9000),
        ] {
            let mut b = fixture();
            put(&mut b, offset, value);
            let mut image = Image::load(&b, 0x10000, 65536).unwrap();
            let before = image.segments[0].bytes.clone();
            assert!(image.relocate(|_| Some(1)).is_err(), "offset {offset:x}");
            assert_eq!(image.segments[0].bytes, before);
            assert_eq!(image.applied, 0);
            assert!(image.dependencies.is_empty());
        }
    }
    #[test]
    fn validates_header_budget_alignment_and_guest_overflow() {
        let b = fixture();
        assert!(Image::load(&b, 0, 1024).is_err());
        assert!(Image::load(&b, u64::MAX - 16, 65536).is_err());
        for (p, v) in [(112, 3), (120, 7), (64, 0x800000001), (32, 0), (152, 143)] {
            let mut b = fixture();
            put(&mut b, p, v);
            assert!(Image::load(&b, 0x10000, 65536).is_err(), "offset {p}");
        }
        for end in 0..b.len() {
            assert!(Image::load(&b[..end], 0, 65536).is_err());
        }
    }
}
