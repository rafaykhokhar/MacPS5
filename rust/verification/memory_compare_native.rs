//! Translated memmove/memcmp, including overlapping guest ranges.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x31000000;
const MOVE: u64 = 0x7d0000;
const COMPARE: u64 = MOVE + 16;
fn invoke(
    n: &mut NativeExecutor,
    i: &mut Image,
    b: &mut [u8],
    service: u64,
    left: u64,
    right: u64,
    count: u64,
) -> Result<i32, String> {
    let mut args = [0u8; 32];
    for (chunk, value) in args.chunks_exact_mut(8).zip([left, right, count, service]) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    match n.run_with_data(
        i,
        0x100100,
        Arguments {
            size: 0x30000000,
            pointer: 0,
        },
        &mut [],
        &mut [
            DataRegion {
                address: 0x30000000,
                bytes: &mut args,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: b,
                writable: true,
            },
        ],
    )? {
        Execution::Returned(v) => Ok(v),
        _ => Err("Unexpected memory service suspension".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Destination points at the future CALL return word; must reject before copy.
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x48, 0xbe];
    alias.extend(DATA.to_le_bytes());
    alias.extend([0xba, 8, 0, 0, 0, 0x48, 0xb8]);
    alias.extend(MOVE.to_le_bytes());
    alias.extend([0xff, 0xd0, 0xc3]);
    file[0x140..0x140 + alias.len()].copy_from_slice(&alias);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    n.register_service(MOVE, Service::Memmove)?;
    n.register_service(COMPARE, Service::Memcmp)?;
    // SAFETY: archive is matching local runtime.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut i = Image::load(&file, 0x100000, 65536)?;
    let mut b = *b"0123456789abcdef";
    assert_eq!(
        invoke(&mut n, &mut i, &mut b, MOVE, DATA + 2, DATA, 8)?,
        (DATA + 2) as i32
    );
    assert_eq!(&b, b"0101234567abcdef");
    invoke(&mut n, &mut i, &mut b, MOVE, DATA, DATA + 2, 8)?;
    assert_eq!(&b[..8], b"01234567");
    invoke(&mut n, &mut i, &mut b, MOVE, DATA, DATA, 16)?;
    let before = b;
    for (dst, src, count) in [
        (0x100100, DATA, 1),
        (DATA, DATA + 15, 2),
        (DATA + 15, DATA, 2),
        (DATA, DATA, 1u64 << 32),
        (u64::MAX, DATA, 2),
    ] {
        assert!(invoke(&mut n, &mut i, &mut b, MOVE, dst, src, count).is_err());
        assert_eq!(b, before);
    }
    assert_eq!(invoke(&mut n, &mut i, &mut b, MOVE, u64::MAX, 0, 0)?, -1);
    assert_eq!(invoke(&mut n, &mut i, &mut b, COMPARE, 0, u64::MAX, 0)?, 0);
    b[..8].copy_from_slice(&[0x80, 0, 1, 0, 0x7f, 0, 2, 0]);
    assert!(invoke(&mut n, &mut i, &mut b, COMPARE, DATA, DATA + 4, 4)? > 0);
    assert!(invoke(&mut n, &mut i, &mut b, COMPARE, DATA + 1, DATA + 5, 3)? < 0);
    assert_eq!(invoke(&mut n, &mut i, &mut b, COMPARE, DATA, DATA, 16)?, 0);
    for count in [17, 1u64 << 32] {
        assert!(invoke(&mut n, &mut i, &mut b, COMPARE, DATA, DATA, count).is_err());
    }
    let before = b;
    assert!(
        n.run_with_data(
            &mut i,
            0x100140,
            Arguments::default(),
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut b,
                writable: false
            }]
        )
        .is_err()
    );
    assert_eq!(b, before);
    println!(
        "PASS translated memmove overlap both directions/selfalias, zero-length, full64-bit limits, readonly/range/return-frame guards; memcmp unsigned bytes and embeddedNUL ordering"
    );
    Ok(())
}
