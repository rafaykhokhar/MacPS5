//! Translated owned strdup/memset calls, preserving full64-bit heap pointers.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
fn invoke(
    n: &mut NativeExecutor,
    i: &mut Image,
    entry: u64,
    target: u64,
    args: [u64; 3],
    data: &mut [u8],
    writable: bool,
) -> Result<u64, String> {
    let mut table = [0u8; 40];
    for (c, v) in table
        .chunks_exact_mut(8)
        .zip(args.into_iter().chain([target, 0]))
    {
        c.copy_from_slice(&v.to_le_bytes());
    }
    let mut errno = [0; 4];
    match n.run_with_data(
        i,
        entry,
        Arguments {
            size: 0x31000000,
            pointer: 0,
        },
        &mut [],
        &mut [
            DataRegion {
                address: 0x31000000,
                bytes: &mut table,
                writable: true,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable,
            },
            DataRegion {
                address: 0x32000000,
                bytes: &mut errno,
                writable: true,
            },
        ],
    )? {
        Execution::Returned(0) => Ok(u64::from_le_bytes(table[32..40].try_into().unwrap())),
        _ => Err("libc memory fixture failed/suspended".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    for (offset, overlap) in [(0x100, false), (0x180, true)] {
        let mut code = vec![0x53, 0x48, 0x89, 0xfb];
        for (reg, off) in [(7, 0), (6, 8), (2, 16), (0, 24)] {
            code.extend([0x48, 0x8b, 0x43 | (reg << 3), off]);
        }
        if overlap {
            code.extend([0x48, 0x8d, 0x7c, 0x24, 0xf8]);
        }
        code.extend([0xff, 0xd0, 0x48, 0x89, 0x43, 32, 0x31, 0xc0, 0x5b, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    native.kernel = Some(Kernel::new(4096, 0x32000000)?);
    for (address, service) in [
        (0x720000, Service::Strdup),
        (0x720010, Service::Memset),
        (0x720020, Service::Memcpy),
        (0x720030, Service::Free),
    ] {
        native.register_service(address, service)?;
    }
    // SAFETY: caller supplies matching locally built runtime archive.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 4096)?;
    let mut bytes = [0xa5; 64];
    bytes[..6].copy_from_slice(b"hello\0");
    let copy = invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720000,
        [DATA, 0, 0],
        &mut bytes,
        false,
    )?;
    assert!(copy > u32::MAX as u64);
    assert_eq!(native.bytes_allocated(), 16);
    bytes[..5].copy_from_slice(b"world");
    invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720020,
        [DATA + 16, copy, 6],
        &mut bytes,
        true,
    )?;
    assert_eq!(&bytes[16..22], b"hello\0");
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            0x720010,
            [copy, 0x134, 1],
            &mut bytes,
            true
        )?,
        copy
    );
    invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720020,
        [DATA + 16, copy, 6],
        &mut bytes,
        true,
    )?;
    assert_eq!(&bytes[16..22], b"4ello\0");
    assert_eq!(&bytes[..6], b"world\0");
    let before = bytes;
    for (address, count, writable) in [
        (DATA, 4, false),
        (DATA + 63, 2, true),
        (u64::MAX - 1, 4, true),
        (DATA, 16 * 1024 * 1024 + 1, true),
    ] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                0x100100,
                0x720010,
                [address, 0, count],
                &mut bytes,
                writable
            )
            .is_err()
        );
        assert_eq!(bytes, before);
    }
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100180,
            0x720010,
            [0, 0, 4],
            &mut bytes,
            true
        )
        .is_err()
    );
    for address in [0, u64::MAX] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x100100,
                0x720010,
                [address, 0, 0],
                &mut bytes,
                true
            )?,
            address
        );
    }
    for source in [0, DATA + 63, u64::MAX] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                0x100100,
                0x720000,
                [source, 0, 0],
                &mut bytes,
                true
            )
            .is_err()
        );
        assert_eq!(native.bytes_allocated(), 16);
    }
    invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720030,
        [copy, 0, 0],
        &mut bytes,
        true,
    )?;
    assert_eq!(native.bytes_allocated(), 0);
    let empty = invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720000,
        [DATA + 5, 0, 0],
        &mut bytes,
        true,
    )?;
    invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720020,
        [DATA + 16, empty, 1],
        &mut bytes,
        true,
    )?;
    assert_eq!(bytes[16], 0);
    invoke(
        &mut native,
        &mut image,
        0x100100,
        0x720030,
        [empty, 0, 0],
        &mut bytes,
        true,
    )?;
    assert_eq!(native.bytes_allocated(), 0);
    println!(
        "PASS: translated strdup independent owned full64-bit heap copies including NUL/free; memset lowbyte/zero-size and readonly/range/overflow/return-frame checks"
    );
    Ok(())
}
