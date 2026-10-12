//! Actual translated realloc; bounded owned heap and FreeBSD zero-size policy.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const REALLOC: u64 = 0x780000;
const FREE: u64 = 0x780010;
const ERRNO: u64 = 0x32000000;
fn invoke(
    n: &mut NativeExecutor,
    i: &mut Image,
    entry: u64,
    target: u64,
    pointer: u64,
    size: u64,
    errno: &mut [u8; 4],
    writable: bool,
) -> Result<u64, String> {
    let mut table = [0u8; 32];
    for (chunk, value) in table.chunks_exact_mut(8).zip([pointer, size, target, 0]) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    let outcome = n.run_with_data(
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
                address: ERRNO,
                bytes: errno,
                writable,
            },
        ],
    )?;
    assert!(matches!(outcome, Execution::Returned(0)));
    Ok(u64::from_le_bytes(table[24..32].try_into().unwrap()))
}
fn inspect(
    n: &mut NativeExecutor,
    i: &mut Image,
    f: impl FnOnce(&mut native_ps5::guest_memory::Memory<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let Execution::Dlsym {
        mut continuation, ..
    } = n.run(i, 0x100140, Arguments::default(), &mut [])?
    else {
        return Err("Expected realloc inspection suspension".into());
    };
    n.with_suspended_memory(&mut continuation, i, &mut [], &mut [], f)
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let code = [
        0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8b, 0x43, 16, 0xff,
        0xd0, 0x48, 0x89, 0x43, 24, 0x31, 0xc0, 0x5b, 0xc3,
    ];
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut suspend = vec![0x48, 0xb8];
    suspend.extend(0x780020u64.to_le_bytes());
    suspend.extend([0xff, 0xd0, 0xc3]);
    file[0x140..0x140 + suspend.len()].copy_from_slice(&suspend);
    // A guest may choose an owned heap as its stack. Realloc must not free the
    // object containing its live return address, even when ordinary ownership matches.
    let heap_stack = [
        0x48, 0x89, 0xfb, 0x48, 0x8b, 0x3b, 0x48, 0x8b, 0x73, 8, 0x48, 0x8d, 0x67, 16, 0x48, 0x8b,
        0x43, 16, 0xff, 0xd0, 0xc3,
    ];
    file[0x180..0x180 + heap_stack.len()].copy_from_slice(&heap_stack);
    let mut n = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    for (address, service) in [
        (REALLOC, Service::Realloc),
        (FREE, Service::Free),
        (0x780020, Service::Dlsym),
    ] {
        n.register_service(address, service)?;
    }
    // SAFETY: caller supplies matching locally built Rust runtime archive.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x180]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 65536)?;
    let mut errno = 77u32.to_le_bytes();
    let old = invoke(
        &mut n, &mut image, 0x100100, REALLOC, 0, 17, &mut errno, true,
    )?;
    assert!(old > u32::MAX as u64);
    assert_eq!(errno, 77u32.to_le_bytes());
    inspect(&mut n, &mut image, |m| m.write(old, b"abcdefghijklmnopq"))?;
    for size in [u64::MAX, 16 * 1024 * 1024 + 1] {
        assert_eq!(
            invoke(
                &mut n, &mut image, 0x100100, REALLOC, old, size, &mut errno, true
            )?,
            0
        );
        assert_eq!(errno, 12u32.to_le_bytes());
        assert_eq!(n.bytes_allocated(), 32);
        inspect(&mut n, &mut image, |m| {
            assert_eq!(m.read(old, 17)?, b"abcdefghijklmnopq");
            Ok(())
        })?;
    }
    for pointer in [old + 1, 1, 0x100100] {
        assert!(
            invoke(
                &mut n, &mut image, 0x100100, REALLOC, pointer, 8, &mut errno, true
            )
            .is_err()
        );
        assert_eq!(n.bytes_allocated(), 32);
    }
    assert!(
        invoke(
            &mut n, &mut image, 0x100100, REALLOC, old, 65, &mut errno, false
        )
        .is_err()
    );
    assert_eq!(n.bytes_allocated(), 32);
    // Error output cannot corrupt the pending service return frame.
    n.kernel = Some(Kernel::new(4096, 0x900fe8)?);
    assert!(
        invoke(
            &mut n,
            &mut image,
            0x100100,
            REALLOC,
            old,
            u64::MAX,
            &mut errno,
            true
        )
        .err()
        .unwrap()
        .contains("return frame")
    );
    assert_eq!(n.bytes_allocated(), 32);
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    let grown = invoke(
        &mut n, &mut image, 0x100100, REALLOC, old, 65, &mut errno, true,
    )?;
    assert_ne!(grown, old);
    assert!(grown > u32::MAX as u64);
    assert_eq!(n.bytes_allocated(), 80);
    inspect(&mut n, &mut image, |m| {
        assert!(m.read(old, 1).is_err());
        assert_eq!(m.read(grown, 17)?, b"abcdefghijklmnopq");
        Ok(())
    })?;
    let small = invoke(
        &mut n, &mut image, 0x100100, REALLOC, grown, 3, &mut errno, true,
    )?;
    assert_eq!(n.bytes_allocated(), 16);
    inspect(&mut n, &mut image, |m| {
        assert!(m.read(grown, 1).is_err());
        assert_eq!(m.read(small, 3)?, b"abc");
        Ok(())
    })?;
    let zero = invoke(
        &mut n, &mut image, 0x100100, REALLOC, small, 0, &mut errno, true,
    )?;
    assert_ne!(zero, 0);
    assert_eq!(n.bytes_allocated(), 16);
    inspect(&mut n, &mut image, |m| {
        assert!(m.read(small, 1).is_err());
        assert_eq!(m.read(zero, 1)?, b"a");
        Ok(())
    })?;
    assert!(
        invoke(
            &mut n, &mut image, 0x100180, REALLOC, zero, 65, &mut errno, true
        )
        .err()
        .unwrap()
        .contains("return frame")
    );
    assert_eq!(n.bytes_allocated(), 16);
    assert_eq!(
        invoke(
            &mut n, &mut image, 0x100100, FREE, zero, 0, &mut errno, true
        )?,
        0
    );
    assert_eq!(n.bytes_allocated(), 0);
    let nullzero = invoke(
        &mut n, &mut image, 0x100100, REALLOC, 0, 0, &mut errno, true,
    )?;
    assert_ne!(nullzero, 0);
    invoke(
        &mut n, &mut image, 0x100100, FREE, nullzero, 0, &mut errno, true,
    )?;
    assert_eq!(n.bytes_allocated(), 0);
    println!(
        "PASS: translated realloc NULL/grow/shrink/zero/full64-bit pointers, preserved payload/ownership on ENOMEM and invalid inputs, readonly/return-frame errno guards, retained heap return stack, and final free. Zero follows bounded FreeBSD allocation policy."
    );
    Ok(())
}
