//! Explicit module-owned native exports. Registration and removal are transactional.
use crate::{Result, imports::Identity};
use std::collections::BTreeMap;
#[derive(Default, Clone)]
pub struct Registry {
    exports: BTreeMap<(String, String), u64>,
    scoped: BTreeMap<ScopedKey, u64>,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopedKey {
    owner: String,
    symbol: String,
    library: String,
    library_version: u16,
    module_version: (u8, u8),
}
impl Registry {
    /// Qualified symbol tokens are already encoded by the producer: do not hash
    /// them again or expose them as unqualified aliases.
    pub fn register_image(
        &mut self,
        owner: &str,
        exports: &[crate::exports::Export],
    ) -> Result<()> {
        let mut planned = self.clone();
        let plain = exports
            .iter()
            .filter(|e| e.scope.is_none())
            .map(|e| (e.name.as_str(), e.address))
            .collect::<Vec<_>>();
        planned.register_module(owner, &plain)?;
        for export in exports {
            let Some(scope) = &export.scope else {
                continue;
            };
            let identity = crate::imports::identify(
                "symbol#A#A",
                &BTreeMap::from([(0, scope.module.name.clone())]),
                &[owner.into()],
            )?;
            if identity.module != owner
                || scope.symbol.is_empty()
                || scope.symbol.contains(['#', '\0'])
                || scope.library.name.is_empty()
                || scope.library.name.contains(['#', '\0'])
                || export.address == 0
            {
                return Err("Invalid qualified export owner or symbol".into());
            }
            let key = ScopedKey {
                owner: owner.into(),
                symbol: scope.symbol.clone(),
                library: scope.library.name.clone(),
                library_version: scope.library.version,
                module_version: (scope.module.major, scope.module.minor),
            };
            if planned
                .scoped
                .get(&key)
                .is_some_and(|&v| v != export.address)
            {
                return Err("Conflicting qualified export".into());
            }
            planned.scoped.insert(key, export.address);
        }
        *self = planned;
        Ok(())
    }
    pub fn register(&mut self, module: &str, name: &str, address: u64) -> Result<()> {
        self.register_module(module, &[(name, address)])
    }
    /// Commit all requested exports together; conflicting name/NID aliases reject the batch.
    pub fn register_module(&mut self, module: &str, exports: &[(&str, u64)]) -> Result<()> {
        if module.is_empty() || module.contains(['/', '\\', ':', '$', '\r', '\n', '\0']) {
            return Err("Invalid export module name".into());
        }
        let mut planned = self.exports.clone();
        for &(name, address) in exports {
            if name.is_empty() || name.contains(['#', '\0']) || address == 0 {
                return Err("Invalid native export".into());
            }
            let nid = crate::strings::nid(name);
            let nid = std::str::from_utf8(&nid[..11]).map_err(|_| "Invalid NID encoding")?;
            for token in [name, nid] {
                let key = (module.to_owned(), token.to_owned());
                if planned.get(&key).is_some_and(|old| *old != address) {
                    return Err("Conflicting module export".into());
                }
                planned.insert(key, address);
            }
        }
        self.exports = planned;
        Ok(())
    }
    pub fn resolve(&self, identity: &Identity) -> Option<u64> {
        self.resolve_owned(identity).map(|(_, address)| address)
    }
    /// Dynamic lookup within one module handle's owner. Scoped exports are only
    /// usable when all matching name/NID candidates agree on one address.
    pub fn resolve_dynamic(&self, owner: &str, name: &str) -> Option<u64> {
        if name.is_empty() || name.contains(['#', '\0']) {
            return None;
        }
        let nid = crate::strings::nid(name);
        let nid = std::str::from_utf8(&nid[..11]).ok()?;
        let mut result = self.exports.get(&(owner.into(), name.into())).copied();
        for (key, &address) in &self.scoped {
            if key.owner == owner && (key.symbol == name || key.symbol == nid) {
                if result.is_some_and(|prior| prior != address) {
                    return None;
                }
                result = Some(address);
            }
        }
        result
    }
    pub fn resolve_owned(&self, identity: &Identity) -> Option<(&str, u64)> {
        if let Some(library) = &identity.library {
            let key = ScopedKey {
                owner: identity.module.clone(),
                symbol: identity.symbol.clone(),
                library: library.name.clone(),
                library_version: library.version,
                module_version: identity.module_version?,
            };
            return self
                .scoped
                .get_key_value(&key)
                .map(|(key, &value)| (key.owner.as_str(), value));
        }
        if identity.module_version.is_some() {
            return None;
        }
        if identity.module_id.is_some() && identity.module.is_empty() {
            return None;
        }
        if !identity.module.is_empty() {
            return self
                .exports
                .get_key_value(&(identity.module.clone(), identity.symbol.clone()))
                .map(|((owner, _), &address)| (owner.as_str(), address));
        }
        let mut result = None;
        for ((owner, symbol), &address) in &self.exports {
            if symbol != &identity.symbol {
                continue;
            }
            if result.is_some() {
                return None;
            }
            result = Some((owner.as_str(), address));
        }
        result
    }
    /// Caller must stop guest execution and detach dependent modules before removal.
    pub fn remove_module(&mut self, module: &str) -> bool {
        let before = self.exports.len() + self.scoped.len();
        self.exports.retain(|(owner, _), _| owner != module);
        self.scoped.retain(|key, _| key.owner != module);
        before != self.exports.len() + self.scoped.len()
    }
}
#[allow(unsafe_code)]
pub mod ffi {
    use super::*;
    use crate::loader_ffi::{Bytes, Identity as ForeignIdentity, Resolution};
    unsafe fn text<'a>(bytes: Bytes) -> Option<&'a str> {
        if bytes.size > isize::MAX as u64 || bytes.data.is_null() {
            return None;
        }
        // SAFETY: caller guarantees readable bytes and lifetime for the synchronous call.
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(bytes.data, bytes.size as usize) })
            .ok()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn native_ps5_registry_new_v1() -> *mut Registry {
        Box::into_raw(Box::new(Registry::default()))
    }
    /// # Safety
    /// Registry must come from new_v1 and be destroyed once, after all users finish.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_registry_delete_v1(registry: *mut Registry) {
        if !registry.is_null() {
            unsafe {
                drop(Box::from_raw(registry));
            }
        }
    }
    /// # Safety
    /// Exclusive live registry and readable module/name buffers valid throughout this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_registry_register_v1(
        registry: *mut Registry,
        module: Bytes,
        name: Bytes,
        address: u64,
    ) -> u64 {
        let Some(registry) = (unsafe { registry.as_mut() }) else {
            return 1;
        };
        let (Some(module), Some(name)) = (unsafe { text(module) }, unsafe { text(name) }) else {
            return 1;
        };
        if registry.register(module, name, address).is_ok() {
            0
        } else {
            1
        }
    }
    /// # Safety
    /// Live immutable registry and all identity buffers valid throughout this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_registry_resolve_v2(
        registry: *const Registry,
        identity: ForeignIdentity,
    ) -> Resolution {
        let missing = Resolution {
            value: 0,
            status: 1,
        };
        let Some(registry) = (unsafe { registry.as_ref() }) else {
            return missing;
        };
        let (Some(name), Some(symbol), Some(module), Some(library_token)) = (
            unsafe { text(identity.name) },
            unsafe { text(identity.symbol) },
            unsafe { text(identity.module) },
            unsafe { text(identity.library_token) },
        ) else {
            return missing;
        };
        let Some(library) = (unsafe { text(identity.library) }) else {
            return missing;
        };
        let scoped = !library.is_empty();
        if identity.module_id > 65536
            || identity.library_version > 65536
            || identity.module_version > 65536
            || scoped != (identity.library_version < 65536)
            || scoped != (identity.module_version < 65536)
            || (scoped && (module.is_empty() || identity.module_id == 65536))
        {
            return missing;
        }
        let identity = Identity {
            library: scoped.then(|| crate::ownership::Library {
                name: library.into(),
                version: identity.library_version as u16,
            }),
            module_version: scoped.then_some((
                (identity.module_version >> 8) as u8,
                identity.module_version as u8,
            )),
            name: name.into(),
            symbol: symbol.into(),
            module: module.into(),
            library_token: library_token.into(),
            module_id: if identity.module_id == 65536 {
                None
            } else {
                Some(identity.module_id as u16)
            },
        };
        match registry.resolve(&identity) {
            Some(value) => Resolution { value, status: 0 },
            None => missing,
        }
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn dynamic_lookup_respects_owner_and_rejects_scoped_ambiguity() {
        use super::*;
        let mut registry = Registry::default();
        registry
            .register_module("a.prx", &[("function", 100)])
            .unwrap();
        registry
            .register_module("b.prx", &[("function", 200)])
            .unwrap();
        assert_eq!(registry.resolve_dynamic("a.prx", "function"), Some(100));
        assert_eq!(registry.resolve_dynamic("b.prx", "function"), Some(200));
        let nid = crate::strings::nid("scoped");
        let token = std::str::from_utf8(&nid[..11]).unwrap();
        let key = ScopedKey {
            owner: "a.prx".into(),
            symbol: token.into(),
            library: "libA".into(),
            library_version: 1,
            module_version: (1, 0),
        };
        registry.scoped.insert(key.clone(), 300);
        assert_eq!(registry.resolve_dynamic("a.prx", "scoped"), Some(300));
        assert_eq!(registry.resolve_dynamic("a.prx", token), Some(300));
        let mut conflicting = key;
        conflicting.library_version = 2;
        registry.scoped.insert(conflicting, 400);
        assert_eq!(registry.resolve_dynamic("a.prx", "scoped"), None);
        assert_eq!(registry.resolve_dynamic("b.prx", "scoped"), None);
        registry.remove_module("a.prx");
        assert_eq!(registry.resolve_dynamic("a.prx", "function"), None);
    }
    use super::*;
    fn plain(name: &str) -> Identity {
        crate::imports::identify(name, &BTreeMap::new(), &[]).unwrap()
    }
    #[test]
    #[allow(unsafe_code)]
    fn foreign_scope_layout_and_incomplete_versions_fail_closed() {
        use crate::{loader_ffi, ownership};
        assert_eq!(std::mem::size_of::<loader_ffi::Identity>(), 104);
        assert_eq!(std::mem::offset_of!(loader_ffi::Identity, library), 72);
        assert_eq!(
            std::mem::offset_of!(loader_ffi::Identity, module_version),
            96
        );
        assert_eq!(std::mem::size_of::<loader_ffi::Record>(), 120);
        let library = ownership::Library {
            name: "lib".into(),
            version: 0,
        };
        let mut registry = Registry::default();
        registry
            .register_image(
                "owner.prx",
                &[crate::exports::Export {
                    name: "token#A#B".into(),
                    address: 0x1234,
                    size: 1,
                    function: true,
                    weak: false,
                    scope: Some(ownership::Scope {
                        symbol: "token".into(),
                        library_id: 0,
                        module_id: 1,
                        library: library.clone(),
                        module: ownership::Module {
                            name: "owner".into(),
                            major: 0,
                            minor: 0,
                        },
                    }),
                }],
            )
            .unwrap();
        let mut identity =
            crate::imports::identify("token#A#B", &BTreeMap::from([(1, "owner".into())]), &[])
                .unwrap();
        identity.library = Some(library);
        identity.module_version = Some((0, 0));
        let resolve = |foreign| unsafe { ffi::native_ps5_registry_resolve_v2(&registry, foreign) };
        let result = resolve(loader_ffi::Identity::of(&identity));
        assert_eq!((result.status, result.value), (0, 0x1234));
        for variant in 0..7 {
            let mut foreign = loader_ffi::Identity::of(&identity);
            match variant {
                0 => foreign.library_version = 65536,
                1 => foreign.module_version = 65536,
                2 => foreign.module_id = 65536,
                3 => foreign.library.size = 0,
                4 => foreign.library_version = 65537,
                5 => foreign.module_version = u64::MAX,
                _ => foreign.module.size = 0,
            }
            assert_eq!(resolve(foreign).status, 1, "variant {variant}");
        }
    }
    #[test]
    fn module_scope_nids_ambiguity_and_removal() {
        let mut registry = Registry::default();
        registry.register("one.prx", "foo", 0x1234).unwrap();
        let nid = crate::strings::nid("foo");
        let nid = std::str::from_utf8(&nid[..11]).unwrap();
        assert_eq!(registry.resolve(&plain("foo")), Some(0x1234));
        assert_eq!(registry.resolve(&plain(nid)), Some(0x1234));
        registry.register("two.prx", "foo", 0x1234).unwrap();
        assert_eq!(registry.resolve(&plain("foo")), None);
        let mut identity = plain(nid);
        identity.module = "one.prx".into();
        identity.module_id = Some(1);
        assert_eq!(registry.resolve(&identity), Some(0x1234));
        identity.module = "absent.prx".into();
        assert_eq!(registry.resolve(&identity), None);
        identity.module.clear();
        assert_eq!(registry.resolve(&identity), None);
        assert!(registry.remove_module("two.prx"));
        assert_eq!(registry.resolve(&plain("foo")), Some(0x1234));
        assert!(!registry.remove_module("two.prx"));
        registry.remove_module("one.prx");
        assert_eq!(registry.resolve(&plain(nid)), None);
    }
    #[test]
    fn failed_batch_preserves_all_prior_exports() {
        let mut registry = Registry::default();
        registry.register("one.prx", "foo", 1).unwrap();
        assert!(
            registry
                .register_module("one.prx", &[("new", 2), ("foo", 3)])
                .is_err()
        );
        assert_eq!(registry.resolve(&plain("new")), None);
        assert_eq!(registry.resolve(&plain("foo")), Some(1));
        let nid = crate::strings::nid("foo");
        assert!(
            registry
                .register("one.prx", std::str::from_utf8(&nid[..11]).unwrap(), 2)
                .is_err()
        );
        for (module, name, address) in [
            ("../bad", "foo", 1),
            ("good", "foo#A#B", 1),
            ("good", "foo", 0),
            ("", "foo", 1),
            ("good", "", 1),
        ] {
            assert!(registry.register(module, name, address).is_err());
        }
    }
    #[test]
    fn rust_loader_links_qualified_fixture_through_registry() {
        let mut registry = Registry::default();
        registry
            .register("libSceAgcDriver.prx", "sceAgcDriverSubmitDcb", 0x123400)
            .unwrap();
        registry
            .register("wrong.prx", "sceAgcDriverSubmitDcb", 0x987600)
            .unwrap();
        for sce in [false, true] {
            let mut image =
                crate::loader::Image::load(&crate::loader_fixture::graphics(sce), 0x100000, 65536)
                    .unwrap();
            image
                .relocate_scoped(|identity| registry.resolve(identity))
                .unwrap();
            assert_eq!(image.relocations, [(0x104000, 0x123400)]);
            assert!(image.unresolved.is_empty());
        }
    }
}
