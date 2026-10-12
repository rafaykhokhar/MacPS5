//! Callable path stat: full-width guest pointers and guarded output/error ordering.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_3400_0001;
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
    let mut frame = vec![0x48, 0xbf];
    frame.extend((DATA + 256).to_le_bytes());
    frame.extend([0x48, 0x8d, 0x74, 0x24, 0xf8, 0x48, 0xb8]);
    frame.extend(SERVICE.to_le_bytes());
    frame.extend([0xff, 0xd0, 0xc3]);
    file[0x200..0x200 + frame.len()].copy_from_slice(&frame);
    let mut native = NativeExecutor::new(0x900000, 65536, 1000, 1000)?;
    let mut kernel = Kernel::new(4096, DATA + 32)?;
    kernel.files.register(b"/fixture", vec![0x5a; 513])?;
    kernel.files.set_stat_tracing(true);
    native.kernel = Some(kernel);
    native.register_service(SERVICE, Service::Stat)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            100,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    for (path, output, errno, overflow) in [
        (DATA + 256, DATA + 65, 0, false),
        // stat consumes the pathname before writing its record, so overlap is legal.
        (DATA + 256, DATA + 256, 0, false),
        (DATA + 256, 0x100100, 14, false),
        (DATA + 256, DATA + 450, 14, false),
        (DATA + 256, 0, 14, false),
        (DATA + 256, u64::MAX, 0, true),
        (DATA + 280, u64::MAX, 2, false),
        (u64::MAX, u64::MAX, 14, false),
        (0, DATA + 65, 14, false),
    ] {
        let mut data = [0xa5; 512];
        data[..8].copy_from_slice(&path.to_le_bytes());
        data[8..16].copy_from_slice(&output.to_le_bytes());
        data[32..36].copy_from_slice(&77u32.to_le_bytes());
        data[256..265].copy_from_slice(b"/fixture\0");
        data[280..289].copy_from_slice(b"/missing\0");
        let before = data;
        let result = native.run_worker_with_data(
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
        if overflow {
            assert!(matches!(result,Err(ref e) if e.contains("output overflow")));
            assert_eq!(data, before);
            continue;
        }
        assert!(matches!(result?, Execution::WorkerReturned(0)));
        assert_eq!(
            &data[16..24],
            &if errno == 0 { 0u64 } else { u32::MAX as u64 }.to_le_bytes()
        );
        assert_eq!(
            &data[32..36],
            &if errno == 0 { 77u32 } else { errno }.to_le_bytes()
        );
        if errno == 0 {
            let start = (output - DATA) as usize;
            let stat = &data[start..start + 120];
            assert_eq!(&stat[8..10], &0o100444u16.to_le_bytes());
            assert_eq!(&stat[72..80], &513i64.to_le_bytes());
            assert_eq!(&stat[80..88], &2i64.to_le_bytes());
            assert_eq!(&stat[88..92], &512u32.to_le_bytes());
            for index in 0..data.len() {
                if !(start..start + 120).contains(&index) && !(16..24).contains(&index) {
                    assert_eq!(data[index], before[index], "unexpected write at {index}");
                }
            }
        } else {
            assert_eq!(&data[64..], &before[64..]);
        }
    }
    let mut data = [0xa5; 512];
    data[256..265].copy_from_slice(b"/fixture\0");
    data[32..36].copy_from_slice(&77u32.to_le_bytes());
    let before = data;
    let result = native.run_worker_with_data(
        &mut image,
        0x100200,
        0,
        &mut [],
        &mut [DataRegion {
            address: DATA,
            bytes: &mut data,
            writable: true,
        }],
    );
    assert!(result.is_err());
    assert_eq!(data, before);
    println!(
        "PASS callable stat: full64 paths/unaligned and pathname-overlapping outputs, 120-byte ABI size/mode, success errno, pathname-before-output errors and live return-frame guards"
    );
    Ok(())
}
