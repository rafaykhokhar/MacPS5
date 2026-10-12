//! Explicit guest contexts, not host TLS or a worker-thread scheduler.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    module_files::ModuleFiles,
    modules::{Arguments, EntryPoints, LoadConfig, LoadProgress, Modules, StartEvent},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
const CREATE: u64 = 0x770000;
const DELETE: u64 = 0x770010;
const GET: u64 = 0x770020;
const SET: u64 = 0x770030;
const DLSYM: u64 = 0x770040;
const LOAD: u64 = 0x770050;
fn target(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn save(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb9]);
    code.extend(address.to_le_bytes());
    code.extend([0x48, 0x89, 0x01]);
}
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    target: u64,
    a: u64,
    b: u64,
    data: &mut [u8; 64],
) -> Result<u64, String> {
    for (chunk, value) in data[..24].chunks_exact_mut(8).zip([a, b, target]) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    let outcome = native.run_with_data(
        image,
        0x100100,
        Arguments {
            size: DATA,
            pointer: 0,
        },
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: data,
            writable: true,
        }],
    )?;
    assert!(matches!(outcome, Execution::Returned(0)));
    Ok(u64::from_le_bytes(data[24..32].try_into().unwrap()))
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let invoke_code = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x43, 16, 0xff,
        0xd0, 0x48, 0x89, 0x43, 24, 0x31, 0xc0, 0x5b, 0xc3,
    ];
    file[0x100..0x100 + invoke_code.len()].copy_from_slice(&invoke_code);
    let mut suspended = vec![0x53, 0x48, 0x89, 0xfb];
    target(&mut suspended, DLSYM);
    suspended.extend([0x48, 0x89, 0xdf]);
    target(&mut suspended, GET);
    save(&mut suspended, DATA + 56);
    suspended.extend([0x31, 0xc0, 0x5b, 0xc3]);
    assert!(suspended.len() <= 64);
    file[0x140..0x140 + suspended.len()].copy_from_slice(&suspended);
    let mut cross = Vec::new();
    target(&mut cross, 0x200100);
    save(&mut cross, DATA + 56);
    cross.extend([0x31, 0xc0, 0xc3]);
    assert!(cross.len() <= 32);
    file[0x180..0x180 + cross.len()].copy_from_slice(&cross);
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x31, 0xf6];
    target(&mut alias, CREATE);
    alias.push(0xc3);
    file[0x1a0..0x1a0 + alias.len()].copy_from_slice(&alias);
    let mut load = Vec::new();
    target(&mut load, LOAD);
    load.extend([0x31, 0xc0, 0xc3]);
    file[0x1c0..0x1c0 + load.len()].copy_from_slice(&load);
    let path = b"/app0/tls-child.prx\0";
    file[0x500..0x500 + path.len()].copy_from_slice(path);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    for (address, service) in [
        (CREATE, Service::TlsKeyCreate),
        (DELETE, Service::TlsKeyDelete),
        (GET, Service::TlsGetSpecific),
        (SET, Service::TlsSetSpecific),
        (DLSYM, Service::Dlsym),
        (LOAD, Service::LoadStartModule),
    ] {
        native.register_service(address, service)?;
    }
    // SAFETY: caller supplies the matching locally built Rust runtime archive.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x180, 0x1a0, 0x1c0]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0u8; 64];
    assert_eq!(native.guest_thread_id(), 1);
    assert!(native.select_guest_thread(2).is_err());
    native.tls.register_thread(2)?;
    for (output, destructor) in [(0, 0), (0x100100, 0), (DATA + 63, 0), (DATA + 32, 1)] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                CREATE,
                output,
                destructor,
                &mut data
            )
            .is_err()
        );
        assert_eq!(native.tls.active_keys(), 0);
    }
    assert!(
        native
            .run(&mut image, 0x1001a0, Arguments::default(), &mut [])
            .err()
            .unwrap()
            .contains("return frame")
    );
    assert_eq!(native.tls.active_keys(), 0);
    assert_eq!(
        invoke(&mut native, &mut image, CREATE, DATA + 32, 0, &mut data)?,
        0
    );
    let mut key = u32::from_le_bytes(data[32..36].try_into().unwrap()) as u64;
    let first = 0xfedc_ba98_7654_3210;
    let second = 0x1234_5678_9abc_def0;
    assert_eq!(invoke(&mut native, &mut image, GET, key, 0, &mut data)?, 0);
    assert_eq!(
        invoke(&mut native, &mut image, SET, key, first, &mut data)?,
        0
    );
    native.select_guest_thread(2)?;
    assert_eq!(invoke(&mut native, &mut image, GET, key, 0, &mut data)?, 0);
    assert_eq!(
        invoke(&mut native, &mut image, SET, key, second, &mut data)?,
        0
    );
    native.select_guest_thread(1)?;
    assert_eq!(
        invoke(&mut native, &mut image, GET, key, 0, &mut data)?,
        first
    );
    let Execution::Dlsym { continuation, .. } = native.run(
        &mut image,
        0x100140,
        Arguments {
            size: key,
            pointer: 0,
        },
        &mut [],
    )?
    else {
        return Err("Expected TLS suspension".into());
    };
    assert_eq!(continuation.guest_thread_id(), 1);
    native.select_guest_thread(2)?;
    let resumed = native.resume_with_data(
        &mut image,
        &mut [],
        *continuation,
        0,
        &mut [DataRegion {
            address: DATA,
            bytes: &mut data,
            writable: true,
        }],
    )?;
    assert!(matches!(resumed, Execution::Returned(0)));
    assert_eq!(u64::from_le_bytes(data[56..64].try_into().unwrap()), first);
    assert_eq!(native.guest_thread_id(), 2);
    assert_eq!(
        invoke(&mut native, &mut image, GET, key, 0, &mut data)?,
        second
    );
    let mut other_file = memory_fixture();
    let mut other_code = Vec::new();
    target(&mut other_code, GET);
    other_code.push(0xc3);
    other_file[0x100..0x100 + other_code.len()].copy_from_slice(&other_code);
    // SAFETY: same matching archive for the second guest image.
    unsafe {
        native.compile_image(
            &other_file,
            0x200000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut other = Image::load(&other_file, 0x200000, 65536)?;
    for (thread, value) in [(1, first), (2, second)] {
        native.select_guest_thread(thread)?;
        assert!(matches!(
            native.run_with_data(
                &mut image,
                0x100180,
                Arguments {
                    size: key,
                    pointer: 0
                },
                &mut [&mut other],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )?,
            Execution::Returned(0)
        ));
        assert_eq!(u64::from_le_bytes(data[56..64].try_into().unwrap()), value);
    }
    assert_eq!(
        invoke(&mut native, &mut image, DELETE, key, 0, &mut data)?,
        0
    );
    assert_eq!(
        invoke(&mut native, &mut image, SET, key, second, &mut data)?,
        22
    );
    assert_eq!(
        invoke(&mut native, &mut image, DELETE, key, 0, &mut data)?,
        22
    );
    assert_eq!(
        invoke(&mut native, &mut image, CREATE, DATA + 32, 0, &mut data)?,
        0
    );
    let old_key = key;
    key = u32::from_le_bytes(data[32..36].try_into().unwrap()) as u64;
    assert_eq!(key, old_key);
    for thread in [1, 2] {
        native.select_guest_thread(thread)?;
        assert_eq!(invoke(&mut native, &mut image, GET, key, 0, &mut data)?, 0);
    }
    native.select_guest_thread(1)?;
    assert_eq!(
        invoke(&mut native, &mut image, SET, key, first, &mut data)?,
        0
    );
    // A real pending module load must not initialize its child under another
    // selected context. The rejected dispatch preserves the original request.
    let mut modules = Modules::new(65536);
    let parent = modules.load_dynamic(
        "tls-parent.prx",
        &file,
        0x100000,
        EntryPoints {
            start: Some(0x1c0),
            stop: None,
        },
    )?;
    assert!(matches!(
        modules.start_native(
            parent,
            &mut native,
            Arguments {
                size: 0x100500,
                pointer: 0
            }
        )?,
        StartEvent::LoadStartModule { .. }
    ));
    let mut child = native_ps5::modules::lifecycle_fixture(false);
    child[0x188..0x190].copy_from_slice(&0x300u64.to_le_bytes());
    let mut init = vec![0xbf];
    init.extend((key as u32).to_le_bytes());
    target(&mut init, GET);
    save(&mut init, 0x100580);
    init.push(0xc3);
    child[0x300..0x300 + init.len()].copy_from_slice(&init);
    let child_path = directory.join(format!("tls-child-{}.elf", std::process::id()));
    std::fs::write(&child_path, &child).map_err(|e| e.to_string())?;
    let mut files = ModuleFiles::default();
    files.register(
        "/app0/tls-child.prx",
        &child_path,
        "tls-child.prx",
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    let bytes = modules.bytes();
    native.select_guest_thread(2)?;
    // SAFETY: rejected before compilation, same archive contract regardless.
    assert!(
        unsafe {
            modules.dispatch_native_load(
                parent,
                &mut native,
                &mut files,
                LoadConfig {
                    bias: 0x400000,
                    instruction_limit: 1000,
                    archive,
                    directory,
                },
            )
        }
        .err()
        .unwrap()
        .contains("different guest thread")
    );
    assert_eq!(modules.bytes(), bytes);
    native.select_guest_thread(1)?;
    // SAFETY: actual child initialization executes against the matching archive.
    let progress = unsafe {
        modules.dispatch_native_load(
            parent,
            &mut native,
            &mut files,
            LoadConfig {
                bias: 0x400000,
                instruction_limit: 1000,
                archive,
                directory,
            },
        )?
    };
    assert!(matches!(
        progress,
        LoadProgress::Child {
            event: StartEvent::Started,
            ..
        }
    ));
    assert!(matches!(
        modules.finish_native_load(parent, &mut native)?,
        StartEvent::Started
    ));
    let stored = modules
        .image(parent)
        .unwrap()
        .segments
        .iter()
        .find_map(|s| {
            s.address
                .checked_sub(0x100500)
                .filter(|&offset| offset == 0)
                .map(|_| u64::from_le_bytes(s.bytes[0x80..0x88].try_into().unwrap()))
        })
        .ok_or("Missing TLS parent data")?;
    assert_eq!(stored, first);
    println!(
        "PASS: translated TLS create/get/set/delete, 64-bit values, generation reset, guarded outputs/destructors/return frame, explicit guest-context isolation, captured suspension identity, cross-image calls, and rejected foreign-thread module dispatch with successful originating-thread retry. These checks do not exercise worker scheduling; TLS destructors remain unsupported."
    );
    Ok(())
}
