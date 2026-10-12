//! ELF constructor ordering. Module start/stop status routines are a separate ABI.
use crate::{Result, loader::Image};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Lifecycle {
    pub initializers: Vec<u64>,
    pub finalizers: Vec<u64>,
}
pub fn discover(image: &Image, tags: &BTreeMap<u64, u64>) -> Result<Lifecycle> {
    let pairs = [
        (12, 0x6000000c),
        (13, 0x6000000d),
        (25, 0x60000019),
        (26, 0x6000001a),
        (27, 0x6000001b),
        (28, 0x6000001c),
    ];
    for (standard, sce) in pairs {
        if tags.contains_key(&standard) && tags.contains_key(&sce) {
            return Err("Ambiguous lifecycle tags".into());
        }
    }
    if tags.contains_key(&32) || tags.contains_key(&33) {
        return Err("Shared-module preinit arrays unsupported".into());
    }
    let get = |tag| {
        tags.get(&tag).copied().or_else(|| {
            pairs
                .iter()
                .find(|(standard, _)| *standard == tag)
                .and_then(|(_, sce)| tags.get(sce).copied())
        })
    };
    let validate = |address: u64| -> Result<u64> {
        if image.segments.iter().any(|s| {
            s.flags & 1 != 0 && address >= s.address && address - s.address < s.bytes.len() as u64
        }) {
            Ok(address)
        } else {
            Err("Lifecycle entry outside executable module memory".into())
        }
    };
    let direct = |tag| -> Result<Option<u64>> {
        get(tag)
            .filter(|value| *value != 0)
            .map(|offset| {
                validate(
                    image
                        .bias
                        .checked_add(offset)
                        .ok_or("Lifecycle entry overflow")?,
                )
            })
            .transpose()
    };
    let array = |pointer_tag, size_tag| -> Result<Vec<u64>> {
        let (pointer, size) = match (get(pointer_tag), get(size_tag)) {
            (None, None) => return Ok(vec![]),
            (Some(p), Some(s)) => (p, s),
            _ => return Err("Incomplete lifecycle array metadata".into()),
        };
        if size % 8 != 0 || size / 8 > 4096 {
            return Err("Invalid lifecycle array size".into());
        }
        if size == 0 {
            return Ok(vec![]);
        }
        let address = image
            .bias
            .checked_add(pointer)
            .ok_or("Lifecycle array overflow")?;
        let segment = image
            .segments
            .iter()
            .find(|s| {
                address >= s.address
                    && address - s.address <= s.bytes.len() as u64
                    && size <= s.bytes.len() as u64 - (address - s.address)
            })
            .ok_or("Lifecycle array outside mapped module")?;
        let offset = (address - segment.address) as usize;
        let bytes = &segment.bytes[offset..offset + size as usize];
        let mut result = Vec::new();
        for bytes in bytes.chunks_exact(8) {
            let address = u64::from_le_bytes(bytes.try_into().unwrap());
            if address == 0 || address == u64::MAX {
                continue;
            }
            result.push(validate(address)?);
        }
        Ok(result)
    };
    let mut initializers = Vec::new();
    if let Some(entry) = direct(12)? {
        initializers.push(entry);
    }
    initializers.extend(array(25, 27)?);
    let mut finalizers = array(26, 28)?;
    finalizers.reverse();
    if let Some(entry) = direct(13)? {
        finalizers.push(entry);
    }
    Ok(Lifecycle {
        initializers,
        finalizers,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::{Arguments, EntryPoints, Executor, Modules, Phase};
    struct Exec {
        calls: Vec<u64>,
        failure: Option<u64>,
    }
    impl Executor for Exec {
        fn call(&mut self, _: &mut Image, address: u64, _: Arguments) -> Result<i32> {
            self.calls.push(address);
            Ok(0)
        }
        fn initialize(&mut self, _: &mut Image, address: u64) -> Result<()> {
            self.calls.push(address);
            if self.failure == Some(address) {
                Err("Constructor fault".into())
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn relocated_arrays_and_direct_entries_obey_order() {
        for sce in [false, true] {
            let file = crate::modules::lifecycle_fixture(sce);
            let mut image = Image::load(&file, 0x10000, 65536).unwrap();
            assert!(image.lifecycle().is_err());
            image.relocate(|_| None).unwrap();
            let lifecycle = image.lifecycle().unwrap();
            assert_eq!(lifecycle.initializers, [0x10120, 0x10130]);
            assert_eq!(lifecycle.finalizers, [0x10150, 0x10140]);
            let mut modules = Modules::new(65536);
            let id = modules
                .load(
                    "test.prx",
                    &file,
                    0x10000,
                    &[],
                    EntryPoints {
                        start: Some(0x100),
                        stop: Some(0x110),
                    },
                )
                .unwrap();
            let mut exec = Exec {
                calls: vec![],
                failure: None,
            };
            modules.start(id, &mut exec, Arguments::default()).unwrap();
            modules.stop(id, &mut exec, Arguments::default()).unwrap();
            assert_eq!(
                exec.calls,
                [0x10120, 0x10130, 0x10100, 0x10110, 0x10150, 0x10140]
            );
            modules.unload(id).unwrap();
        }
    }
    #[test]
    fn array_order_is_forward_for_init_and_reverse_for_fini() {
        let mut file = crate::modules::lifecycle_fixture(false);
        file[0x240..0x250]
            .copy_from_slice(&[0x10120u64.to_le_bytes(), 0x10130u64.to_le_bytes()].concat());
        file[0x250..0x260]
            .copy_from_slice(&[0x10140u64.to_le_bytes(), 0x10150u64.to_le_bytes()].concat());
        file[0x1b8..0x1c0].copy_from_slice(&16u64.to_le_bytes());
        file[0x1d8..0x1e0].copy_from_slice(&16u64.to_le_bytes());
        // No relocation writes: test both elements, including reverse finalization.
        file[0x1f8..0x200].copy_from_slice(&0u64.to_le_bytes());
        let mut image = Image::load(&file, 0x10000, 65536).unwrap();
        image.relocate(|_| None).unwrap();
        let plan = image.lifecycle().unwrap();
        assert_eq!(plan.initializers, [0x10120, 0x10120, 0x10130]);
        assert_eq!(plan.finalizers, [0x10150, 0x10140, 0x10140]);
    }
    #[test]
    fn malformed_lifecycle_never_publishes_module() {
        for (offset, value) in [
            (0x188, u64::MAX),
            (0x1a8, 0x3ff),
            (0x1b8, 7),
            (0x1b8, 32776),
            (0x290, 0x800),
            (0x210, 0x6000000c),
            (0x210, 32),
        ] {
            let mut file = crate::modules::lifecycle_fixture(false);
            file[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            if offset == 0x210 {
                file[152..160].copy_from_slice(&176u64.to_le_bytes());
                file[160..168].copy_from_slice(&176u64.to_le_bytes());
            }
            let mut modules = Modules::new(65536);
            assert!(
                modules
                    .load("bad.prx", &file, 0x10000, &[], EntryPoints::default())
                    .is_err(),
                "offset {offset:x}"
            );
            assert_eq!(modules.bytes(), 0);
        }
    }
    #[test]
    fn failed_initializer_or_finalizer_pins_image_and_stops_sequence() {
        for fail in [0x10120, 0x10150] {
            let mut modules = Modules::new(65536);
            let id = modules
                .load(
                    "test.prx",
                    &crate::modules::lifecycle_fixture(false),
                    0x10000,
                    &[],
                    EntryPoints::default(),
                )
                .unwrap();
            let mut exec = Exec {
                calls: vec![],
                failure: Some(fail),
            };
            if fail == 0x10120 {
                assert!(modules.start(id, &mut exec, Arguments::default()).is_err());
                assert_eq!(exec.calls, [fail]);
            } else {
                modules.start(id, &mut exec, Arguments::default()).unwrap();
                assert!(modules.stop(id, &mut exec, Arguments::default()).is_err());
                assert_eq!(exec.calls, [0x10120, 0x10130, fail]);
            }
            assert_eq!(modules.phase(id), Some(Phase::Faulted));
            assert!(modules.unload(id).is_err());
        }
    }
}
