//! Cancellation mode only; request APIs and delivery remain unsupported.
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
fn call(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn setter(code: &mut Vec<u8>, kind: u32, output: u64) {
    code.push(0xbf);
    code.extend(kind.to_le_bytes());
    code.extend([0x48, 0xbe]);
    code.extend(output.to_le_bytes());
    call(code, 0x780000);
}
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 2],
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
        Execution::Returned(v) => Ok(v),
        _ => Err("Cancellation setter did not return".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let mut code = vec![0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f];
    call(&mut code, 0x780000);
    code.push(0xc3);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    native.register_service(0x780000, Service::ThreadSetCancelType)?;
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
    let mut data = [0x55; 8];
    let mut errno = 99u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [2, DATA],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(data, [0, 0, 0, 0, 0x55, 0x55, 0x55, 0x55]);
    assert_eq!(native.tls.cancel_type(1)?, 2);
    for invalid in [1, u64::MAX, 3] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                [invalid, u64::MAX],
                &mut data,
                false,
                &mut errno
            )?,
            22
        );
    }
    for (len, writable) in [(3, true), (8, false)] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                [0, DATA],
                &mut data[..len],
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(native.tls.cancel_type(1)?, 2);
    }
    // Initial RSP points at sentinel; service call adds its own 8-byte return slot.
    assert!(
        invoke(
            &mut native,
            &mut image,
            [0, 0x900000 + 4096 - 16],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(native.tls.cancel_type(1)?, 2);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, DATA],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..4], &2u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0x1_0000_0002, 0],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    native.tls.register_thread(2)?;
    native.select_guest_thread(2)?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [2, DATA],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[..4], &0u32.to_le_bytes());
    native.select_guest_thread(1)?;
    assert_eq!(native.tls.cancel_type(1)?, 2);
    assert_eq!(errno, 99u32.to_le_bytes());
    verify_create(archive, &directory.join("create"))?;
    println!(
        "PASS: cancellation mode0/2, previous4B/NULL, low32 ABI, EINVAL preserves errno/output, guarded/frame outputs, per-thread isolation and actual create resets mode. Requests/delivery unsupported."
    );
    Ok(())
}
fn verify_create(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    file[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    let mut parent = vec![0x48, 0x83, 0xec, 16];
    setter(&mut parent, 2, DATA);
    parent.extend([0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba]);
    parent.extend(0x100200u64.to_le_bytes());
    parent.extend([0x31, 0xc9]);
    call(&mut parent, 0x780010);
    parent.extend([0x48, 0x8b, 0x3c, 0x24, 0x31, 0xf6]);
    call(&mut parent, 0x780020);
    setter(&mut parent, 0, DATA + 8);
    parent.extend([0x48, 0x83, 0xc4, 16, 0xc3]);
    file[0x100..0x100 + parent.len()].copy_from_slice(&parent);
    let mut child = Vec::new();
    setter(&mut child, 2, DATA + 4);
    child.push(0xc3);
    file[0x200..0x200 + child.len()].copy_from_slice(&child);
    let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
    for (a, s) in [
        (0x780000, Service::ThreadSetCancelType),
        (0x780010, Service::ThreadCreate),
        (0x780020, Service::ThreadJoin),
    ] {
        native.register_service(a, s)?;
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
    let mut data = [0xff; 12];
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
    assert_eq!(&data[..8], &[0; 8]);
    assert_eq!(&data[8..], &2u32.to_le_bytes());
    assert_eq!(native.tls.live_threads(), 1);
    Ok(())
}
