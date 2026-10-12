//! Background worker retains its origin/provider while a second module joins it.
use native_ps5::{
    module_executor::{NativeExecutor, Service, memory_fixture},
    modules::{Arguments, EntryPoints, Modules, Phase, StartEvent},
};
use std::{collections::BTreeSet, path::Path};
const VALUE: u64 = 0xfedcba9876543210;
fn call(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn finish(modules: &mut Modules, handle: u64, native: &mut NativeExecutor) -> Result<(), String> {
    for _ in 0..1000 {
        match modules.resume_native_threads(handle, native, (2, 10000, 100000), 1)? {
            StartEvent::Started => return Ok(()),
            StartEvent::ThreadRequest { .. } => {}
            _ => return Err("Unexpected background module boundary".into()),
        }
    }
    Err("Background module did not finish".into())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut provider = memory_fixture();
    provider[0x100..0x104].copy_from_slice(&[0x48, 0x8b, 0x07, 0xc3]);
    provider[0x110..0x113].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    let mut a = memory_fixture();
    a.resize(0x800, 0);
    a[56..58].copy_from_slice(&3u16.to_le_bytes());
    for offset in [152, 160] {
        a[offset..offset + 8].copy_from_slice(&0x300u64.to_le_bytes());
    }
    for (i, value) in [0x400000002u64, 0x500, 0x500, 0, 64, 64, 8]
        .into_iter()
        .enumerate()
    {
        a[176 + i * 8..184 + i * 8].copy_from_slice(&value.to_le_bytes());
    }
    for (i, (tag, value)) in [(1u64, 0u64), (5, 0x700), (10, 13), (0, 0)]
        .into_iter()
        .enumerate()
    {
        a[0x500 + i * 16..0x508 + i * 16].copy_from_slice(&tag.to_le_bytes());
        a[0x508 + i * 16..0x510 + i * 16].copy_from_slice(&value.to_le_bytes());
    }
    a[0x700..0x70d].copy_from_slice(b"provider.prx\0");
    a[0x608..0x610].copy_from_slice(&VALUE.to_le_bytes());
    let mut parent = vec![0x48, 0xbf];
    parent.extend(0x100600u64.to_le_bytes());
    parent.extend([0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x1001a0u64.to_le_bytes());
    parent.extend([0x48, 0xb9]);
    parent.extend(0x100608u64.to_le_bytes());
    call(&mut parent, 0x780000);
    parent.extend([0x31, 0xc0, 0xc3]);
    a[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    let mut child = vec![0x48, 0xb9];
    child.extend(0x100610u64.to_le_bytes());
    child.extend([0x48, 0x83, 0x39, 0, 0x74, 0xfa]);
    call(&mut child, 0x300100);
    child.push(0xc3);
    a[0x1a0..0x1a0 + child.len()].copy_from_slice(&child);
    a[0x1e0..0x1e3].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    let mut b = memory_fixture();
    let mut join = vec![0x48, 0xb9];
    join.extend(0x100610u64.to_le_bytes());
    join.extend([0x48, 0xc7, 0x01, 1, 0, 0, 0, 0x48, 0xbf]);
    join.extend(0x100600u64.to_le_bytes());
    join.extend([0x48, 0x8b, 0x3f, 0x48, 0xbe]);
    join.extend(0x200500u64.to_le_bytes());
    call(&mut join, 0x780010);
    join.push(0xc3);
    b[0x100..0x100 + join.len()].copy_from_slice(&join);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 100000, 100000)?;
    native.register_service(0x780000, Service::ThreadCreate)?;
    native.register_service(0x780010, Service::ThreadJoin)?;
    unsafe {
        for (file, bias, roots) in [
            (&provider, 0x300000, BTreeSet::from([0x100, 0x110])),
            (&a, 0x100000, BTreeSet::from([0x100, 0x1e0])),
            (&b, 0x200000, BTreeSet::from([0x100])),
        ] {
            native.compile_image(file, bias, roots, 1000, archive, directory)?;
        }
        native.set_automatic_translation(true);
    }
    let mut modules = Modules::new(65536);
    let p = modules.load_dynamic(
        "provider.prx",
        &provider,
        0x300000,
        EntryPoints {
            start: Some(0x110),
            stop: Some(0x110),
        },
    )?;
    assert!(matches!(
        modules.start_native(p, &mut native, Arguments::default())?,
        StartEvent::Started
    ));
    let first = modules.load_dynamic(
        "background.prx",
        &a,
        0x100000,
        EntryPoints {
            start: Some(0x100),
            stop: Some(0x1e0),
        },
    )?;
    assert!(matches!(
        modules.start_native(first, &mut native, Arguments::default())?,
        StartEvent::ThreadRequest { .. }
    ));
    finish(&mut modules, first, &mut native)?;
    assert_eq!(modules.phase(first), Some(Phase::Started));
    assert_eq!(native.tls.live_threads(), 2);
    for handle in [first, p] {
        assert!(
            modules
                .stop(handle, &mut native, Arguments::default())
                .unwrap_err()
                .contains("worker pins")
        );
        assert!(
            modules
                .unload_native(handle, &mut native)
                .unwrap_err()
                .contains("worker pins")
        );
    }
    for _ in 0..4 {
        modules.step_native_workers(first, &mut native)?;
    }
    let second = modules.load_dynamic(
        "joiner.prx",
        &b,
        0x200000,
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    assert!(matches!(
        modules.start_native(second, &mut native, Arguments::default())?,
        StartEvent::ThreadRequest { .. }
    ));
    finish(&mut modules, second, &mut native)?;
    let image = modules.image(second).unwrap();
    let data = image
        .segments
        .iter()
        .find(|s| s.address == 0x200500)
        .unwrap();
    assert_eq!(
        u64::from_le_bytes(data.bytes[..8].try_into().unwrap()),
        VALUE
    );
    assert_eq!(native.tls.live_threads(), 1);
    assert_eq!(native.guest_thread_id(), 1);
    modules.stop(first, &mut native, Arguments::default())?;
    modules.unload_native(first, &mut native)?;
    modules.stop(p, &mut native, Arguments::default())?;
    modules.unload_native(p, &mut native)?;
    let mut replacement = memory_fixture();
    let mut code = vec![0x48, 0xb8];
    code.extend(0x100500u64.to_le_bytes());
    code.extend([0x48, 0xc7, 0x00, 77, 0, 0, 0, 0x31, 0xc0, 0xc3]);
    replacement[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Same bias, different guest bytes: native unload must have removed both
    // old blocks and their immutable source before this new compilation.
    unsafe {
        native.compile_image(
            &replacement,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
    }
    let reused = modules.load_dynamic(
        "replacement.prx",
        &replacement,
        0x100000,
        EntryPoints {
            start: Some(0x100),
            stop: None,
        },
    )?;
    assert!(reused > first);
    assert!(matches!(
        modules.start_native(reused, &mut native, Arguments::default())?,
        StartEvent::Started
    ));
    let data = modules
        .image(reused)
        .unwrap()
        .segments
        .iter()
        .find(|s| s.address == 0x100500)
        .unwrap();
    assert_eq!(u64::from_le_bytes(data.bytes[..8].try_into().unwrap()), 77);
    println!(
        "Background leases verified: parent handback with live worker, origin/provider stop+unload pins, explicit background progress, second-module join with original primary image and provider call, full64 result and retirement releases pins"
    );
    Ok(())
}
