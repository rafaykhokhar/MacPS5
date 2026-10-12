//! Translated mutex calls and explicit cooperative lock suspension; no scheduler.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const ATTR: u64 = DATA + 40;
const SLOT: u64 = DATA + 48;
const BASE: u64 = 0x790000;
fn call(
    n: &mut NativeExecutor,
    i: &mut Image,
    b: &mut [u8; 64],
    service: usize,
    a: u64,
    c: u64,
) -> Result<Execution, String> {
    for (chunk, value) in
        b[..32]
            .chunks_exact_mut(8)
            .zip([a, c, BASE + service as u64 * 16, u64::MAX])
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    n.run_with_data(
        i,
        0x100100,
        Arguments {
            size: DATA,
            pointer: 0,
        },
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: b,
            writable: true,
        }],
    )
}
fn status(
    n: &mut NativeExecutor,
    i: &mut Image,
    b: &mut [u8; 64],
    service: usize,
    a: u64,
    c: u64,
) -> Result<i32, String> {
    assert!(matches!(
        call(n, i, b, service, a, c)?,
        Execution::Returned(0)
    ));
    Ok(u64::from_le_bytes(b[24..32].try_into().unwrap()) as u32 as i32)
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let code = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x43, 16, 0xff,
        0xd0, 0x48, 0x89, 0x43, 24, 0x31, 0xc0, 0x5b, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Output exactly aliases live call frame: reject before allocating an object.
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x48, 0xb8];
    alias.extend(BASE.to_le_bytes());
    alias.extend([0xff, 0xd0, 0xc3]);
    file[0x140..0x140 + alias.len()].copy_from_slice(&alias);
    let mut tail = vec![0x48, 0xb8];
    tail.extend((BASE + 80).to_le_bytes());
    tail.extend([0xff, 0xe0]);
    file[0x180..0x180 + tail.len()].copy_from_slice(&tail);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    for (index, service) in [
        Service::MutexAttrInit,
        Service::MutexAttrSetType,
        Service::MutexAttrDestroy,
        Service::MutexInit,
        Service::MutexDestroy,
        Service::MutexLock,
        Service::MutexTryLock,
        Service::MutexUnlock,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(BASE + index as u64 * 16, service)?;
    }
    // SAFETY: matching locally built runtime archive is supplied by the verifier.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x180]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut i = Image::load(&file, 0x100000, 65536)?;
    n.tls.register_thread(2)?;
    let mut b = [0; 64];
    assert!(
        n.run(&mut i, 0x100140, Arguments::default(), &mut [])
            .is_err()
    );
    assert_eq!(n.mutex.attribute_count(), 0);
    assert!(status(&mut n, &mut i, &mut b, 0, DATA + 61, 0).is_err());
    assert!(status(&mut n, &mut i, &mut b, 0, 0x100100, 0).is_err());
    assert_eq!(n.mutex.attribute_count(), 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 0, ATTR, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 1, ATTR, 2)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 3, SLOT, ATTR)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 2, ATTR, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 5, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 5, SLOT, 0)?, 0);
    n.select_guest_thread(2)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 6, SLOT, 0)?, 16);
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 1);
    let Execution::MutexWait(mut wait) = call(&mut n, &mut i, &mut b, 5, SLOT, 0)? else {
        return Err("Expected real mutex suspension".into());
    };
    let mut other = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    assert!(
        matches!(other.resume_mutex(&mut i, &mut [], &mut wait), Err(error) if error.contains("another executor"))
    );
    assert_eq!(wait.guest_thread_id(), 2);
    assert_eq!(wait.slot_address(), SLOT);
    assert!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )?
        .is_none()
    );
    n.select_guest_thread(1)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    assert!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )?
        .is_none()
    );
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    // Corrupt and repair the saved live frame. Acquisition must not precede validation.
    let mut frame = 0;
    n.with_mutex_wait_memory(
        &mut wait,
        &mut i,
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut b,
            writable: true,
        }],
        |m| {
            for address in (0x900000..0x901000).step_by(8) {
                if m.load(address, 8)? == 0x100111 {
                    frame = address;
                    break;
                }
            }
            if frame == 0 {
                return Err("Saved mutex return frame missing".into());
            }
            m.write(frame, &0u64.to_le_bytes())
        },
    )?;
    assert!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )
        .is_err()
    );
    assert!(!wait.is_consumed());
    assert_eq!(status(&mut n, &mut i, &mut b, 6, SLOT, 0)?, 0); // thread1 can still acquire
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    n.with_mutex_wait_memory(
        &mut wait,
        &mut i,
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut b,
            writable: true,
        }],
        |m| m.write(frame, &0x100111u64.to_le_bytes()),
    )?;
    let segment = i
        .segments
        .iter_mut()
        .find(|s| s.address <= 0x100111 && s.address + s.bytes.len() as u64 > 0x100111)
        .unwrap();
    let code_index = (0x100111 - segment.address) as usize;
    let original = segment.bytes[code_index];
    segment.bytes[code_index] ^= 1;
    assert!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )
        .is_err()
    );
    assert!(!wait.is_consumed());
    i.segments
        .iter_mut()
        .find(|s| s.address <= 0x100111 && s.address + s.bytes.len() as u64 > 0x100111)
        .unwrap()
        .bytes[code_index] = original;
    assert!(matches!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )?,
        Some(Execution::Returned(0))
    ));
    assert!(wait.is_consumed());
    assert_eq!(n.guest_thread_id(), 1);
    assert!(n.resume_mutex(&mut i, &mut [], &mut wait).is_err());
    assert_eq!(status(&mut n, &mut i, &mut b, 6, SLOT, 0)?, 16);
    n.select_guest_thread(2)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    // A stale ticket must not acquire a replacement object, even at the same slot.
    n.select_guest_thread(1)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 5, SLOT, 0)?, 0);
    n.select_guest_thread(2)?;
    let Execution::MutexWait(mut stale) = call(&mut n, &mut i, &mut b, 5, SLOT, 0)? else {
        unreachable!()
    };
    n.select_guest_thread(1)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 4, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 3, SLOT, 0)?, 0);
    assert!(matches!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut stale,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )?,
        Some(Execution::Returned(0))
    ));
    assert_eq!(u64::from_le_bytes(b[24..32].try_into().unwrap()), 22);
    assert_eq!(status(&mut n, &mut i, &mut b, 6, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 4, SLOT, 0)?, 0);
    // Tail-call suspension has a sentinel return, not a resumable code block.
    assert_eq!(status(&mut n, &mut i, &mut b, 3, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 5, SLOT, 0)?, 0);
    n.select_guest_thread(2)?;
    let Execution::MutexWait(mut tail_wait) = n.run_with_data(
        &mut i,
        0x100180,
        Arguments {
            size: SLOT,
            pointer: 0,
        },
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut b,
            writable: true,
        }],
    )?
    else {
        return Err("Tail lock did not suspend".into());
    };
    n.select_guest_thread(1)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    assert!(matches!(
        n.resume_mutex_with_data(
            &mut i,
            &mut [],
            &mut tail_wait,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: true
            }]
        )?,
        Some(Execution::Returned(0))
    ));
    n.select_guest_thread(2)?;
    assert_eq!(status(&mut n, &mut i, &mut b, 7, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut i, &mut b, 4, SLOT, 0)?, 0);
    nested(archive, directory)?;
    println!(
        "PASS native mutex: SDL recursive attributes, two explicit contexts, pending retries, frame precommit, captured identity, replay rejection, stale generation, bounded output guards"
    );
    Ok(())
}
fn target(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn nested(archive: &Path, directory: &Path) -> Result<(), String> {
    use native_ps5::{
        module_files::ModuleFiles,
        modules::{EntryPoints, LoadConfig, LoadProgress, Modules, StartEvent},
    };
    let mut file = memory_fixture();
    let invoke = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x43, 16, 0xff,
        0xd0, 0x48, 0x89, 0x43, 24, 0x31, 0xc0, 0x5b, 0xc3,
    ];
    file[0x100..0x100 + invoke.len()].copy_from_slice(&invoke);
    let mut load = Vec::new();
    target(&mut load, BASE + 128);
    load.extend([0x31, 0xc0, 0xc3]);
    file[0x1c0..0x1c0 + load.len()].copy_from_slice(&load);
    let path = b"/app0/mutex-child.prx\0";
    file[0x500..0x500 + path.len()].copy_from_slice(path);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    for (index, service) in [
        (3, Service::MutexInit),
        (5, Service::MutexLock),
        (7, Service::MutexUnlock),
        (8, Service::LoadStartModule),
    ] {
        n.register_service(BASE + index * 16, service)?;
    }
    n.tls.register_thread(2)?;
    // SAFETY: local archive matches this runtime.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x1c0]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut owner_image = Image::load(&file, 0x100000, 65536)?;
    let mut b = [0; 64];
    assert_eq!(status(&mut n, &mut owner_image, &mut b, 3, SLOT, 0)?, 0);
    assert_eq!(status(&mut n, &mut owner_image, &mut b, 5, SLOT, 0)?, 0);
    // Copy the opaque pointer-object handle into the parent's writable data.
    // Both slots refer to the same owned mutex; neither contains a host pointer.
    file[0x580..0x588].copy_from_slice(&b[48..56]);
    let mut modules = Modules::new(65536);
    let parent = modules.load_dynamic(
        "mutex-parent.prx",
        &file,
        0x100000,
        EntryPoints {
            start: Some(0x1c0),
            stop: None,
        },
    )?;
    n.select_guest_thread(2)?;
    assert!(matches!(
        modules.start_native(
            parent,
            &mut n,
            Arguments {
                size: 0x100500,
                pointer: 0
            }
        )?,
        StartEvent::LoadStartModule { .. }
    ));
    let mut child = native_ps5::modules::lifecycle_fixture(false);
    child[0x188..0x190].copy_from_slice(&0x300u64.to_le_bytes());
    let mut init = vec![0x48, 0xbf];
    init.extend(0x100580u64.to_le_bytes());
    target(&mut init, BASE + 80);
    init.extend([0x48, 0xb9]);
    init.extend(0x100588u64.to_le_bytes());
    init.extend([0x48, 0x83, 0x01, 0x01, 0xc3]);
    child[0x300..0x300 + init.len()].copy_from_slice(&init);
    let child_path = directory.join(format!("mutex-child-{}.elf", std::process::id()));
    std::fs::write(&child_path, &child).map_err(|e| e.to_string())?;
    let mut files = ModuleFiles::default();
    files.register(
        "/app0/mutex-child.prx",
        &child_path,
        "mutex-child.prx",
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    // SAFETY: translated child uses the matching runtime archive.
    let progress = unsafe {
        modules.dispatch_native_load(
            parent,
            &mut n,
            &mut files,
            LoadConfig {
                bias: 0x400000,
                instruction_limit: 2000,
                archive,
                directory,
            },
        )?
    };
    let LoadProgress::Child {
        handle: child,
        event: StartEvent::MutexWait {
            thread: 2,
            slot: 0x100580,
        },
    } = progress
    else {
        return Err("Child initializer did not remain suspended".into());
    };
    assert!(modules.resume_native(child, &mut n, 0).is_err());
    assert!(modules.finish_native_load(parent, &mut n).is_err());
    assert!(matches!(
        modules.resume_native_mutex(child, &mut n)?,
        StartEvent::MutexWait { .. }
    ));
    modules.with_pending_memory(child, &mut n, |m| {
        assert_eq!(m.load(0x100588, 8)?, 0);
        Ok(())
    })?;
    n.select_guest_thread(1)?;
    assert!(modules.resume_native_mutex(child, &mut n).is_err());
    assert_eq!(status(&mut n, &mut owner_image, &mut b, 7, SLOT, 0)?, 0);
    n.select_guest_thread(2)?;
    assert!(matches!(
        modules.resume_native_mutex(child, &mut n)?,
        StartEvent::Started
    ));
    assert!(modules.resume_native_mutex(child, &mut n).is_err());
    assert!(matches!(
        modules.finish_native_load(parent, &mut n)?,
        StartEvent::Started
    ));
    let segment = modules
        .image(parent)
        .unwrap()
        .segments
        .iter()
        .find(|s| s.address == 0x100500)
        .unwrap();
    assert_eq!(
        u64::from_le_bytes(segment.bytes[0x88..0x90].try_into().unwrap()),
        1
    );
    println!(
        "PASS nested mutex: child initializer remains pending, generic/foreign-thread resumes reject, owner unlock enables checked resume and parent finishes exactly once"
    );
    Ok(())
}
