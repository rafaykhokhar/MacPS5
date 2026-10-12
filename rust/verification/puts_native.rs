//! Real translated puts calls against explicit captured stdout, not host FILE.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    address: u64,
    data: &mut [u8],
) -> Result<i32, String> {
    match native.run_with_data(
        image,
        0x100100,
        Arguments {
            size: address,
            pointer: 0,
        },
        &mut [],
        &mut [DataRegion {
            address: 0x30000000,
            bytes: data,
            writable: false,
        }],
    )? {
        Execution::Returned(result) => Ok(result),
        _ => Err("Puts fixture unexpectedly suspended".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let mut code = vec![0x48, 0xb8];
    code.extend(0x722000u64.to_le_bytes());
    code.extend([0xff, 0xd0, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    native.register_service(0x722000, Service::Puts)?;
    // SAFETY: caller supplies matching locally built runtime archive.
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
    let mut image = Image::load(&file, 0x100000, 4096)?;
    let mut data = *b"abc\0\0\xff\n\0";
    assert!(
        invoke(&mut native, &mut image, 0x30000000, &mut data)
            .unwrap_err()
            .contains("not configured")
    );
    assert!(native.stdout.bytes().is_empty());
    native.stdout.configure_capture(8)?;
    assert_eq!(invoke(&mut native, &mut image, 0x30000000, &mut data)?, 10);
    data[..3].copy_from_slice(b"XYZ");
    assert_eq!(native.stdout.bytes(), b"abc\n");
    assert_eq!(invoke(&mut native, &mut image, 0x30000004, &mut data)?, 10);
    assert_eq!(invoke(&mut native, &mut image, 0x30000005, &mut data)?, 10);
    assert_eq!(native.stdout.bytes(), b"abc\n\n\xff\n\n");
    assert!(
        invoke(&mut native, &mut image, 0x30000004, &mut data)
            .unwrap_err()
            .contains("capacity")
    );
    assert_eq!(native.stdout.bytes(), b"abc\n\n\xff\n\n");
    assert_eq!(native.stdout.take_bytes(), b"abc\n\n\xff\n\n");
    for address in [0, 1, u64::MAX] {
        assert!(invoke(&mut native, &mut image, address, &mut data).is_err());
        assert!(native.stdout.bytes().is_empty());
    }
    let mut unterminated = vec![65; 65536];
    assert!(
        invoke(&mut native, &mut image, 0x30000000, &mut unterminated)
            .unwrap_err()
            .contains("bounded string limit")
    );
    assert!(native.stdout.bytes().is_empty());
    assert_eq!(invoke(&mut native, &mut image, 0x30000000, &mut data)?, 10);
    assert_eq!(native.stdout.bytes(), b"XYZ\n");
    println!(
        "PASS: translated puts returns FreeBSD newline10, captured bytes/newline/empty/nonUTF8, explicit sink policy and atomic invalid-source/unterminated/capacity failures"
    );
    Ok(())
}
