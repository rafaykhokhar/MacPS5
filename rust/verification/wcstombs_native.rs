//! C-locale wchar16 conversion and bounded actual PS5 keyboard conversion branch.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_3d000001;
const ARGS: u64 = 0x3e000000;
const ERRNO: u64 = 0x3f000000;
const SERVICE: u64 = 0x7f0000;
fn patch(image: &mut Image, offset: u64, bytes: &[u8]) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| {
            address >= s.address && address + bytes.len() as u64 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing wcstombs fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + bytes.len()].copy_from_slice(bytes);
    Ok(())
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    args: [u64; 3],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut regions = vec![DataRegion {
        address: ARGS,
        bytes: &mut descriptor,
        writable: false,
    }];
    if !data.is_empty() {
        regions.push(DataRegion {
            address: DATA,
            bytes: data,
            writable,
        });
    }
    if !errno.is_empty() {
        regions.push(DataRegion {
            address: ERRNO,
            bytes: errno,
            writable: true,
        });
    }
    match n.run_worker_with_data(a, 0x100100, ARGS, &mut [], &mut regions)? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("wcstombs did not return".into()),
    }
}
fn wide(data: &mut [u8], values: &[u16]) {
    for (chunk, value) in data.chunks_exact_mut(2).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let mut c = vec![
        0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48, 0xb8,
    ];
    c.extend(SERVICE.to_le_bytes());
    c.extend([0xff, 0xd0, 0xc3]);
    f[0x100..0x100 + c.len()].copy_from_slice(&c);
    let mut branch = vec![
        0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec, 0x20, 8, 0, 0, 0x48, 0xb8,
    ];
    branch.extend((BIAS + 0xc412d).to_le_bytes());
    branch.extend([0xff, 0xe0]);
    f[0x180..0x180 + branch.len()].copy_from_slice(&branch);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Wcstombs)?;
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    patch(&mut real, 0x26fa20, &SERVICE.to_le_bytes())?;
    patch(&mut real, 0x592040, &[b'A', 0, 0xff, 0, 0, 0])?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xc412d]),
            10000,
            archive,
            &directory.join("branch"),
        )?;
    }
    let mut data = [0xa5; 160];
    let mut errno = 77u32.to_le_bytes();
    wide(&mut data, &[65, 0x80, 0xff, 0]);
    for (count, expected, output) in [
        (0u64, 0u64, &b""[..]),
        (2, 2, &b"A\x80"[..]),
        (3, 3, &b"A\x80\xff"[..]),
        (u64::MAX, 3, &b"A\x80\xff\0"[..]),
    ] {
        data[64..].fill(0xa5);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [DATA + 64, DATA, count],
                &mut data,
                true,
                &mut errno
            )?,
            expected
        );
        assert_eq!(&data[64..64 + output.len()], output);
        assert_eq!(data[64 + output.len()], 0xa5);
        assert_eq!(errno, 77u32.to_le_bytes());
    }
    assert_eq!(
        invoke(&mut n, &mut a, [0, DATA, 0], &mut data, true, &mut errno)?,
        3
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [u64::MAX, u64::MAX, 0],
            &mut [],
            false,
            &mut []
        )?,
        0
    );
    // Raw code units, not UTF8: non-byte values and surrogate halves fail.
    for invalid in [0x100, 0xd800, 0xdc00, 0xffff] {
        wide(&mut data, &[65, invalid, 0]);
        data[64..80].fill(0xa5);
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [DATA + 64, DATA, 16],
                &mut data,
                true,
                &mut errno
            )?,
            u64::MAX
        );
        assert_eq!(&data[64..67], &[65, 0xa5, 0xa5]);
        assert_eq!(errno, 86u32.to_le_bytes());
    }
    wide(&mut data, &[65, 0x100, 0]);
    let before = data;
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, DATA, 16],
            &mut data,
            true,
            &mut short
        )
        .is_err()
    );
    assert_eq!(data, before);
    assert_eq!(short, [0xa5; 3]);
    wide(&mut data, &[65, 66, 0]);
    for (dst, writable) in [(DATA + 159, true), (DATA + 64, false), (u64::MAX, true)] {
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut a,
                [dst, DATA, 3],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    // Failed source guard and full64 count cannot wrap into a small valid request.
    assert!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, DATA + 159, u64::MAX],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    // Error prefix is committed before an aliased errno write.
    wide(&mut data, &[65, 0x100, 0]);
    n.kernel = Some(Kernel::new(4096, DATA + 64)?);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, DATA, 8],
            &mut data,
            true,
            &mut errno
        )?,
        u64::MAX
    );
    assert_eq!(&data[64..68], &86u32.to_le_bytes());
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    let before = data;
    assert!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, DATA, 8],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert_eq!(data, before);
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    // Success never requires errno storage.
    wide(&mut data, &[0xff, 0]);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 64, DATA, 2],
            &mut data,
            true,
            &mut []
        )?,
        1
    );
    n.set_worker_quantum(Some(1))?;
    let mut outcome = n.run_worker_with_data(
        &mut a,
        0x100180,
        0,
        &mut [&mut real],
        &mut [DataRegion {
            address: ERRNO,
            bytes: &mut errno,
            writable: true,
        }],
    )?;
    let mut observed = false;
    for _ in 0..40 {
        let Execution::QuantumYield(mut wait) = outcome else {
            return Err("Expected bounded keyboard branch quantum".into());
        };
        let stack = wait.stack_region()?;
        let offset = stack.bytes.len() - 16 - 0x820;
        if &stack.bytes[offset..offset + 3] == b"A\xff\0" {
            observed = true;
            break;
        }
        outcome = n.resume_quantum(
            &mut a,
            &mut [&mut real],
            &mut wait,
            &mut [DataRegion {
                address: ERRNO,
                bytes: &mut errno,
                writable: true,
            }],
        )?;
    }
    assert!(
        observed,
        "Actual wcstombs branch never produced stack bytes"
    );
    println!(
        "Native C-locale wchar16 byte conversion and actual bounded keyboard conversion branch passed; no IME input delivery exercised"
    );
    Ok(())
}
