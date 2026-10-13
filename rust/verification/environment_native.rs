//! Real guest setenv/getenv, including stable values and writable owned storage.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
const SET: u64 = 0x780000;
const GET: u64 = 0x780010;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: &mut [u8],
    entry: u64,
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match native.run_worker_with_data(
        image,
        entry,
        ARGS,
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
                writable: true,
            },
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Environment wrapper did not return".into()),
    }
}
fn string(data: &mut [u8], offset: usize, value: &[u8]) {
    data[offset..offset + value.len()].copy_from_slice(value);
    data[offset + value.len()] = 0;
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    file[96..104].copy_from_slice(&0x200u64.to_le_bytes());
    file[104..112].copy_from_slice(&0x200u64.to_le_bytes());
    // Arguments descriptor, indirect call, full pointer result.
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Read a byte through a saved getenv pointer using translated guest code.
    let read = [0x48, 0x8b, 0x3f, 0x0f, 0xb6, 0x07, 0xc3];
    file[0x180..0x180 + read.len()].copy_from_slice(&read);
    // Write a byte through a saved pointer, then return that byte.
    let edit = [
        0x48, 0x8b, 0x47, 8, 0x48, 0x8b, 0x3f, 0x88, 0x07, 0x0f, 0xb6, 0x07, 0xc3,
    ];
    file[0x1a0..0x1a0 + edit.len()].copy_from_slice(&edit);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    native.environment = DATA;
    native.register_service(SET, Service::Setenv)?;
    native.register_service(GET, Service::Getenv)?;
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x180, 0x1a0]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0u8; 512];
    let mut errno = 77u32.to_le_bytes();
    data[..8].copy_from_slice(&(DATA + 64).to_le_bytes());
    string(&mut data, 64, b"A=initial");
    string(&mut data, 128, b"A");
    string(&mut data, 192, b"replacement");
    native.environment_budget = 1;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )? as u32,
        u32::MAX
    );
    assert_eq!(errno, 12u32.to_le_bytes());
    assert_eq!(native.environment, DATA);
    native.environment_budget = 64;
    errno = 77u32.to_le_bytes();
    let mut descriptor: Vec<u8> = [DATA + 128, DATA + 192, 1, SET]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    let mut collision = [0u8; 1];
    assert!(
        native
            .run_worker_with_data(
                &mut image,
                0x100100,
                ARGS,
                &mut [],
                &mut [
                    DataRegion {
                        address: ARGS,
                        bytes: &mut descriptor,
                        writable: false
                    },
                    DataRegion {
                        address: DATA,
                        bytes: &mut data,
                        writable: true
                    },
                    DataRegion {
                        address: ERRNO,
                        bytes: &mut errno,
                        writable: true
                    },
                    DataRegion {
                        address: 0x7ffe00000000 + 32776 + 63,
                        bytes: &mut collision,
                        writable: true
                    }
                ]
            )
            .is_err()
    );
    assert_eq!(native.environment, DATA);
    assert_eq!(errno, 77u32.to_le_bytes());
    // Failed allocation requires errno publication; success must not touch it.
    native.environment_budget = 1;
    native.kernel = Some(Kernel::new(4096, 0x900000 + 4096 - 16)?);
    assert!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )
        .is_err()
    );
    assert_eq!(native.environment, DATA);
    native.environment_budget = 64;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, 0, 0, GET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        DATA + 66
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 0, SET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        0
    );
    let original = invoke(
        &mut native,
        &mut image,
        [DATA + 128, 0, 0, GET],
        &mut data,
        &mut errno,
        0x100100,
    )?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [original, 0, 0, 0],
            &mut data,
            &mut errno,
            0x100180
        )?,
        b'i' as u64
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [original, b'X' as u64, 0, 0],
            &mut data,
            &mut errno,
            0x1001a0
        )?,
        b'X' as u64
    );
    string(&mut data, 192, b"tiny");
    let fuel = native.consumed_instruction_fuel();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        0
    );
    let spent = native.consumed_instruction_fuel() - fuel;
    assert_eq!(spent, 7, "six guest instructions plus one service charge");
    let shorter = invoke(
        &mut native,
        &mut image,
        [DATA + 128, 0, 0, GET],
        &mut data,
        &mut errno,
        0x100100,
    )?;
    assert_eq!(original, shorter);
    string(&mut data, 192, b"a much longer new value");
    let fuel = native.consumed_instruction_fuel();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        0
    );
    assert_eq!(native.consumed_instruction_fuel() - fuel, spent);
    let larger = invoke(
        &mut native,
        &mut image,
        [DATA + 128, 0, 0, GET],
        &mut data,
        &mut errno,
        0x100100,
    )?;
    assert_ne!(original, larger);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [original, 0, 0, 0],
            &mut data,
            &mut errno,
            0x100180
        )?,
        b't' as u64
    );
    // Invalid value still faults when overwrite=0 would otherwise preserve A.
    assert!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, u64::MAX, 0, SET],
            &mut data,
            &mut errno,
            0x100100
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, u64::MAX, 0, SET],
            &mut data,
            &mut errno,
            0x100100
        )? as u32,
        u32::MAX
    );
    assert_eq!(errno, 22u32.to_le_bytes());
    errno = 77u32.to_le_bytes();
    assert!(
        invoke(
            &mut native,
            &mut image,
            [0, u64::MAX, 0, SET],
            &mut data,
            &mut errno[..3],
            0x100100
        )
        .is_err()
    );
    assert_eq!(errno, 77u32.to_le_bytes());
    string(&mut data, 128, b"\xff");
    string(&mut data, 192, b"\xfe");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        0
    );
    let raw = invoke(
        &mut native,
        &mut image,
        [DATA + 128, 0, 0, GET],
        &mut data,
        &mut errno,
        0x100100,
    )?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [raw, 0, 0, 0],
            &mut data,
            &mut errno,
            0x100180
        )?,
        254
    );
    string(&mut data, 192, b"");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [raw, 0, 0, 0],
            &mut data,
            &mut errno,
            0x100180
        )?,
        0
    );
    string(&mut data, 128, b"A");
    string(&mut data, 192, &[b'Z'; 80]);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, DATA + 192, 1, SET],
            &mut data,
            &mut errno,
            0x100100
        )? as u32,
        u32::MAX
    );
    assert_eq!(errno, 12u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, 0, 0, GET],
            &mut data,
            &mut errno,
            0x100100
        )?,
        larger
    );
    println!(
        "PASS: native imported/set/get environment, writable values, stable/replaced pointers, overwrite ordering, raw/empty bytes, errno guards and equal once-only service fuel."
    );
    Ok(())
}
