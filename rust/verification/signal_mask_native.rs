//! Translated mask state only. Signal posting, handlers and delivery are unsupported.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    scheduler::{Scheduler, Step},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 3],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<i32, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match native.run_with_data(
        image,
        0x100100,
        Arguments {
            size: ARGS,
            pointer: 0,
        },
        &mut [],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut descriptor,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable,
            },
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true,
            },
        ],
    )? {
        Execution::Returned(value) => Ok(value),
        _ => Err("Mask call did not return".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    // Load all three SysV arguments from a descriptor, then call the real service.
    let mut code = vec![
        0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48, 0xb8,
    ];
    code.extend(0x780000u64.to_le_bytes());
    code.extend([0xff, 0xd0, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    native.register_service(0x780000, Service::ThreadSigmask)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0xff; 32];
    let mut errno = 99u32.to_le_bytes();
    // NULL input ignores even an invalid how; initial thread mask is empty.
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [u64::MAX, 0, DATA + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..], &[0; 16]);
    // FreeBSD filtering is independently encoded: SIGKILL9, SIGSTOP17, SIGTHR32.
    let mut filtered = [0xff; 16];
    filtered[1] &= !1;
    filtered[2] &= !1;
    filtered[3] &= !0x80;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [3, DATA, DATA],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..16], &[0; 16]); // aliased output receives the old mask
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, 0, DATA + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..], &filtered);
    assert_eq!(errno, 99u32.to_le_bytes());
    // UNBLOCK removes bit128; BLOCK restores it without touching other words.
    data[..16].fill(0);
    data[15] = 0x80;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [2, DATA, 0],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    filtered[15] &= !0x80;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, 0, DATA + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..], &filtered);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [1, DATA, 0],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    filtered[15] |= 0x80;
    // Invalid how reads input first, leaves oldmask destination untouched, returns +EINVAL.
    let before = data;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [4, DATA, u64::MAX],
            &mut data,
            true,
            &mut errno
        )?,
        22
    );
    assert_eq!(data, before);
    assert_eq!(errno, 22u32.to_le_bytes());
    errno = 99u32.to_le_bytes();
    assert!(
        invoke(
            &mut native,
            &mut image,
            [4, u64::MAX, 0],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(errno, 99u32.to_le_bytes());
    // Guarded output failures cannot partially replace either memory or mask.
    for (length, writable) in [(31, true), (32, false)] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                [3, DATA, DATA + 16],
                &mut data[..length],
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    native.tls.register_thread(2)?;
    native.select_guest_thread(2)?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, 0, DATA + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..], &[0; 16]);
    native.select_guest_thread(1)?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, 0, DATA + 16],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[16..], &filtered);
    println!(
        "PASS: translated pthread_sigmask query/block/unblock/set, full128-bit mask,9/17/32 filtering, aliases, invalid-how ordering/errno, guarded outputs and per-thread isolation. No signal delivery claim."
    );
    verify_inheritance(archive, &directory.join("inheritance"))
}

fn call(code: &mut Vec<u8>, target: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(target.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn mask(code: &mut Vec<u8>, how: u32, input: u64, output: u64) {
    code.push(0xbf);
    code.extend(how.to_le_bytes());
    code.extend([0x48, 0xbe]);
    code.extend(input.to_le_bytes());
    code.extend([0x48, 0xba]);
    code.extend(output.to_le_bytes());
    call(code, 0x780000);
}
fn verify_inheritance(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    // This fixture has two guest functions; extend its RX PT_LOAD past child0x200.
    file[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    let mut parent = vec![0x48, 0x83, 0xec, 16];
    mask(&mut parent, 3, DATA, 0);
    parent.extend([0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x100200u64.to_le_bytes());
    parent.extend([0x31, 0xc9]);
    call(&mut parent, 0x780010);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
    call(&mut parent, 0x780020);
    mask(&mut parent, 0, 0, DATA + 48);
    parent.extend([0x48, 0x83, 0xc4, 16, 0xc3]);
    assert!(parent.len() <= 0x100);
    file[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    let mut child = Vec::new();
    // Query inherited mask, clear only the child's state, then query again.
    mask(&mut child, 0, 0, DATA + 16);
    mask(&mut child, 3, DATA + 64, 0);
    mask(&mut child, 0, 0, DATA + 32);
    child.push(0xc3);
    assert!(child.len() <= 0x100);
    file[0x200..0x200 + child.len()].copy_from_slice(&child);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
    for (address, service) in [
        (0x780000, Service::ThreadSigmask),
        (0x780010, Service::ThreadCreate),
        (0x780020, Service::ThreadJoin),
    ] {
        native.register_service(address, service)?;
    }
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut scheduler = Scheduler::new(2, 1000, 10000)?;
    scheduler.enqueue(&mut native, 2, 0x100100, 0)?;
    let mut data = [0; 80];
    data[0] = 1;
    data[15] = 0x80;
    let mut finished = false;
    for _ in 0..1000 {
        if matches!(
            scheduler.step(
                &mut native,
                &mut image,
                &mut [],
                &mut [DataRegion {
                    address: DATA,
                    bytes: &mut data,
                    writable: true
                }]
            )?,
            Step::Finished
        ) {
            finished = true;
            break;
        }
    }
    assert!(finished);
    assert_eq!(scheduler.result(2), Some(0));
    assert_eq!(&data[16..32], &data[..16]);
    assert_eq!(&data[32..48], &[0; 16]);
    assert_eq!(&data[48..64], &data[..16]);
    assert_eq!(native.tls.live_threads(), 1);
    println!(
        "PASS: translated parent sets mask; actual pthread_create child inherits bits1/128, clears its own mask, joins, and leaves parent's mask intact."
    );
    Ok(())
}
