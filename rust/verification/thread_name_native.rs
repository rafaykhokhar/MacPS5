//! Translated void pthread_set_name_np metadata; no host thread naming.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const BASE: u64 = 0x7c0000;
const DATA: u64 = 0x31000000;
fn target(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
fn call(
    n: &mut NativeExecutor,
    i: &mut Image,
    data: &mut [u8],
    thread: u64,
    pointer: u64,
) -> Result<(), String> {
    let outcome = n.run_with_data(
        i,
        0x100100,
        Arguments {
            size: thread,
            pointer,
        },
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: data,
            writable: true,
        }],
    )?;
    assert!(matches!(outcome, Execution::Returned(value) if value == BASE as i32));
    Ok(())
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let mut code = Vec::new();
    target(&mut code, BASE);
    code.push(0xc3);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut suspend = Vec::new();
    target(&mut suspend, BASE);
    target(&mut suspend, BASE + 16);
    suspend.push(0xc3);
    file[0x140..0x140 + suspend.len()].copy_from_slice(&suspend);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    n.register_service(BASE, Service::ThreadSetName)?;
    n.register_service(BASE + 16, Service::Dlsym)?;
    n.tls.register_thread(2)?;
    // SAFETY: supplied archive must match this local build.
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
    let mut data = b"worker-name-longer-than-sixteen\0".to_vec();
    call(&mut n, &mut i, &mut data, 1, DATA)?;
    let expected = b"worker-name-longer-than-sixteen";
    assert_eq!(n.tls.thread_name(1), Some(&expected[..]));
    data[0] = b'X';
    assert_eq!(n.tls.thread_name(1), Some(&expected[..]));
    assert!(call(&mut n, &mut i, &mut data, 1, 0xdeadbeef).is_err());
    assert_eq!(n.tls.thread_name(1), Some(&expected[..]));
    let len = data.len();
    data[len - 1] = b'!';
    assert!(call(&mut n, &mut i, &mut data, 1, DATA).is_err());
    assert_eq!(n.tls.thread_name(1), Some(&expected[..]));
    data[len - 1] = 0;
    call(&mut n, &mut i, &mut data, 99, u64::MAX)?;
    assert_eq!(n.tls.thread_name(2), Some(&b""[..]));
    call(&mut n, &mut i, &mut data, 1, 0)?;
    assert_eq!(n.tls.thread_name(1), Some(&b""[..]));
    let outcome = n.run_with_data(
        &mut i,
        0x100140,
        Arguments {
            size: 2,
            pointer: DATA,
        },
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut data,
            writable: false,
        }],
    )?;
    let Execution::Dlsym { continuation, .. } = outcome else {
        return Err("Expected post-name suspension".into());
    };
    let saved = n.tls.thread_name(2).unwrap().to_vec();
    data.fill(0xcc);
    n.select_guest_thread(2)?;
    assert!(matches!(
        n.resume_with_data(
            &mut i,
            &mut [],
            *continuation,
            0,
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true
            }]
        )?,
        Execution::Returned(0)
    ));
    assert_eq!(n.tls.thread_name(2), Some(saved.as_slice()));
    n.select_guest_thread(1)?;
    n.tls.retire_thread(2)?;
    call(&mut n, &mut i, &mut data, 2, u64::MAX)?;
    assert_eq!(n.tls.thread_name(2), None);
    println!(
        "PASS translated void pthread_set_name_np: copied raw bytes beyond16characters, NULLclear, ignored staleIDs, guarded faults preserve oldname, suspension retention and retirement; no host naming"
    );
    Ok(())
}
