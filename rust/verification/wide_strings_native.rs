//! Actual pinned ELF SDL wide-string tail wrappers; SCE unsigned16 code units.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
fn invoke(
    native: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match native.run_worker_with_data(
        adapter,
        0x100100,
        ARGS,
        &mut [real],
        &mut [
            DataRegion {
                address: ARGS,
                bytes: &mut descriptor,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable: false,
            },
        ],
    )? {
        Execution::WorkerReturned(value) => Ok(value),
        _ => Err("Real SDL wide wrapper did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut adapter_file = memory_fixture();
    // Adapter only loads SysV arguments and calls the actual guest wrapper.
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    adapter_file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&adapter_file, 0x100000, 65536)?;
    for (index, (got, service)) in [
        (0x26f7f0, Service::Wcslen),
        (0x26f7f8, Service::Wcsstr),
        (0x26f800, Service::Wcscmp),
        (0x26f808, Service::Wcsncmp),
    ]
    .into_iter()
    .enumerate()
    {
        let target = 0x780000 + index as u64 * 16;
        native.register_service(target, service)?;
        let address = BIAS + got;
        let segment = real
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing real SDL wide GOT")?;
        let offset = (address - segment.address) as usize;
        segment.bytes[offset..offset + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        native.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x19b50, 0x19c80, 0x19c90, 0x19ca0]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        native.compile_image(
            &adapter_file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut data = [0xa5; 80];
    for (offset, units) in [
        (1, &[0xd800u16, 0xffff, 0xdc00, 0][..]),
        (17, &[0xffff, 0xdc00, 0]),
        (33, &[0xd800, 0xfffe, 0xdc00, 0]),
        (49, &[0x1234, 0]),
        (63, &[0]),
    ] {
        for (index, unit) in units.iter().enumerate() {
            data[offset + index * 2..offset + index * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    }
    let before = data;
    let cases = [
        ([DATA + 1, 0, 0, BIAS + 0x19b50], 3),
        ([DATA + 1, DATA + 17, 0, BIAS + 0x19c80], DATA + 3),
        ([DATA + 1, DATA + 49, 0, BIAS + 0x19c80], 0),
        ([u64::MAX, DATA + 63, 0, BIAS + 0x19c80], u64::MAX),
        ([DATA + 1, DATA + 33, 0, BIAS + 0x19c90], 1),
        ([DATA + 33, DATA + 1, 0, BIAS + 0x19c90], u32::MAX as u64),
        ([DATA + 1, DATA + 33, 1, BIAS + 0x19ca0], 0),
        ([DATA + 1, DATA + 33, 1u64 << 32, BIAS + 0x19ca0], 1),
        ([u64::MAX, u64::MAX, 0, BIAS + 0x19ca0], 0),
    ];
    for (args, expected) in cases {
        assert_eq!(
            invoke(&mut native, &mut adapter, &mut real, args, &mut data)?,
            expected
        );
        assert_eq!(data, before);
    }
    let mut partial = [0x41];
    for target in [0x19b50, 0x19c90, 0x19ca0] {
        assert!(
            invoke(
                &mut native,
                &mut adapter,
                &mut real,
                [DATA, DATA, 1, BIAS + target],
                &mut partial
            )
            .is_err()
        );
        assert_eq!(partial, [0x41]);
    }
    println!(
        "PASS: actual four SDL wide wrappers, unsigned16 surrogate/rawffff/unaligned reads, full64 match and emptyneedle pointer, signed comparison, full64 count, zero count, partialword guards and unchanged sources."
    );
    Ok(())
}
