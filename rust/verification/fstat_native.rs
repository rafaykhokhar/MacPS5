//! Callable fstat with registered descriptor metadata and guarded output.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_3200_0001;
const SERVICE: u64 = 0x720000;
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    file[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    let mut code = vec![0x48, 0xbb];
    code.extend(DATA.to_le_bytes());
    code.extend([0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0xb8]);
    code.extend(SERVICE.to_le_bytes());
    code.extend([0xff, 0xd0, 0x48, 0x89, 0x43, 16, 0x31, 0xc0, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut frame = vec![0xbf, 3, 0, 0, 0, 0x48, 0x8d, 0x74, 0x24, 0xf8, 0x48, 0xb8];
    frame.extend(SERVICE.to_le_bytes());
    frame.extend([0xff, 0xd0, 0xc3]);
    file[0x200..0x200 + frame.len()].copy_from_slice(&frame);
    let mut n = NativeExecutor::new(0x900000, 65536, 1000, 1000)?;
    let mut k = Kernel::new(4096, DATA + 32)?;
    k.files.register(b"/fixture", vec![0x5a; 513])?;
    let token = k.descriptors.open_file(&k.files, b"/fixture").unwrap();
    let expected = k.descriptors.stat_file(&token).unwrap();
    k.descriptors.seek_file(&token, 99, 0).unwrap();
    n.kernel = Some(k);
    n.register_service(SERVICE, Service::Fstat)?;
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            100,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    for (fd, output, errno) in [
        (0x1_0000_0003u64, DATA + 65, 0u32),
        (3, 0x100100, 14),
        (3, DATA + 450, 14),
        (3, u64::MAX, 14),
        (3, 0, 14),
        (u64::MAX, u64::MAX, 9),
    ] {
        let mut data = [0xa5; 512];
        data[..8].copy_from_slice(&fd.to_le_bytes());
        data[8..16].copy_from_slice(&output.to_le_bytes());
        data[32..36].copy_from_slice(&77u32.to_le_bytes());
        let before = data;
        let outcome = n.run_worker_with_data(
            &mut image,
            0x100100,
            0,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        );
        if fd == 3 && output == u64::MAX {
            // Pointer arithmetic overflow is an explicit callable-service guard
            // boundary, distinct from an ordinary mapped-range EFAULT.
            assert!(matches!(outcome, Err(ref error) if error.contains("output overflow")));
            assert_eq!(data, before);
            assert_eq!(
                n.kernel.as_ref().unwrap().descriptors.file_position(&token),
                Ok(99)
            );
            continue;
        }
        assert!(matches!(outcome?, Execution::WorkerReturned(0)));
        assert_eq!(
            &data[16..24],
            &if errno == 0 { 0u64 } else { u32::MAX as u64 }.to_le_bytes()
        );
        assert_eq!(
            &data[32..36],
            &if errno == 0 { 77u32 } else { errno }.to_le_bytes()
        );
        if errno == 0 {
            assert_eq!(&data[65..185], &expected);
        } else {
            assert!(data[64..].iter().all(|b| *b == 0xa5));
        }
        assert_eq!(
            n.kernel.as_ref().unwrap().descriptors.file_position(&token),
            Ok(99)
        );
    }
    let mut data = [0xa5; 512];
    data[32..36].copy_from_slice(&77u32.to_le_bytes());
    assert!(
        n.run_worker_with_data(
            &mut image,
            0x100200,
            0,
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true
            }]
        )
        .is_err()
    );
    assert_eq!(&data[32..36], &77u32.to_le_bytes());
    assert_eq!(
        n.kernel.as_ref().unwrap().descriptors.file_position(&token),
        Ok(99)
    );
    println!(
        "PASS: callable fstat serializes the 120-byte guest ABI with file identity/size, preserves descriptor cursor and success errno, truncates fd int, and reports guarded faults/badfd in reference order."
    );
    Ok(())
}
