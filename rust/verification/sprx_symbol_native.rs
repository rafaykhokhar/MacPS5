//! Execute the pinned homebrew's real SPRX symbol resolver, including its NID encoder.
use native_ps5::{
    guest_memory::{DataRegion, Memory},
    kernel_module::{Export, Kind, ModuleRecord},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    let bias = 0x10000000;
    let mut image = Image::load(file, bias, 64 * 1024 * 1024)?;
    image.relocate(|_| None)?;
    // Pinned ELF sprx_sym2addr prologue; reject a different binary/layout.
    let code = image
        .segments
        .iter()
        .find(|s| s.address <= bias + 0x67e0 && bias + 0x67e8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing SPRX resolver")?;
    let offset = (bias + 0x67e0 - code.address) as usize;
    if code.bytes[offset..offset + 8] != [0x48, 0x83, 0xbf, 0x78, 0x04, 0, 0, 0] {
        return Err("Unexpected pinned SPRX resolver ABI".into());
    }
    let mut native = NativeExecutor::new(0x20000000, 65536, 1_000_000, 1_000_000)?;
    native.register_service(0x790000, Service::Strncmp)?;
    native.register_service(0x770000, Service::SystemHideSplashScreen)?;
    // SAFETY: caller supplies the matching trusted local runtime archive.
    unsafe {
        native.compile_image(
            file,
            bias,
            BTreeSet::from([0x67e0]),
            65536,
            archive,
            directory,
        )?;
    }
    let slot = bias + 0x590010;
    let segment = image
        .segments
        .iter_mut()
        .find(|s| {
            s.flags & 2 != 0 && s.address <= slot && slot + 8 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing resolver strncmp slot")?;
    let index = (slot - segment.address) as usize;
    segment.bytes[index..index + 8].copy_from_slice(&0x790000u64.to_le_bytes());
    // Real service registrations establish callable metadata, though this test
    // executes only the resolver and its strncmp dependency, not the exports.
    for (address, service) in [
        (0x710000, Service::Getpid),
        (0x720000, Service::Snprintf),
        (0x750000, Service::SysmoduleLoadInternal),
    ] {
        native.register_service(address, service)?;
    }
    for (base, path, definitions) in [
        (
            0x76f000u64,
            "/system/common/lib/libSceSystemService.sprx",
            vec![(
                "sceSystemServiceHideSplashScreen",
                0x770000u64,
                Kind::ServiceEntry,
            )],
        ),
        (
            0x6ff000,
            "/system/common/lib/libkernel_web.sprx",
            vec![
                ("getpid", 0x710000, Kind::ServiceEntry),
                (
                    "environ",
                    0x30004010,
                    Kind::Object {
                        size: 8,
                        writable: true,
                    },
                ),
            ],
        ),
        (
            0x71f000,
            "/system/common/lib/libSceLibcInternal.sprx",
            vec![
                ("snprintf", 0x720000, Kind::ServiceEntry),
                (
                    "__isthreaded",
                    0x30003000,
                    Kind::Object {
                        size: 4,
                        writable: true,
                    },
                ),
            ],
        ),
        (
            0x74f000,
            "/system/common/lib/libSceSysmodule.sprx",
            vec![(
                "sceSysmoduleLoadModuleInternal",
                0x750000,
                Kind::ServiceEntry,
            )],
        ),
    ] {
        let nids: Vec<_> = definitions
            .iter()
            .map(|(name, _, _)| native_ps5::strings::nid(name))
            .collect();
        let exports: Vec<_> = definitions
            .iter()
            .zip(&nids)
            .map(|((_, address, kind), nid)| {
                Ok(Export {
                    nid: std::str::from_utf8(&nid[..11]).map_err(|e| e.to_string())?,
                    address: *address,
                    kind: *kind,
                })
            })
            .collect::<Result<_, String>>()?;
        let mut objects = vec![0xa5; 0x1020];
        let original = {
            let mut memory = Memory::default();
            memory.add(0x30003000, &mut objects, true, false)?;
            ModuleRecord::build(0x30002000, 0, 4, base, &exports, &memory, |address| {
                definitions.iter().any(|(_, target, kind)| {
                    *target == address && matches!(kind, Kind::ServiceEntry)
                })
            })?
            .with_path(0x30002000, path)?
            .bytes
        };
        for (symbol, (name, target, kind)) in definitions.iter().enumerate() {
            let st = 0x2a0 + (symbol + 1) * 24;
            assert_eq!(
                u64::from_le_bytes(original[st + 8..st + 16].try_into().unwrap()),
                target - base
            );
            let expected_size = match kind {
                Kind::Object { size, .. } => *size,
                Kind::ServiceEntry => 1,
                Kind::Function => unreachable!(),
            };
            assert_eq!(
                u64::from_le_bytes(original[st + 16..st + 24].try_into().unwrap()),
                expected_size
            );
            // The actual code returns NULL for missing symbols (loop at6860),
            // a zero map base (6808), or a zero st_size (6883). Mutate only the
            // fixture for negatives; the provider builder correctly rejects bad
            // service/object declarations rather than publishing these forms.
            for variant in 0..4 {
                let mut record = original.clone();
                if variant == 3 {
                    record[st + 16..st + 24].fill(0);
                }
                let word =
                    |offset| u64::from_le_bytes(record[offset..offset + 8].try_into().unwrap());
                let mut context = vec![0u8; 0x490];
                for (offset, value) in [
                    (0x438, if variant == 2 { 0 } else { base }),
                    (0x470, word(0x1b8)),
                    (0x478, word(0x1a8)),
                    (0x480, word(0x1b0)),
                ] {
                    context[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                }
                let mut query = if variant == 1 {
                    b"definitely_missing_symbol".to_vec()
                } else {
                    name.as_bytes().to_vec()
                };
                query.push(0);
                let snapshots = (
                    context.clone(),
                    record.clone(),
                    query.clone(),
                    objects.clone(),
                );
                let mut data = [
                    DataRegion {
                        address: 0x30000000,
                        bytes: &mut context,
                        writable: false,
                    },
                    DataRegion {
                        address: 0x30001000,
                        bytes: &mut query,
                        writable: false,
                    },
                    DataRegion {
                        address: 0x30002000,
                        bytes: &mut record,
                        writable: false,
                    },
                    DataRegion {
                        address: 0x30003000,
                        bytes: &mut objects,
                        writable: true,
                    },
                ];
                native.fuel = 1_000_000;
                let result = native.run_with_data(
                    &mut image,
                    bias + 0x67e0,
                    Arguments {
                        size: 0x30000000,
                        pointer: 0x30001000,
                    },
                    &mut [],
                    &mut data,
                )?;
                let expected = if variant == 0 { *target } else { 0 };
                // These fixture addresses fit the executor's i32 entry return.
                // This is not a test of resolving addresses above 4 GiB.
                if !matches!(result,Execution::Returned(value) if u64::from(value as u32)==expected)
                {
                    return Err(format!(
                        "Actual SPRX resolver failed {name} variant{variant}: expected{expected:#x}"
                    ));
                }
                assert_eq!((context, record, query, objects.clone()), snapshots);
            }
        }
    }
    println!(
        "PASS: actual homebrew SPRX resolver/NID encoder resolve nonzero-base legacy service and object exports; missing names/zero base/zero extent return NULL without data mutation"
    );
    Ok(())
}
