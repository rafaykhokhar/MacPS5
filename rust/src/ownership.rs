//! Producer-local ELF ownership records. IDs are not global registry keys.
//! Layout reference: shadPS4 src/core/module.h ModuleInfo/LibraryInfo and
//! src/core/loader/elf.h dynamic tags. This parser does not infer compatibility
//! between module versions or silently discard library versions.
use crate::Result;
use std::collections::BTreeMap;

pub(crate) const MODULE_INFO: u64 = 0x6100000d;
pub(crate) const EXPORT_LIB: u64 = 0x61000013;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub major: u8,
    pub minor: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    pub name: String,
    pub version: u16,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub symbol: String,
    pub module_id: u16,
    pub library_id: u16,
    pub module: Module,
    pub library: Library,
}
#[derive(Debug, Default)]
pub(crate) struct Exports {
    modules: BTreeMap<u16, Module>,
    libraries: BTreeMap<u16, Library>,
}

pub(crate) fn decode_id(token: &str) -> Result<u16> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    if token.is_empty() || token.len() > 3 {
        return Err("Invalid ownership ID length".into());
    }
    token.bytes().try_fold(0u16, |value, byte| {
        let digit = ALPHABET
            .iter()
            .position(|&b| b == byte)
            .ok_or("Invalid ownership ID encoding")? as u16;
        value
            .checked_mul(64)
            .and_then(|v| v.checked_add(digit))
            .ok_or_else(|| "Ownership ID overflow".into())
    })
}

impl Exports {
    pub(crate) fn parse(
        records: &[(u64, u64)],
        mut string: impl FnMut(u64) -> Result<String>,
    ) -> Result<Self> {
        let mut result = Self::default();
        for &(tag, value) in records {
            let name = string(value & 0xffff_ffff)?;
            if name.is_empty() || name.contains(['/', '\\', ':', '$', '#', '\0', '\r', '\n']) {
                return Err("Invalid export owner name".into());
            }
            let id = (value >> 48) as u16;
            let duplicate = match tag {
                MODULE_INFO => result
                    .modules
                    .insert(
                        id,
                        Module {
                            name,
                            major: (value >> 40) as u8,
                            minor: (value >> 32) as u8,
                        },
                    )
                    .is_some(),
                EXPORT_LIB => result
                    .libraries
                    .insert(
                        id,
                        Library {
                            name,
                            version: (value >> 32) as u16,
                        },
                    )
                    .is_some(),
                _ => return Err("Invalid export ownership record tag".into()),
            };
            if duplicate {
                return Err("Duplicate export ownership ID".into());
            }
        }
        Ok(result)
    }

    pub(crate) fn identify(&self, name: &str) -> Result<Option<Scope>> {
        if !name.contains('#') {
            return Ok(None);
        }
        let parts: Vec<_> = name.split('#').collect();
        if parts.len() != 3 || parts[0].is_empty() {
            return Err("Invalid qualified export name".into());
        }
        let library_id = decode_id(parts[1])?;
        let module_id = decode_id(parts[2])?;
        Ok(Some(Scope {
            symbol: parts[0].into(),
            module_id,
            library_id,
            module: self
                .modules
                .get(&module_id)
                .ok_or("Unknown export module ID")?
                .clone(),
            library: self
                .libraries
                .get(&library_id)
                .ok_or("Unknown export library ID")?
                .clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ownership_ids_are_bounded_and_versions_are_preserved() {
        let records = [
            (MODULE_INFO, (1 << 48) | (7 << 40) | (9 << 32)),
            (EXPORT_LIB, (65535 << 48) | (513 << 32) | 1),
        ];
        let exports = Exports::parse(&records, |offset| {
            Ok(if offset == 0 { "module" } else { "library" }.into())
        })
        .unwrap();
        let scope = exports.identify("nid#P--#B").unwrap().unwrap();
        assert_eq!(
            scope.module,
            Module {
                name: "module".into(),
                major: 7,
                minor: 9
            }
        );
        assert_eq!(
            scope.library,
            Library {
                name: "library".into(),
                version: 513
            }
        );
        assert_eq!(scope.library_id, 65535);
        assert_eq!(scope.module_id, 1);
        assert!(exports.identify("plain").unwrap().is_none());
        for name in [
            "nid#A#B",
            "nid#P--#C",
            "nid#QAA#B",
            "nid#AAAA#B",
            "nid##B",
            "nid#?#B",
            "#P--#B",
            "nid#P--#B#A",
        ] {
            assert!(exports.identify(name).is_err(), "{name}");
        }
        assert!(Exports::parse(&[records[0], records[0]], |_| Ok("module".into())).is_err());
        assert!(Exports::parse(&[records[1], records[1]], |_| Ok("library".into())).is_err());
        assert!(Exports::parse(&records, |_| Ok("../owner".into())).is_err());
    }
}
