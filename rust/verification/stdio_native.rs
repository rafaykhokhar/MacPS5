//! Translated FILE pointer-object, fprintf register/stack arguments and flush.
use native_ps5::{
    guest_memory::DataRegion,
    guest_stdio::{FILE_ADDR, STDERR_PTR_ADDR},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
fn invoke(
    n: &mut NativeExecutor,
    i: &mut Image,
    indirect: bool,
    target: u64,
    args: [u64; 8],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut table = [0u8; 80];
    for (chunk, value) in table
        .chunks_exact_mut(8)
        .zip(args.into_iter().chain([target, 0]))
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    match n.run_with_data(
        i,
        if indirect { 0x100100 } else { 0x100180 },
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
                writable: true,
            },
        ],
    )? {
        Execution::Returned(0) => Ok(u64::from_le_bytes(table[72..80].try_into().unwrap())),
        _ => Err("Unexpected stdio fixture execution".into()),
    }
}
fn inspect(
    n: &mut NativeExecutor,
    i: &mut Image,
    f: impl FnOnce(&mut native_ps5::guest_memory::Memory<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let Execution::Dlsym {
        mut continuation, ..
    } = n.run(i, 0x1001e0, Arguments::default(), &mut [])?
    else {
        return Err("Expected stdio suspension".into());
    };
    n.with_suspended_memory(&mut continuation, i, &mut [], &mut [], f)
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    for (offset, indirect) in [(0x100, true), (0x180, false)] {
        let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0x48, 0x83, 0xec, 0x10];
        // Two variadic stack arguments at caller RSP, then RSP+8.
        code.extend([0x48, 0x8b, 0x43, 48, 0x48, 0x89, 0x04, 0x24]);
        code.extend([0x48, 0x8b, 0x43, 56, 0x48, 0x89, 0x44, 0x24, 8]);
        for (reg, off) in [(7u8, 0), (6, 8), (2, 16), (1, 24), (8, 32), (9, 40)] {
            code.extend([
                if reg >= 8 { 0x4c } else { 0x48 },
                0x8b,
                0x43 | ((reg & 7) << 3),
                off,
            ]);
        }
        if indirect {
            code.extend([0x48, 0x8b, 0x3f]);
        }
        code.extend([
            0x48, 0x8b, 0x43, 64, 0xff, 0xd0, 0x48, 0x89, 0x43, 72, 0x48, 0x83, 0xc4, 0x10, 0x31,
            0xc0, 0x5b, 0xc3,
        ]);
        assert!(code.len() < 96);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    let mut suspend = vec![0x48, 0xb8];
    suspend.extend(0x760020u64.to_le_bytes());
    suspend.extend([0xff, 0xd0, 0xc3]);
    file[0x1e0..0x1e0 + suspend.len()].copy_from_slice(&suspend);
    let mut n = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    for (address, service) in [
        (0x760000, Service::Fprintf),
        (0x760010, Service::Fflush),
        (0x760020, Service::Dlsym),
    ] {
        n.register_service(address, service)?;
    }
    // SAFETY: caller supplies the matching locally built runtime archive.
    unsafe {
        n.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x180, 0x1e0]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut i = Image::load(&file, 0x100000, 65536)?;
    let mut data = [0u8; 256];
    for (offset, text) in [
        (0, b"\n\n%s\n%s\n\n\0".as_slice()),
        (32, b"warning\0"),
        (48, b"detail\0"),
        (64, b"%s %s %s %s %s %d\0"),
        (96, b"%f\0"),
        (112, b"%s\0"),
        (128, b"%128s\0"),
    ] {
        data[offset..offset + text.len()].copy_from_slice(text);
    }
    let warning = [STDERR_PTR_ADDR, DATA, DATA + 32, DATA + 48, 0, 0, 0, 0];
    assert!(invoke(&mut n, &mut i, true, 0x760000, warning, &mut data).is_err());
    n.stdio.configure_capture(128)?;
    inspect(&mut n, &mut i, |m| {
        assert_eq!(m.load(STDERR_PTR_ADDR, 8)?, FILE_ADDR);
        assert_eq!(m.load(FILE_ADDR + 16, 2)?, 10);
        assert_eq!(m.load(FILE_ADDR + 18, 2)?, 2);
        assert!(m.write(STDERR_PTR_ADDR, &0u64.to_le_bytes()).is_err());
        Ok(())
    })?;
    let expected = b"\n\nwarning\ndetail\n\n";
    assert_eq!(
        invoke(&mut n, &mut i, true, 0x760000, warning, &mut data)?,
        expected.len() as u64
    );
    assert_eq!(n.stdio.bytes(), expected);
    let args = [
        FILE_ADDR,
        DATA + 64,
        DATA + 32,
        DATA + 32,
        DATA + 32,
        DATA + 32,
        DATA + 32,
        42,
    ];
    assert_eq!(
        invoke(&mut n, &mut i, false, 0x760000, args, &mut data)?,
        42
    );
    assert_eq!(
        n.stdio.bytes(),
        b"\n\nwarning\ndetail\n\nwarning warning warning warning warning 42"
    );
    let saved = n.stdio.bytes().to_vec();
    inspect(&mut n, &mut i, |m| {
        assert_eq!(m.load(FILE_ADDR + 172, 4)?, u32::MAX as u64);
        Ok(())
    })?;
    for stream in [FILE_ADDR, 0] {
        assert_eq!(
            invoke(
                &mut n,
                &mut i,
                false,
                0x760010,
                [stream, 0, 0, 0, 0, 0, 0, 0],
                &mut data
            )?,
            0
        );
        assert_eq!(n.stdio.bytes(), saved);
    }
    for args in [
        [1, DATA, DATA + 32, DATA + 48, 0, 0, 0, 0],
        [FILE_ADDR, 1, 0, 0, 0, 0, 0, 0],
        [FILE_ADDR, DATA + 96, 0, 0, 0, 0, 0, 0],
        [FILE_ADDR, DATA + 112, 1, 0, 0, 0, 0, 0],
        [FILE_ADDR, DATA + 128, DATA + 32, 0, 0, 0, 0, 0],
    ] {
        assert!(invoke(&mut n, &mut i, false, 0x760000, args, &mut data).is_err());
        assert_eq!(n.stdio.bytes(), saved);
    }
    inspect(&mut n, &mut i, |m| m.store(FILE_ADDR + 16, 0, 2))?;
    assert!(invoke(&mut n, &mut i, true, 0x760000, warning, &mut data).is_err());
    assert!(invoke(&mut n, &mut i, false, 0x760010, [0; 8], &mut data).is_err());
    assert_eq!(n.stdio.bytes(), saved);
    inspect(&mut n, &mut i, |m| m.store(FILE_ADDR + 16, 10, 2))?;
    inspect(&mut n, &mut i, |m| m.store(FILE_ADDR + 172, 1, 4))?;
    assert!(invoke(&mut n, &mut i, true, 0x760000, warning, &mut data).is_err());
    assert_eq!(n.stdio.bytes(), saved);
    inspect(&mut n, &mut i, |m| {
        m.store(FILE_ADDR + 172, u32::MAX as u64, 4)
    })?;
    assert_eq!(n.stdio.take_bytes(), saved);
    data[32] = b'W';
    assert_eq!(
        invoke(&mut n, &mut i, true, 0x760000, warning, &mut data)?,
        expected.len() as u64
    );
    assert_eq!(n.stdio.bytes(), b"\n\nWarning\ndetail\n\n");
    // A valid tail return word immediately before an unmapped overflow argument.
    // This exercises the executor's lazy stack argument guards, not the formatter
    // with a fabricated failing closure.
    let mut bad_stack_file = memory_fixture();
    let mut code = vec![0x48, 0xbf];
    code.extend(FILE_ADDR.to_le_bytes());
    code.extend([0x48, 0xbe]);
    code.extend(DATA.to_le_bytes());
    code.extend([0x31, 0xd2, 0x31, 0xc9, 0x45, 0x31, 0xc0, 0x45, 0x31, 0xc9]);
    code.extend([0x48, 0xbc]);
    code.extend((DATA + 248).to_le_bytes());
    code.extend([0x48, 0xb8]);
    code.extend(0x760000u64.to_le_bytes());
    code.extend([0xff, 0xe0]);
    assert!(code.len() < 64);
    bad_stack_file[0x100..0x100 + code.len()].copy_from_slice(&code);
    bad_stack_file[0x140] = 0xc3;
    let mut bad_stack = NativeExecutor::new(0x900000, 4096, 2000, 2000)?;
    bad_stack.register_service(0x760000, Service::Fprintf)?;
    bad_stack.stdio.configure_capture(128)?;
    // SAFETY: same matching archive contract as above.
    unsafe {
        bad_stack.compile_image(
            &bad_stack_file,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            2000,
            archive,
            directory,
        )?;
    }
    let mut bad_image = Image::load(&bad_stack_file, 0x100000, 65536)?;
    let format = b"%d %d %d %d %d\0";
    data.fill(0);
    data[..format.len()].copy_from_slice(format);
    data[248..256].copy_from_slice(&0x100140u64.to_le_bytes());
    let error = bad_stack
        .run_with_data(
            &mut bad_image,
            0x100100,
            Arguments::default(),
            &mut [],
            &mut [DataRegion {
                address: DATA,
                bytes: &mut data,
                writable: true,
            }],
        )
        .err()
        .ok_or("Invalid variadic stack unexpectedly succeeded")?;
    assert!(
        error.contains("Printf argument fault: Read(")
            && error.contains("Guest access outside mapping"),
        "{error}"
    );
    assert!(bad_stack.stdio.bytes().is_empty());
    println!(
        "PASS: translated stderr FILE pointer object, observed fprintf warning format, register/stack varargs, exact captured bytes/counts, flush/NULL, readonly object and atomic invalid stream/format/string/FILE/capacity/overflow-stack failures; unbuffered owned stderr only"
    );
    Ok(())
}
