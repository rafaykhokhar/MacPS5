//! Translated libc signal-set calls only: no signal delivery or pthread_sigmask.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const SET: u64 = 0x31000000;
const ERRNO: u64 = 0x32000000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    entry: u64,
    pointer: u64,
    signum: u64,
    set: &mut [u8],
    set_writable: bool,
    errno: &mut [u8],
    errno_writable: bool,
) -> Result<i32, String> {
    let outcome = native.run_with_data(
        image,
        entry,
        Arguments {
            size: pointer,
            pointer: signum,
        },
        &mut [],
        &mut [
            DataRegion {
                address: SET,
                bytes: set,
                writable: set_writable,
            },
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: errno_writable,
            },
        ],
    )?;
    match outcome {
        Execution::Returned(value) => Ok(value),
        _ => Err("Signal-set call did not return".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    for (offset, target) in [(0x100, 0x780000u64), (0x120, 0x780010)] {
        let mut code = vec![0x48, 0xb8];
        code.extend(target.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    // Tailcall with output exactly at live returnword must fail before mutation.
    let mut alias = vec![0x48, 0x89, 0xe7, 0x48, 0xb8];
    alias.extend(0x780000u64.to_le_bytes());
    alias.extend([0xff, 0xe0]);
    file[0x140..0x140 + alias.len()].copy_from_slice(&alias);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    native.register_service(0x780000, Service::Sigemptyset)?;
    native.register_service(0x780010, Service::Sigaddset)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x120, 0x140]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut set = [0xa5; 16];
    let mut errno = 99u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            SET,
            0,
            &mut set,
            true,
            &mut errno,
            true
        )?,
        0
    );
    assert_eq!(set, [0; 16]);
    assert_eq!(errno, 99u32.to_le_bytes());
    let mut expected = [0u8; 16];
    for signum in [1u64, 32, 33, 64, 65, 96, 97, 128, 128] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x100120,
                SET,
                0xdeadbeef00000000 | signum,
                &mut set,
                true,
                &mut errno,
                true
            )?,
            0
        );
        expected[(signum as usize - 1) / 8] |= 1 << ((signum - 1) % 8);
        assert_eq!(set, expected);
        assert_eq!(errno, 99u32.to_le_bytes());
    }
    for signum in [0u64, u64::MAX, 129, 0x80000000, 0x7fffffff] {
        errno = 99u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x100120,
                u64::MAX,
                signum,
                &mut set,
                true,
                &mut errno,
                true
            )?,
            -1
        );
        assert_eq!(set, expected);
        assert_eq!(errno, 22u32.to_le_bytes());
    }
    for entry in [0x100100, 0x100120] {
        let mut short = [0xa5; 15];
        assert!(
            invoke(
                &mut native,
                &mut image,
                entry,
                SET,
                1,
                &mut short,
                true,
                &mut errno,
                true
            )
            .is_err()
        );
        assert_eq!(short, [0xa5; 15]);
        let before = set;
        assert!(
            invoke(
                &mut native,
                &mut image,
                entry,
                SET,
                1,
                &mut set,
                false,
                &mut errno,
                true
            )
            .is_err()
        );
        assert_eq!(set, before);
    }
    errno = 99u32.to_le_bytes();
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100120,
            SET,
            0,
            &mut set,
            true,
            &mut errno,
            false
        )
        .is_err()
    );
    assert_eq!(errno, 99u32.to_le_bytes());
    assert_eq!(set, expected);
    let error = invoke(
        &mut native,
        &mut image,
        0x100140,
        SET,
        0,
        &mut set,
        true,
        &mut errno,
        true,
    )
    .expect_err("Return frame alias rejected");
    assert!(error.contains("return frame"), "{error}");
    assert_eq!(set, expected);
    let mut short_errno = [0x63u8; 3];
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100120,
            SET,
            0,
            &mut set,
            true,
            &mut short_errno,
            true
        )
        .is_err()
    );
    assert_eq!(short_errno, [0x63; 3]);
    assert_eq!(set, expected);
    native.kernel = Some(Kernel::new(4096, 0x900ff0)?);
    let error = invoke(
        &mut native,
        &mut image,
        0x100120,
        SET,
        0,
        &mut set,
        true,
        &mut errno,
        true,
    )
    .expect_err("Errno may not overwrite live return word");
    assert!(error.contains("return frame"), "{error}");
    assert_eq!(set, expected);
    println!(
        "PASS: translated sigemptyset/sigaddset16byte ABI, allword boundaries/high32 truncation, invalidsignum -1/errno22 beforepointer access, unchanged successerrno, readonly/15byte/returnframe guards; no signalmask/delivery claimed"
    );
    Ok(())
}
