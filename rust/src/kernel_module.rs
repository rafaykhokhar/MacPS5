//! Kernel module bytes observed by this homebrew's own dynamic resolver.
//! Function and data exports; the caller validates function addresses against live code.
use crate::Result;
#[derive(Clone, Copy)]
pub enum Kind {
    Function,
    /// One virtual callable address dispatched to a native service. The symbol
    /// extent is one address, not the size of host or guest machine code.
    ServiceEntry,
    Object {
        size: u64,
        writable: bool,
    },
}
pub struct Export<'a> {
    pub nid: &'a str,
    pub address: u64,
    pub kind: Kind,
}
pub struct ModuleRecord {
    pub bytes: Vec<u8>,
}
impl ModuleRecord {
    /// Publish the already-loaded provider's guest pathname for the SDK's
    /// `kernel_dynlib_handle` lookup. Its resolver copies exactly 1024 bytes
    /// from record+8, so the backing region includes zero padding, not just a
    /// short C string. This describes a loaded module; it creates no guest file.
    pub fn with_path(mut self, address: u64, path: &str) -> Result<Self> {
        if !crate::module_files::valid_path(path) || path.len() >= 1024 {
            return Err("Invalid kernel module pathname".into());
        }
        if self.bytes.len() < 0x2a0
            || address == 0
            || address % 16 != 0
            || address.checked_add(0x180)
                != Some(u64::from_le_bytes(
                    self.bytes[0x148..0x150].try_into().unwrap(),
                ))
            || self.bytes[8..16] != [0; 8]
        {
            return Err("Kernel module pathname address mismatch or already named".into());
        }
        let offset = self.bytes.len();
        let length = offset
            .checked_add(1024)
            .ok_or("Kernel pathname size overflow")?;
        address
            .checked_add(length as u64)
            .ok_or("Kernel pathname address overflow")?;
        let pointer = address + offset as u64;
        self.bytes.resize(length, 0);
        self.bytes[offset..offset + path.len()].copy_from_slice(path.as_bytes());
        self.bytes[8..16].copy_from_slice(&pointer.to_le_bytes());
        Ok(self)
    }
    pub fn functions(
        address: u64,
        next: u64,
        handle: u32,
        base: u64,
        exports: &[(&str, u64)],
        callable: impl FnMut(u64) -> bool,
    ) -> Result<Self> {
        if exports.is_empty() || exports.len() > 128 {
            return Err("Invalid kernel export count".into());
        }
        let entries: Vec<_> = exports
            .iter()
            .map(|&(nid, address)| Export {
                nid,
                address,
                kind: Kind::Function,
            })
            .collect();
        Self::build(
            address,
            next,
            handle,
            base,
            &entries,
            &crate::guest_memory::Memory::default(),
            callable,
        )
    }
    pub fn build(
        address: u64,
        next: u64,
        handle: u32,
        base: u64,
        exports: &[Export<'_>],
        user: &crate::guest_memory::Memory<'_>,
        mut callable: impl FnMut(u64) -> bool,
    ) -> Result<Self> {
        if address == 0
            || address % 16 != 0
            || handle == 0
            || exports.is_empty()
            || exports.len() > 128
        {
            return Err("Invalid kernel module record".into());
        }
        let symbols = 0x2a0;
        let symbol_bytes = (exports.len() + 1) * 24;
        let strings = symbols + symbol_bytes;
        let string_bytes = 1 + exports.len() * 12;
        address
            .checked_add((strings + string_bytes) as u64)
            .ok_or("Kernel module address overflow")?;
        let mut seen = std::collections::BTreeSet::new();
        for export in exports {
            let (nid, target) = (export.nid, export.address);
            if nid.len() != 11
                || !nid
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-')
                || !seen.insert(nid)
                || target <= base
            {
                return Err("Invalid or duplicate kernel export".into());
            }
            match export.kind {
                Kind::ServiceEntry if base == 0 || target == u64::MAX => {
                    return Err("Invalid native service entry base or extent".into());
                }
                Kind::Function | Kind::ServiceEntry if !callable(target) => {
                    return Err("Noncallable kernel export".into());
                }
                Kind::Object { size, writable } => {
                    if size == 0
                        || size > 16 * 1024 * 1024
                        || user.executable(target)
                        || callable(target)
                    {
                        return Err("Invalid data export".into());
                    }
                    user.read(target, size as usize)?;
                    if writable {
                        user.validate_write(target, size as usize)?;
                    }
                }
                _ => {}
            }
        }
        let mut bytes = vec![0; strings + string_bytes];
        let mut put = |offset: usize, value: u64| {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes())
        };
        for (offset, value) in [
            (0, next),
            (0x28, handle.into()),
            (0x30, base),
            (0x148, address + 0x180),
            (0x1a8, address + symbols as u64),
            (0x1b0, symbol_bytes as u64),
            (0x1b8, address + strings as u64),
            (0x1c0, string_bytes as u64),
        ] {
            put(offset, value);
        }
        for (i, export) in exports.iter().enumerate() {
            let (nid, target) = (export.nid, export.address);
            let symbol = symbols + (i + 1) * 24;
            let name = 1 + i * 12;
            bytes[symbol..symbol + 4].copy_from_slice(&(name as u32).to_le_bytes());
            let (info, size) = match export.kind {
                Kind::Function => (0x12, 0),
                Kind::ServiceEntry => (0x12, 1),
                Kind::Object { size, .. } => (0x11, size),
            };
            bytes[symbol + 4] = info;
            bytes[symbol + 6] = 1;
            bytes[symbol + 8..symbol + 16].copy_from_slice(&(target - base).to_le_bytes());
            bytes[symbol + 16..symbol + 24].copy_from_slice(&size.to_le_bytes());
            bytes[strings + name..strings + name + 11].copy_from_slice(nid.as_bytes());
        }
        Ok(Self { bytes })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_service_entries_have_exact_virtual_extents_and_relative_values() {
        let make = |base, target, callable| {
            ModuleRecord::build(
                0x8000,
                0,
                4,
                base,
                &[Export {
                    nid: "9BcDykPmo1I",
                    address: target,
                    kind: Kind::ServiceEntry,
                }],
                &crate::guest_memory::Memory::default(),
                move |_| callable,
            )
        };
        let record = make(0x76f000, 0x770000, true).unwrap();
        let word =
            |offset| u64::from_le_bytes(record.bytes[offset..offset + 8].try_into().unwrap());
        assert_eq!(word(0x30), 0x76f000);
        assert_eq!(record.bytes[0x2bc], 0x12);
        assert_eq!(word(0x2c0), 0x1000);
        assert_eq!(word(0x2c8), 1);
        assert!(make(0x76f000, 0x770000, false).is_err());
        assert!(make(0, 0x770000, true).is_err());
        assert!(make(0x770000, 0x770000, true).is_err());
        assert!(make(0x770001, 0x770000, true).is_err());
        assert!(make(0x76f000, u64::MAX, true).is_err());
        let legacy =
            ModuleRecord::functions(0x8000, 0, 4, 0, &[("9BcDykPmo1I", 0x770000)], |_| true)
                .unwrap();
        assert_eq!(&legacy.bytes[0x2c8..0x2d0], &[0; 8]);
    }
    #[test]
    fn pathname_is_a_complete_resolver_copy_without_moving_symbols() {
        let make = |address| {
            ModuleRecord::functions(address, 0x7000, 4, 0, &[("9BcDykPmo1I", 0x1020)], |_| true)
                .unwrap()
        };
        let original = make(0x8000);
        let path = "/system/common/lib/libSceSystemService.sprx";
        let named = make(0x8000).with_path(0x8000, path).unwrap();
        let offset = original.bytes.len();
        assert_eq!(
            u64::from_le_bytes(named.bytes[8..16].try_into().unwrap()),
            0x8000 + offset as u64
        );
        assert_eq!(&named.bytes[..8], &original.bytes[..8]);
        assert_eq!(&named.bytes[16..offset], &original.bytes[16..]);
        assert_eq!(named.bytes.len(), offset + 1024);
        assert_eq!(&named.bytes[offset..offset + path.len()], path.as_bytes());
        assert!(named.bytes[offset + path.len()..].iter().all(|b| *b == 0));
        assert!(named.with_path(0x8000, path).is_err());
        assert!(make(0x8000).with_path(0x9000, path).is_err());
        for invalid in [
            "",
            "relative.sprx",
            "/",
            "/a/../b",
            "/a//b",
            "/a\0b",
            "/a\\b",
        ] {
            assert!(make(0x8000).with_path(0x8000, invalid).is_err());
        }
        assert!(
            make(0x8000)
                .with_path(0x8000, &format!("/{}", "x".repeat(1022)))
                .is_ok()
        );
        assert!(
            make(0x8000)
                .with_path(0x8000, &format!("/{}", "x".repeat(1023)))
                .is_err()
        );
        let high = u64::MAX - 1023;
        assert!(make(high).with_path(high, path).is_err());
    }
    #[test]
    fn data_exports_validate_whole_storage_and_permissions() {
        let mut writable = [0u8; 4];
        let mut readonly = [0u8; 4];
        let mut code = [0u8; 4];
        let mut memory = crate::guest_memory::Memory::default();
        memory.add(0x1000, &mut writable, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        memory.add(0x3000, &mut code, false, true).unwrap();
        let make = |address, size, writable| {
            ModuleRecord::build(
                0x8000,
                0,
                2,
                0,
                &[Export {
                    nid: "q1OvUam0BJU",
                    address,
                    kind: Kind::Object { size, writable },
                }],
                &memory,
                |_| false,
            )
        };
        let record = make(0x1000, 4, true).unwrap();
        assert_eq!(record.bytes[0x2bc], 0x11);
        assert_eq!(&record.bytes[0x2c8..0x2d0], &4u64.to_le_bytes());
        assert!(make(0x2000, 4, false).is_ok());
        assert!(make(0x2000, 4, true).is_err());
        assert!(make(0x3000, 4, false).is_err());
        assert!(make(0x1000, 5, true).is_err());
        assert!(make(0x1000, 0, true).is_err());
        assert!(make(u64::MAX, 4, true).is_err());
    }
    #[test]
    fn resolver_offsets_relative_values_and_rejections() {
        let r =
            ModuleRecord::functions(0x8000, 0x7000, 2, 0x1000, &[("9BcDykPmo1I", 0x1020)], |a| {
                a == 0x1020
            })
            .unwrap();
        let word = |o| u64::from_le_bytes(r.bytes[o..o + 8].try_into().unwrap());
        assert_eq!(word(0), 0x7000);
        assert_eq!(word(0x148), 0x8180);
        assert_eq!(word(0x1a8), 0x82a0);
        assert_eq!(word(0x1b0), 48);
        assert_eq!(word(0x1b8), 0x82d0);
        assert_eq!(word(0x1c0), 13);
        assert_eq!(&r.bytes[0x2a0..0x2b8], &[0; 24]);
        assert_eq!(word(0x2c0), 0x20);
        assert_eq!(
            &r.bytes[0x2d0..],
            b"\0"
                .iter()
                .chain(b"9BcDykPmo1I\0".iter())
                .copied()
                .collect::<Vec<_>>()
        );
        assert!(
            ModuleRecord::functions(0x8000, 0, 1, 0, &[("9BcDykPmo1I", 1)], |_| false).is_err()
        );
        assert!(
            ModuleRecord::functions(
                0x8000,
                0,
                1,
                0,
                &[("9BcDykPmo1I", 1), ("9BcDykPmo1I", 2)],
                |_| true
            )
            .is_err()
        );
        assert!(
            ModuleRecord::functions(u64::MAX - 15, 0, 1, 0, &[("9BcDykPmo1I", 1)], |_| true)
                .is_err()
        );
    }
}
