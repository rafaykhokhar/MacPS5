use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match native.run_worker_with_data(
        image,
        0x100100,
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
                writable,
            },
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Narrow string wrapper did not return".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut file = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    for (i, service) in [
        Service::Strlcpy,
        Service::Strlcat,
        Service::Strchr,
        Service::Strrchr,
        Service::Strstr,
    ]
    .into_iter()
    .enumerate()
    {
        native.register_service(0x780000 + i as u64 * 16, service)?;
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
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0x55u8; 128];
    data[..7].copy_from_slice(b"ababa\xff\0");
    data[16..20].copy_from_slice(b"aba\0");
    data[24] = 0;
    for (args, want) in [
        ([DATA, 0x1ff, 0, 0x780020], DATA + 5),
        ([DATA, b'a' as u64, 0, 0x780030], DATA + 4),
        ([DATA, 0, 0, 0x780020], DATA + 6),
        ([DATA, 0x100, 0, 0x780030], DATA + 6),
        ([DATA, b'z' as u64, 0, 0x780020], 0),
        ([DATA, DATA + 16, 0, 0x780040], DATA),
        ([u64::MAX, DATA + 24, 0, 0x780040], u64::MAX),
    ] {
        assert_eq!(
            invoke(&mut native, &mut image, args, &mut data, false)?,
            want
        );
    }
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [u64::MAX, DATA, 0, 0x780000],
            &mut data,
            false
        )?,
        6
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 32, DATA, 4, 0x780000],
            &mut data,
            true
        )?,
        6
    );
    assert_eq!(&data[32..37], b"aba\0U");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 32, DATA + 16, 5, 0x780010],
            &mut data,
            true
        )?,
        6
    );
    assert_eq!(&data[32..38], b"abaa\0U");
    let before = data;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 32, DATA, 2, 0x780010],
            &mut data,
            false
        )?,
        8
    );
    assert_eq!(data, before);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 48, DATA, u64::MAX, 0x780000],
            &mut data,
            true
        )?,
        6
    );
    assert_eq!(&data[48..55], &before[..7]);
    let before = data;
    assert!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 125, DATA, 9, 0x780000],
            &mut data,
            true
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 32, DATA, 3, 0x780000],
            &mut data,
            false
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert!(
        invoke(
            &mut native,
            &mut image,
            [0x900000 + 4096 - 16, DATA, 3, 0x780000],
            &mut data,
            true
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 32, DATA + 127, 1, 0x780000],
            &mut data,
            true
        )
        .is_err()
    );
    assert_eq!(data, before);
    println!(
        "PASS: narrow strlcpy/strlcat truncation, no-termination/zero-size, full64 size and pointers, unsigned character/NUL searches, empty needle, readonly/short/source/frame atomic guards."
    );
    Ok(())
}
