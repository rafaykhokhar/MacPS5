//! Real guest setenv/getenv, including stable values and writable owned storage.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
const SET: u64 = 0x780000;
const GET: u64 = 0x780010;
const PUT: u64 = 0x780020;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    errno: (&mut [u8], bool),
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
                bytes: errno.0,
                writable: errno.1,
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
pub fn verify(elf: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut inventory = Image::load(elf, 0, 64 * 1024 * 1024)?;
    inventory.relocate(|_| None)?;
    let import = inventory
        .unresolved
        .iter()
        .find(|i| i.name == "putenv" && i.address == 0x26fbb0)
        .ok_or("Missing named putenv import")?;
    println!("Verified named putenv import at 0x{:x}", import.address);
    let mut file = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48,
        0x83, 0xec, 8, 0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 65536, 10000, 10000)?;
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    native.environment = DATA;
    for (address, service) in [
        (SET, Service::Setenv),
        (GET, Service::Getenv),
        (PUT, Service::Putenv),
    ] {
        native.register_service(address, service)?;
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
    let mut data = [0u8; 512];
    let mut errno = 77u32.to_le_bytes();
    string(&mut data, 64, b"A=first");
    string(&mut data, 256, b"A");
    string(&mut data, 288, b"owned");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 64, 0, 0, PUT],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 66
    );
    string(&mut data, 66, b"edited");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 66
    );
    data[64] = b'C';
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    string(&mut data, 320, b"C");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 320, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 66
    );
    data[64] = b'A';
    string(&mut data, 128, b"B=second");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 128, 0, 0, PUT],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    data[128] = b'A';
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 130
    );
    string(&mut data, 192, b"A=third");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 192, 0, 0, PUT],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 194
    );
    let before = data;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, DATA + 288, 0, SET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    assert_eq!(data, before);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 194
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, DATA + 288, 1, SET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    assert_eq!(data, before);
    let owned = invoke(
        &mut native,
        &mut image,
        [DATA + 256, 0, 0, GET],
        &mut data,
        (&mut errno, false),
        0x100100,
    )?;
    assert_ne!(owned, DATA + 194);
    assert_ne!(owned, 0);
    string(&mut data, 194, b"changed");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 256, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        owned
    );
    // A successful mutation must not validate or overwrite errno even when
    // its address is inside the live worker return frame.
    native.kernel = Some(Kernel::new(4096, 0x900000 + 65536 - 16)?);
    string(&mut data, 448, b"R=frame-safe");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 448, 0, 0, PUT],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    native.kernel = Some(Kernel::new(4096, ERRNO)?);
    // Name-only reads: successful putenv/getenv need not access value bytes.
    data[510..].copy_from_slice(b"Q=");
    string(&mut data, 352, b"Q");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 510, 0, 0, PUT],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [DATA + 352, 0, 0, GET],
            &mut data,
            (&mut errno, false),
            0x100100
        )?,
        DATA + 512
    );
    for text in [b"bad".as_slice(), b"=bad", b""] {
        string(&mut data, 384, text);
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                [DATA + 384, 0, 0, PUT],
                &mut data,
                (&mut errno, true),
                0x100100
            )?,
            u32::MAX as u64
        );
        assert_eq!(errno, 22u32.to_le_bytes());
    }
    errno = 77u32.to_le_bytes();
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            [0, 0, 0, PUT],
            &mut data,
            (&mut errno, true),
            0x100100
        )?,
        u32::MAX as u64
    );
    assert_eq!(errno, 22u32.to_le_bytes());
    string(&mut data, 384, b"bad");
    for writable in [false, true] {
        errno = 77u32.to_le_bytes();
        let len = if writable { 3 } else { 4 };
        let before = data;
        assert!(
            invoke(
                &mut native,
                &mut image,
                [DATA + 384, 0, 0, PUT],
                &mut data,
                (&mut errno[..len], writable),
                0x100100
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    println!(
        "PASS: native putenv exact borrowed pointers, mutable value/name and reverse collision lookup, pointer replacement, setenv overwrite0/1 ownership, name-only access to mapped-end assignment, malformed EINVAL and errno guards"
    );
    Ok(())
}
