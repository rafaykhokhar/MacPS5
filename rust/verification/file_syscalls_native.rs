//! Translated read-only registered-file syscalls; no host filesystem forwarding.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_3100_0001;
const BRIDGE: u64 = 0x71000a;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    data: &mut [u8],
    number: u64,
    args: [u64; 3],
) -> Result<(u64, bool), String> {
    for (i, v) in [number, args[0], args[1], args[2]].into_iter().enumerate() {
        data[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    assert!(matches!(
        native.run_worker_with_data(
            image,
            0x100100,
            0,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: data,
                writable: true
            }]
        )?,
        Execution::WorkerReturned(0)
    ));
    Ok((
        u64::from_le_bytes(data[32..40].try_into().unwrap()),
        data[40] != 0,
    ))
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let mut code = vec![0x48, 0xbb];
    code.extend(DATA.to_le_bytes());
    code.extend([
        0x48, 0x8b, 0x03, 0x48, 0x8b, 0x7b, 8, 0x48, 0x8b, 0x73, 16, 0x48, 0x8b, 0x53, 24, 0x49,
        0xbb,
    ]);
    code.extend(BRIDGE.to_le_bytes());
    code.extend([
        0x41, 0xff, 0xd3, 0x48, 0x89, 0x43, 32, 0x0f, 0x92, 0x43, 40, 0x31, 0xc0, 0xc3,
    ]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 65536, 1000, 1000)?;
    let mut kernel = Kernel::new(4096, DATA + 120)?;
    kernel.files.register(b"/fixture", b"abcdef".to_vec())?;
    native.kernel = Some(kernel);
    native.register_service(BRIDGE, Service::Syscall)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            100,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0xa5; 128];
    data[64..73].copy_from_slice(b"/fixture\0");
    data[120..124].copy_from_slice(&77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            &mut data,
            5,
            [DATA + 64, 0x1_0000_0000, 0]
        )?,
        (3, false)
    );
    assert_eq!(&data[120..124], &77u32.to_le_bytes());
    let token = native
        .kernel
        .as_ref()
        .unwrap()
        .descriptors
        .file_token(3)
        .unwrap();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            &mut data,
            3,
            [0x1_0000_0003, DATA + 80, 3]
        )?,
        (3, false)
    );
    assert_eq!(&data[80..83], b"abc");
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 3, [3, DATA + 127, 3])?,
        (u64::MAX, true)
    );
    assert_eq!(&data[120..124], &14u32.to_le_bytes());
    assert_eq!(data[127], 0xa5);
    assert_eq!(
        native
            .kernel
            .as_ref()
            .unwrap()
            .descriptors
            .file_position(&token),
        Ok(3)
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            &mut data,
            478,
            [0x1_0000_0003, (-1i64) as u64, 0x1_0000_0002]
        )?,
        (5, false)
    );
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 3, [3, DATA + 127, 100])?,
        (1, false)
    );
    assert_eq!(data[127], b'f');
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 3, [3, u64::MAX, 100])?,
        (0, false)
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            &mut data,
            478,
            [3, i64::MAX as u64, 1]
        )?,
        (u64::MAX, true)
    );
    assert_eq!(&data[120..124], &84u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            &mut data,
            3,
            [u64::MAX, DATA + 80, u64::MAX]
        )?,
        (u64::MAX, true)
    );
    assert_eq!(&data[120..124], &22u32.to_le_bytes());
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 6, [0x1_0000_0003, 0, 0])?,
        (0, false)
    );
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 5, [DATA + 64, 0, 0])?,
        (3, false)
    );
    assert_eq!(
        native
            .kernel
            .as_mut()
            .unwrap()
            .descriptors
            .close_file(&token),
        9
    );
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 3, [3, DATA + 80, 2])?,
        (2, false)
    );
    assert_eq!(&data[80..82], b"ab");
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 6, [3, 0, 0])?,
        (0, false)
    );
    assert_eq!(
        invoke(&mut native, &mut image, &mut data, 6, [3, 0, 0])?,
        (u64::MAX, true)
    );
    assert_eq!(&data[120..124], &9u32.to_le_bytes());
    println!(
        "PASS: translated registered-file open/read/lseek/close, shared offsets, high-bit int truncation, signed64 seeks, errno/carry, guarded EFAULT without cursor advance, EOF and stale descriptor reuse."
    );
    Ok(())
}
