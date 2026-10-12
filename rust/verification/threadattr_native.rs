//! Real translated pthread attribute calls; no thread creation or scheduler claim.
use native_ps5::{
    guest_memory::{DataRegion, Memory},
    guest_thread_attr::{STACK_MIN, ThreadOptions},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const SLOT: u64 = DATA + 48;
const BASE: u64 = 0x7a0000;
fn call(
    n: &mut NativeExecutor,
    i: &mut Image,
    b: &mut [u8; 64],
    service: usize,
    slot: u64,
    arg: u64,
) -> Result<i32, String> {
    for (chunk, value) in
        b[..32]
            .chunks_exact_mut(8)
            .zip([slot, arg, BASE + service as u64 * 16, 0])
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    assert!(matches!(
        n.run_with_data(
            i,
            0x100100,
            Arguments {
                size: DATA,
                pointer: 0
            },
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: b,
                writable: true
            }]
        )?,
        Execution::Returned(0)
    ));
    Ok(u64::from_le_bytes(b[24..32].try_into().unwrap()) as u32 as i32)
}
fn snapshot(
    n: &NativeExecutor,
    b: &mut [u8; 64],
    slot: u64,
) -> Result<Option<ThreadOptions>, String> {
    let mut m = Memory::default();
    m.add(DATA, b, false, false)?;
    n.thread_attributes.snapshot(&m, slot)
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let code = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x43, 16, 0xff,
        0xd0, 0x48, 0x89, 0x43, 24, 0x31, 0xc0, 0x5b, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x48, 0xb8];
    alias.extend(BASE.to_le_bytes());
    alias.extend([0xff, 0xd0, 0xc3]);
    file[0x140..0x140 + alias.len()].copy_from_slice(&alias);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    for (index, service) in [
        Service::ThreadAttrInit,
        Service::ThreadAttrDestroy,
        Service::ThreadAttrSetDetachState,
        Service::ThreadAttrSetStackSize,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(BASE + index as u64 * 16, service)?;
    }
    // SAFETY: caller supplies the matching local runtime archive.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut i = Image::load(&file, 0x100000, 65536)?;
    let mut b = [0; 64];
    assert_eq!(call(&mut n, &mut i, &mut b, 0, 0, 0)?, 22);
    assert!(
        n.run(&mut i, 0x100140, Arguments::default(), &mut [])
            .is_err()
    );
    assert!(call(&mut n, &mut i, &mut b, 0, DATA + 61, 0).is_err());
    assert!(call(&mut n, &mut i, &mut b, 0, 0x100100, 0).is_err());
    assert_eq!(n.thread_attributes.live_count(), 0);
    assert_eq!(call(&mut n, &mut i, &mut b, 0, SLOT, 0)?, 0);
    let identity = u64::from_le_bytes(b[48..56].try_into().unwrap());
    assert!(identity > u32::MAX as u64);
    assert_eq!(snapshot(&n, &mut b, SLOT)?, Some(ThreadOptions::default()));
    assert_eq!(call(&mut n, &mut i, &mut b, 2, SLOT, 2)?, 22);
    assert_eq!(call(&mut n, &mut i, &mut b, 3, SLOT, STACK_MIN - 1)?, 22);
    assert_eq!(snapshot(&n, &mut b, SLOT)?, Some(ThreadOptions::default()));
    assert_eq!(call(&mut n, &mut i, &mut b, 2, SLOT, 1)?, 0);
    assert_eq!(call(&mut n, &mut i, &mut b, 3, SLOT, STACK_MIN + 1)?, 0);
    let captured = snapshot(&n, &mut b, SLOT)?.unwrap();
    assert_eq!(call(&mut n, &mut i, &mut b, 3, SLOT, u64::MAX)?, 0);
    assert_eq!(call(&mut n, &mut i, &mut b, 2, SLOT, 0)?, 0);
    assert_eq!(
        captured,
        ThreadOptions {
            detached: true,
            stack_size: STACK_MIN + 1
        }
    );
    assert_eq!(
        snapshot(&n, &mut b, SLOT)?,
        Some(ThreadOptions {
            detached: false,
            stack_size: u64::MAX
        })
    );
    b[56..64].copy_from_slice(&identity.to_le_bytes());
    assert_eq!(call(&mut n, &mut i, &mut b, 1, SLOT, 0)?, 0);
    assert_eq!(&b[48..56], &[0; 8]);
    assert_eq!(snapshot(&n, &mut b, DATA + 56)?, None);
    assert_eq!(call(&mut n, &mut i, &mut b, 0, SLOT, 0)?, 0);
    assert_ne!(u64::from_le_bytes(b[48..56].try_into().unwrap()), identity);
    assert_eq!(call(&mut n, &mut i, &mut b, 1, DATA + 56, 0)?, 22);
    assert_eq!(n.thread_attributes.live_count(), 1);
    assert_eq!(call(&mut n, &mut i, &mut b, 1, SLOT, 0)?, 0);
    assert_eq!(n.thread_attributes.live_count(), 0);
    assert_eq!(n.bytes_allocated(), 0);
    println!(
        "PASS translated pthread attributes: full64-bit opaque handles and stack metadata, SDK minimum, detach states, stable snapshots, destroy/stale identities, readonly/short/frame output guards; no threads allocated"
    );
    Ok(())
}
