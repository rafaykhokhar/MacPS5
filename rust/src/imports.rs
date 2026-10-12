//! Consumer-local module identities. A qualified symbol never loses its owner.
use crate::Result;
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub symbol: String,
    pub module: String,
    pub library_token: String,
    pub module_id: Option<u16>,
    pub library: Option<crate::ownership::Library>,
    pub module_version: Option<(u8, u8)>,
}
fn normalize(value: &str) -> String {
    let value = value.strip_suffix(".prx").unwrap_or(value);
    value
        .strip_suffix("-module")
        .unwrap_or(value)
        .replace('.', "_")
}
pub fn identify(
    name: &str,
    modules: &BTreeMap<u16, String>,
    dependencies: &[String],
) -> Result<Identity> {
    let mut result = Identity {
        name: name.into(),
        symbol: name.into(),
        module: String::new(),
        library_token: String::new(),
        module_id: None,
        library: None,
        module_version: None,
    };
    if !name.contains('#') {
        return Ok(result);
    }
    let pieces: Vec<_> = name.split('#').collect();
    if pieces.len() != 3 || pieces.iter().any(|s| s.is_empty()) {
        return Err("Invalid qualified import".into());
    }
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    if !pieces[1].bytes().all(|b| ALPHABET.contains(&b)) {
        return Err("Invalid library token".into());
    }
    let mut id = 0u16;
    for b in pieces[2].bytes() {
        let digit = ALPHABET
            .iter()
            .position(|v| *v == b)
            .ok_or("Invalid module ID encoding")? as u16;
        id = id
            .checked_mul(64)
            .and_then(|v| v.checked_add(digit))
            .ok_or("Module ID overflow")?;
    }
    let module = modules.get(&id).ok_or("Unknown module ID")?;
    if module.is_empty() || module.contains(['/', '\\', ':', '$', '\r', '\n']) {
        return Err("Invalid module name".into());
    }
    let module = if module.ends_with(".prx") {
        module.clone()
    } else {
        format!("{module}.prx")
    };
    let mut matched = None;
    if !dependencies.contains(&module) {
        for dependency in dependencies {
            if normalize(dependency) == normalize(&module) {
                if matched.is_some_and(|old| old != dependency) {
                    return Err("Ambiguous module dependency alias".into());
                }
                matched = Some(dependency);
            }
        }
    }
    result.symbol = pieces[0].into();
    result.library_token = pieces[1].into();
    result.module_id = Some(id);
    result.module = matched.cloned().unwrap_or(module);
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_aliases_exact_precedence_and_maximum_id() {
        let modules = BTreeMap::from([(1, "foo_native".into()), (65535, "last".into())]);
        let one = vec!["foo.native.prx".into()];
        let identity = identify("nid#A#B", &modules, &one).unwrap();
        assert_eq!(
            (
                identity.symbol.as_str(),
                identity.library_token.as_str(),
                identity.module_id,
                identity.module.as_str()
            ),
            ("nid", "A", Some(1), "foo.native.prx")
        );
        assert_eq!(
            identify("nid#A#P--", &modules, &[]).unwrap().module_id,
            Some(65535)
        );
        assert_eq!(
            identify("nid#A#B", &modules, &[]).unwrap().module,
            "foo_native.prx"
        );
        assert_eq!(
            identify(
                "nid#A#B",
                &modules,
                &["foo_native.prx".into(), "foo.native.prx".into()]
            )
            .unwrap()
            .module,
            "foo_native.prx"
        );
        assert_eq!(identify("plain", &modules, &[]).unwrap().module_id, None);
    }
    #[test]
    fn rejects_invalid_or_ambiguous_ownership() {
        let modules = BTreeMap::from([(1, "foo_native".into())]);
        for name in [
            "foo#A#C",
            "foo#A#",
            "foo#A#?",
            "foo#A#QAA",
            "foo#A#BBBBBBBB",
            "foo#A",
            "#A#B",
            "foo##B",
            "foo#?#B",
            "foo#A#B#A",
        ] {
            assert!(identify(name, &modules, &[]).is_err(), "{name}");
        }
        assert!(
            identify(
                "foo#A#B",
                &modules,
                &["foo.native.prx".into(), "foo.native-module.prx".into()]
            )
            .is_err()
        );
        assert!(identify("foo#A#B", &BTreeMap::from([(1, "../bad".into())]), &[]).is_err());
    }
}
