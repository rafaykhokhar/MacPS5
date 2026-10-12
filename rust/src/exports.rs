//! Bounded discovery of defined, externally visible dynamic symbols.
use crate::{Result, loader::Image};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    /// Resolved producer-local ownership; the original qualified name is retained.
    pub scope: Option<crate::ownership::Scope>,
    pub address: u64,
    pub size: u64,
    pub function: bool,
    pub weak: bool,
}
fn word<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N]> {
    bytes
        .get(offset..offset.checked_add(N).ok_or("Metadata overflow")?)
        .ok_or("Truncated export metadata")?
        .try_into()
        .map_err(|_| "Invalid metadata word".into())
}
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(word(bytes, offset)?))
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(word(bytes, offset)?))
}
impl Image {
    pub fn exported_symbols(&self) -> Result<Vec<Export>> {
        if !self.relocated {
            return Err("Export discovery requires relocation metadata".into());
        }
        let tags = &self.dynamic_tags;
        let get = |standard, sce| {
            tags.get(&standard)
                .or_else(|| tags.get(&sce))
                .copied()
                .ok_or("Missing export table metadata")
        };
        if !tags.contains_key(&6) && !tags.contains_key(&0x61000039) {
            return Ok(vec![]);
        }
        if get(11, 0x6100003b)? != 24 {
            return Err("Unsupported dynamic symbol size".into());
        }
        let metadata = |standard, sce, size: usize| -> Result<&[u8]> {
            let address = get(standard, sce)?;
            if tags.contains_key(&standard) {
                return self.file_at(address, size);
            }
            let (base, extent) = self.dynlib.ok_or("Missing SCE dynamic data")?;
            let offset = usize::try_from(address).map_err(|_| "SCE table offset overflow")?;
            if offset > extent || size > extent - offset {
                return Err("SCE export table exceeds metadata segment".into());
            }
            self.file
                .get(base + offset..base + offset + size)
                .ok_or_else(|| "SCE export file bounds".into())
        };
        let size = if let Some(&size) = tags.get(&0x6100003f) {
            size
        } else if let Some(&address) = tags.get(&4) {
            let header = self.file_at(address, 8)?;
            let buckets = u32_at(header, 0)? as u64;
            let count = u32_at(header, 4)? as u64;
            if buckets == 0 || count == 0 {
                return Err("Empty SysV dynamic hash table".into());
            }
            self.file_at(
                address,
                usize::try_from(8 + (buckets + count) * 4)
                    .map_err(|_| "Hash table extent overflow")?,
            )?;
            count * 24
        } else {
            // Section headers provide an explicit bound when DT_HASH/SCE size is absent.
            let offset = usize::try_from(u64_at(&self.file, 40)?)
                .map_err(|_| "Section table offset overflow")?;
            let count = u16::from_le_bytes(word(&self.file, 60)?) as usize;
            if offset == 0 || count == 0 || u16::from_le_bytes(word(&self.file, 58)?) != 64 {
                return Err(
                    "Dynamic symbol count unavailable (GNU-hash-only images unsupported)".into(),
                );
            }
            let table = self
                .file
                .get(
                    offset
                        ..offset
                            .checked_add(count * 64)
                            .ok_or("Section table overflow")?,
                )
                .ok_or("Section table outside file")?;
            let mut result = None;
            for section in table.chunks_exact(64) {
                if u32_at(section, 4)? != 11 || u64_at(section, 16)? != get(6, 0x61000039)? {
                    continue;
                }
                if result.is_some() || u64_at(section, 56)? != 24 {
                    return Err("Ambiguous or invalid dynamic symbol section".into());
                }
                let size = u64_at(section, 32)?;
                let file_offset = usize::try_from(u64_at(section, 24)?)
                    .map_err(|_| "Symbol section offset overflow")?;
                let length = usize::try_from(size).map_err(|_| "Symbol section size overflow")?;
                let bytes = self
                    .file
                    .get(
                        file_offset
                            ..file_offset
                                .checked_add(length)
                                .ok_or("Symbol section overflow")?,
                    )
                    .ok_or("Symbol section outside file")?;
                if bytes != self.file_at(get(6, 0x61000039)?, length)? {
                    return Err("Dynamic symbol section disagrees with load mapping".into());
                }
                result = Some(size);
            }
            result.ok_or("No bounded dynamic symbol section")?
        };
        if size == 0 || size % 24 != 0 || size / 24 > 1_000_000 {
            return Err("Invalid export symbol table bound".into());
        }
        let symbols = metadata(6, 0x61000039, size as usize)?;
        let strings = metadata(
            5,
            0x61000035,
            usize::try_from(get(10, 0x61000037)?).map_err(|_| "Export string size overflow")?,
        )?;
        let mut exports = Vec::new();
        for sym in symbols.chunks_exact(24) {
            let binding = sym[4] >> 4;
            let kind = sym[4] & 15;
            let visibility = sym[5] & 3;
            let section = u16::from_le_bytes(sym[6..8].try_into().unwrap());
            if section == 0 || binding == 0 || visibility == 1 || visibility == 2 {
                continue;
            }
            if ![1, 2].contains(&binding) || ![0, 1, 2].contains(&kind) || section >= 0xff00 {
                return Err("Unsupported dynamic export kind, binding or section".into());
            }
            let offset = u32_at(sym, 0)? as usize;
            let bytes = strings
                .get(offset..)
                .ok_or("Export name outside string table")?;
            let length = bytes
                .iter()
                .position(|b| *b == 0)
                .ok_or("Unterminated export name")?;
            let name = std::str::from_utf8(&bytes[..length])
                .map_err(|_| "Non-UTF8 export name unsupported")?;
            if name.is_empty() {
                return Err("Empty export name".into());
            }
            let scope = self.export_owners.identify(name)?;
            let address = self
                .bias
                .checked_add(u64_at(sym, 8)?)
                .ok_or("Export address overflow")?;
            let size = u64_at(sym, 16)?;
            let extent = size.max(1);
            if !self.segments.iter().any(|s| {
                address >= s.address
                    && address - s.address < s.bytes.len() as u64
                    && extent <= s.bytes.len() as u64 - (address - s.address)
                    && (kind != 2 || s.flags & 1 != 0)
            }) {
                return Err("Export extent or function permissions invalid".into());
            }
            exports.push(Export {
                name: name.into(),
                scope,
                address,
                size,
                function: kind == 2,
                weak: binding == 2,
            });
        }
        Ok(exports)
    }
}

/// Synthetic symbol-table fixture used by Rust/native loader checks.
pub fn fixture(sce: bool) -> Vec<u8> {
    let mut b = crate::modules::lifecycle_fixture(sce);
    b.resize(0x600, 0);
    fn put(b: &mut [u8], p: usize, v: u64) {
        b[p..p + 8].copy_from_slice(&v.to_le_bytes());
    }
    b.copy_within(0x180..0x220, 0x300);
    put(&mut b, 128, 0x300);
    put(&mut b, 136, 0x300);
    put(&mut b, 152, 240);
    put(&mut b, 160, 240);
    put(&mut b, 96, 0x600);
    put(&mut b, 104, 0x600);
    let strings = b"\0entry\0object\0";
    b[0x450..0x450 + strings.len()].copy_from_slice(strings);
    for (i, name, kind, value, size) in [(1, 1u32, 0x12u8, 0x100, 3), (2, 7, 0x11, 0x500, 8)] {
        let p = 0x400 + i * 24;
        b[p..p + 4].copy_from_slice(&name.to_le_bytes());
        b[p + 4] = kind;
        b[p + 6..p + 8].copy_from_slice(&1u16.to_le_bytes());
        put(&mut b, p + 8, value);
        put(&mut b, p + 16, size);
    }
    for (i, value) in [1u32, 3, 1, 0, 2, 0].iter().enumerate() {
        b[0x480 + i * 4..0x484 + i * 4].copy_from_slice(&value.to_le_bytes());
    }
    let tags = if sce {
        [
            (0x61000039, 0),
            (0x6100003b, 24),
            (0x61000035, 0x50),
            (0x61000037, strings.len() as u64),
            (0x6100003f, 72),
            (0, 0),
        ]
    } else {
        [
            (6, 0x400),
            (11, 24),
            (5, 0x450),
            (10, strings.len() as u64),
            (4, 0x480),
            (0, 0),
        ]
    };
    for (i, (tag, value)) in tags.iter().enumerate() {
        put(&mut b, 0x390 + i * 16, *tag);
        put(&mut b, 0x398 + i * 16, *value);
    }
    if sce {
        b[56..58].copy_from_slice(&3u16.to_le_bytes());
        for (i, value) in [0x461000000, 0x400, 0, 0, 0x100, 0x100, 8]
            .iter()
            .enumerate()
        {
            put(&mut b, 176 + i * 8, *value);
        }
    }
    b
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        imports::identify,
        modules::{EntryPoints, Modules},
    };
    use std::collections::BTreeMap;
    fn image(bytes: &[u8]) -> Image {
        let mut image = Image::load(bytes, 0x10000, 65536).unwrap();
        image.relocate(|_| None).unwrap();
        image
    }
    pub(crate) fn qualified_fixture(sce: bool) -> Vec<u8> {
        let mut b = fixture(sce);
        b.resize(0x900, 0);
        let put = |b: &mut [u8], offset: usize, value: u64| {
            b[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        };
        b.copy_within(0x300..0x3e0, 0x600);
        for (offset, value) in [
            (128, 0x600),
            (136, 0x600),
            (152, 288),
            (160, 288),
            (96, 0x900),
            (104, 0x900),
        ] {
            put(&mut b, offset, value);
        }
        let strings = b"\0entry#B#C\0object\0owner\0libOne\0libTwo\0";
        b[0x780..0x780 + strings.len()].copy_from_slice(strings);
        b[0x430..0x434].copy_from_slice(&11u32.to_le_bytes());
        put(&mut b, 0x6b8, if sce { 0x380 } else { 0x780 });
        put(&mut b, 0x6c8, strings.len() as u64);
        for (i, (tag, value)) in [
            (
                crate::ownership::MODULE_INFO,
                (2 << 48) | (3 << 40) | (4 << 32) | 18,
            ),
            (crate::ownership::EXPORT_LIB, (1 << 48) | (7 << 32) | 24),
            (crate::ownership::EXPORT_LIB, (9 << 48) | (8 << 32) | 31),
            (0, 0),
        ]
        .into_iter()
        .enumerate()
        {
            put(&mut b, 0x6e0 + i * 16, tag);
            put(&mut b, 0x6e8 + i * 16, value);
        }
        if sce {
            put(&mut b, 208, 0x500);
            put(&mut b, 216, 0x500);
        }
        b
    }
    #[test]
    fn qualified_exports_resolve_local_owners_without_publishing_unscoped_aliases() {
        for sce in [false, true] {
            let bytes = qualified_fixture(sce);
            let exports = image(&bytes).exported_symbols().unwrap();
            assert_eq!(exports.len(), 2);
            assert_eq!(exports[0].name, "entry#B#C");
            let scope = exports[0].scope.as_ref().unwrap();
            assert_eq!(scope.symbol, "entry");
            assert_eq!(scope.module.name, "owner");
            assert_eq!((scope.module.major, scope.module.minor), (3, 4));
            assert_eq!(scope.library.name, "libOne");
            assert_eq!(scope.library.version, 7);
            assert!(exports[1].scope.is_none());
            let mut modules = Modules::new(65536);
            let handle = modules
                .load_dynamic("owner.prx", &bytes, 0x10000, EntryPoints::default())
                .unwrap();
            let mut identity =
                identify("entry#Z#H", &BTreeMap::from([(7, "owner".into())]), &[]).unwrap();
            identity.library = Some(scope.library.clone());
            identity.module_version = Some((3, 4));
            assert_eq!(modules.resolve(&identity), Some(0x10100));
            assert_eq!(
                modules.resolve(&identify("entry", &BTreeMap::new(), &[]).unwrap()),
                None
            );
            modules.unload(handle).unwrap();
            assert_eq!(modules.resolve(&identity), None);
            assert_eq!(modules.bytes(), 0);
        }
    }
    #[test]
    fn malformed_owner_records_fail_before_relocation_writes() {
        for sce in [false, true] {
            for (offset, value) in [
                (0x708, (1u64 << 48) | 31),          // Duplicate library ID.
                (0x6e8, (2u64 << 48) | 0xffff_ffff), // Invalid module name offset.
                (0x6f8, 1u64 << 48),                 // Empty library name.
            ] {
                let mut b = qualified_fixture(sce);
                b[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                let mut image = Image::load(&b, 0x10000, 65536).unwrap();
                let before: Vec<_> = image.segments.iter().map(|s| s.bytes.clone()).collect();
                assert!(image.relocate(|_| None).is_err());
                assert_eq!(image.applied, 0);
                assert!(image.exported_symbols().is_err());
                assert_eq!(
                    before,
                    image
                        .segments
                        .iter()
                        .map(|s| s.bytes.clone())
                        .collect::<Vec<_>>()
                );
            }
            let mut b = qualified_fixture(sce);
            b[0x787] = b'K'; // Valid token but undeclared export library ID.
            assert!(image(&b).exported_symbols().is_err());
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn producer_consumer_local_ids_link_by_library_version_and_pin_owner() {
        for sce in [false, true] {
            let producer = qualified_fixture(sce);
            let mut consumer = qualified_fixture(sce);
            consumer[0x787] = b'J'; // Consumer library 9, producer library 1.
            consumer[0x789] = b'H'; // Consumer module 7, producer module 2.
            consumer[0x41e..0x420].fill(0); // Undefined imported function.
            for (p, value) in [
                (0x6e0, 0x61000045u64),
                (0x6e8, (7 << 48) | (3 << 40) | (4 << 32) | 18),
                (0x6f0, 0x61000015),
                (0x6f8, (9 << 48) | (7 << 32) | 24),
                (0x700, 0x61000015),
                (0x708, (1 << 48) | (8 << 32) | 31),
                (0x678, 72), // Third RELA record.
                (0x2b0, 0x500),
                (0x2b8, (1 << 32) | 6),
                (0x2c0, 0),
            ] {
                consumer[p..p + 8].copy_from_slice(&value.to_le_bytes());
            }
            let mut modules = Modules::new(65536);
            // Exercise the exact callback/registry ABI used by the native bridge.
            unsafe extern "C" fn scoped_resolve(
                context: *mut std::ffi::c_void,
                identity: crate::loader_ffi::Identity,
            ) -> crate::loader_ffi::Resolution {
                unsafe {
                    crate::registry::ffi::native_ps5_registry_resolve_v2(context.cast(), identity)
                }
            }
            let mut registry = crate::registry::Registry::default();
            registry
                .register_image("owner.prx", &image(&producer).exported_symbols().unwrap())
                .unwrap();
            let foreign = unsafe {
                crate::loader_ffi::native_ps5_image_build_v3(
                    consumer.as_ptr(),
                    consumer.len() as u64,
                    0x20000,
                    65536,
                    (&mut registry as *mut crate::registry::Registry).cast(),
                    Some(scoped_resolve),
                )
            };
            assert!(!foreign.is_null());
            unsafe {
                assert_eq!(crate::loader_ffi::native_ps5_image_count_v3(foreign, 1), 0);
                let patch = crate::loader_ffi::native_ps5_image_patch_v3(foreign, 2);
                assert_eq!((patch.address, patch.value), (0x20500, 0x10100));
                crate::loader_ffi::native_ps5_image_delete_v3(foreign);
            }
            registry.remove_module("owner.prx");
            let missing = unsafe {
                crate::loader_ffi::native_ps5_image_build_v3(
                    consumer.as_ptr(),
                    consumer.len() as u64,
                    0x20000,
                    65536,
                    (&mut registry as *mut crate::registry::Registry).cast(),
                    Some(scoped_resolve),
                )
            };
            assert!(!missing.is_null());
            unsafe {
                assert_eq!(crate::loader_ffi::native_ps5_image_count_v3(missing, 1), 1);
                let record = crate::loader_ffi::native_ps5_image_import_v3(missing, 0);
                assert_eq!(record.identity.module_id, 7);
                assert_eq!(record.identity.module_version, 0x304);
                assert_eq!(record.identity.library_version, 7);
                assert_eq!(
                    std::slice::from_raw_parts(
                        record.identity.library.data,
                        record.identity.library.size as usize
                    ),
                    b"libOne"
                );
                crate::loader_ffi::native_ps5_image_delete_v3(missing);
            }
            let provider = modules
                .load_dynamic("owner.prx", &producer, 0x10000, EntryPoints::default())
                .unwrap();
            let baseline = modules.bytes();
            // Matching local numeric IDs do not authorize a different library/version.
            for (p, value) in [
                (0x6f8, (9u64 << 48) | (8 << 32) | 24),
                (0x6f8, (9 << 48) | (7 << 32) | 31),
                (0x6e8, (7 << 48) | (3 << 40) | (5 << 32) | 18),
            ] {
                let mut bad = consumer.clone();
                bad[p..p + 8].copy_from_slice(&value.to_le_bytes());
                assert!(
                    modules
                        .load_dynamic("consumer.prx", &bad, 0x20000, EntryPoints::default())
                        .is_err()
                );
                assert_eq!(modules.bytes(), baseline);
            }
            let client = modules
                .load_dynamic("consumer.prx", &consumer, 0x20000, EntryPoints::default())
                .unwrap();
            assert!(
                modules
                    .image(client)
                    .unwrap()
                    .relocations
                    .contains(&(0x20500, 0x10100))
            );
            assert!(modules.unload(provider).is_err());
            modules.unload(client).unwrap();
            modules.unload(provider).unwrap();
            assert_eq!(modules.bytes(), 0);
        }
    }
    #[test]
    fn qualified_registration_conflicts_roll_back_entire_batch() {
        let mut exports = image(&qualified_fixture(false)).exported_symbols().unwrap();
        let mut registry = crate::registry::Registry::default();
        registry.register_image("owner.prx", &exports).unwrap();
        let mut identity =
            identify("entry#B#C", &BTreeMap::from([(2, "owner".into())]), &[]).unwrap();
        identity.library = Some(exports[0].scope.as_ref().unwrap().library.clone());
        identity.module_version = Some((3, 4));
        exports[1].name = "new_plain".into();
        exports[0].address += 8;
        assert!(registry.register_image("owner.prx", &exports).is_err());
        assert_eq!(
            registry.resolve(&identify("new_plain", &BTreeMap::new(), &[]).unwrap()),
            None
        );
        assert_eq!(registry.resolve(&identity), Some(0x10100));
        assert!(registry.register_image("impostor.prx", &exports).is_err());
        identity.library.as_mut().unwrap().name = "different".into();
        registry.register("owner.prx", "entry", 99).unwrap();
        assert_eq!(registry.resolve(&identity), None); // No fallback to plain/native export.
    }
    #[test]
    fn standard_and_sce_tables_discover_functions_and_objects() {
        for sce in [false, true] {
            let exports = image(&fixture(sce)).exported_symbols().unwrap();
            assert_eq!(
                exports,
                [
                    Export {
                        name: "entry".into(),
                        scope: None,
                        address: 0x10100,
                        size: 3,
                        function: true,
                        weak: false
                    },
                    Export {
                        name: "object".into(),
                        scope: None,
                        address: 0x10500,
                        size: 8,
                        function: false,
                        weak: false
                    }
                ]
            );
        }
    }
    #[test]
    fn filters_local_hidden_undefined_and_retains_weak() {
        let mut b = fixture(false);
        b[0x41c] = 2;
        b[0x435] = 2;
        assert!(image(&b).exported_symbols().unwrap().is_empty());
        b[0x41c] = 0x22;
        b[0x435] = 0;
        b[0x436..0x438].copy_from_slice(&0u16.to_le_bytes());
        let exports = image(&b).exported_symbols().unwrap();
        assert_eq!(exports.len(), 1);
        assert!(exports[0].weak);
    }
    #[test]
    fn invalid_metadata_cannot_publish_exports() {
        for (p, value) in [
            (0x398, u64::MAX),
            (0x3c8, u64::MAX),
            (0x420, u64::MAX),
            (0x440, u64::MAX),
        ] {
            let mut b = fixture(false);
            b[p..p + 8].copy_from_slice(&value.to_le_bytes());
            let mut modules = Modules::new(65536);
            assert!(
                modules
                    .load_dynamic("bad.prx", &b, 0x10000, EntryPoints::default())
                    .is_err(),
                "offset {p:x}"
            );
            assert_eq!(modules.bytes(), 0);
        }
        let mut b = fixture(false);
        b[0x418..0x41c].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(image(&b).exported_symbols().is_err());
        let mut b = fixture(false);
        b[0x41c] = 0x1a;
        assert!(image(&b).exported_symbols().is_err());
        let mut b = fixture(false);
        b[0x451] = b'#';
        assert!(image(&b).exported_symbols().is_err());
    }
    #[test]
    fn section_headers_bound_standard_tables_without_hash() {
        let mut b = fixture(false);
        b[0x3d0..0x3d8].copy_from_slice(&21u64.to_le_bytes());
        assert!(image(&b).exported_symbols().is_err());
        b[40..48].copy_from_slice(&0x520u64.to_le_bytes());
        b[58..60].copy_from_slice(&64u16.to_le_bytes());
        b[60..62].copy_from_slice(&1u16.to_le_bytes());
        b[0x524..0x528].copy_from_slice(&11u32.to_le_bytes());
        for (p, value) in [(0x530, 0x400u64), (0x538, 0x400), (0x540, 72), (0x558, 24)] {
            b[p..p + 8].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(image(&b).exported_symbols().unwrap().len(), 2);
        b[0x540..0x548].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(image(&b).exported_symbols().is_err());
    }
    #[test]
    fn module_registration_uses_discovered_exports_and_removes_aliases() {
        for sce in [false, true] {
            let mut modules = Modules::new(65536);
            let id = modules
                .load_dynamic("auto.prx", &fixture(sce), 0x10000, EntryPoints::default())
                .unwrap();
            assert_eq!(
                modules.resolve(&identify("entry", &BTreeMap::new(), &[]).unwrap()),
                Some(0x10100)
            );
            let nid = crate::strings::nid("object");
            let identity = identify(
                std::str::from_utf8(&nid[..11]).unwrap(),
                &BTreeMap::new(),
                &[],
            )
            .unwrap();
            assert_eq!(modules.resolve(&identity), Some(0x10500));
            modules.unload(id).unwrap();
            assert_eq!(modules.resolve(&identity), None);
            assert_eq!(modules.bytes(), 0);
        }
    }
}
