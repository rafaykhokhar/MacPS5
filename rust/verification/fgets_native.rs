//! Explicit registered stdin/file input through bounded real guest fgets callsites.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    stdio_file::{Operation, STDIN_FILE_ADDR},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_44000001;
const ARGS: u64 = 0x45000000;
const ERRNO: u64 = 0x46000000;
const SERVICE: u64 = 0x810000;
fn patch(image: &mut Image, offset: u64, value: u64) -> Result<(), String> {
    let address = BIAS + offset;
    let s = image
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing fgets fixture mapping")?;
    let at = (address - s.address) as usize;
    s.bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}
fn fixed(c: &mut Vec<u8>, opcode: u8, value: u64) {
    c.extend([0x48, opcode]);
    c.extend(value.to_le_bytes());
}
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    args: [u64; 4],
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
        _ => Err("fgets call did not return".into()),
    }
}
fn callsite(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    entry: u64,
    data: &mut [u8],
    errno: &mut [u8],
    stack_output: bool,
    expected: &[u8],
) -> Result<(), String> {
    n.set_worker_quantum(Some(1))?;
    let mut result = n.run_worker_with_data(
        a,
        entry,
        0,
        &mut [&mut *real],
        &mut [
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
    )?;
    let mut observed = false;
    for _ in 0..40 {
        let Execution::QuantumYield(mut wait) = result else {
            return Err("Expected bounded fgets callsite quantum".into());
        };
        let matched = if stack_output {
            let stack = wait.stack_region()?;
            let at = stack.bytes.len() - 16 - 0x90;
            &stack.bytes[at..at + expected.len()] == expected
        } else {
            &data[128..128 + expected.len()] == expected
        };
        if matched {
            observed = true;
            break;
        }
        result = n.resume_quantum(
            a,
            &mut [&mut *real],
            &mut wait,
            &mut [
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
        )?;
    }
    assert!(
        observed,
        "Actual fgets callsite did not produce expected bytes"
    );
    n.set_worker_quantum(None)?;
    Ok(())
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    f[96..104].copy_from_slice(&0x300u64.to_le_bytes());
    f[104..112].copy_from_slice(&0x300u64.to_le_bytes());
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut stdin = vec![0x55, 0x48, 0x89, 0xe5, 0x48, 0x81, 0xec, 0x90, 0, 0, 0];
    fixed(&mut stdin, 0xb8, BIAS + 0xee97c);
    stdin.extend([0xff, 0xe0]);
    f[0x200..0x200 + stdin.len()].copy_from_slice(&stdin);
    let mut normal = vec![0x49, 0xbd];
    normal.extend((DATA + 32).to_le_bytes());
    fixed(&mut normal, 0xb8, BIAS + 0x1a4c88);
    normal.extend([0xff, 0xe0]);
    f[0x240..0x240 + normal.len()].copy_from_slice(&normal);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.kernel
        .as_mut()
        .unwrap()
        .files
        .register(b"/app0/stdin", b"yes\nnext".to_vec())?;
    n.kernel
        .as_mut()
        .unwrap()
        .files
        .register(b"/app0/line", b"ab\ncdef\nlast".to_vec())?;
    n.configure_stdin(b"/app0/stdin")?;
    n.register_service(SERVICE, Service::Fgets)?;
    n.register_service(SERVICE + 144, Service::Memcpy)?;
    for (index, op) in [
        Operation::Open,
        Operation::Seek,
        Operation::Tell,
        Operation::Close,
        Operation::Eof,
        Operation::Error,
        Operation::Clearerr,
        Operation::Fileno,
    ]
    .into_iter()
    .enumerate()
    {
        n.register_service(SERVICE + 16 * (index as u64 + 1), Service::File(op))?;
    }
    let mut a = Image::load(&f, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    patch(&mut real, 0x26fa38, n.stdin_pointer_address()?)?;
    patch(&mut real, 0x26fa40, SERVICE)?;
    patch(&mut real, 0x63e8e8, DATA + 128)?;
    unsafe {
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x200, 0x240]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0xee97c, 0x1a4c88]),
            10000,
            archive,
            &directory.join("callsite"),
        )?;
    }
    let mut data = [0xa5; 512];
    let mut errno = 77u32.to_le_bytes();
    data[..11].copy_from_slice(b"/app0/line\0");
    data[16..19].copy_from_slice(b"rb\0");
    let stream = invoke(
        &mut n,
        &mut a,
        [DATA, DATA + 16, 0, SERVICE + 16],
        &mut data,
        true,
        &mut errno,
    )?;
    assert!(stream > u32::MAX as u64);
    data[32..40].copy_from_slice(&stream.to_le_bytes());
    // The exported data symbol is a read-only pointer cell, not a FILE object.
    let stdin_cell = n.stdin_pointer_address()?;
    assert!(
        invoke(
            &mut n,
            &mut a,
            [stdin_cell, DATA, 8, SERVICE + 144],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    invoke(
        &mut n,
        &mut a,
        [DATA + 400, stream + 172, 4, SERVICE + 144],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(&data[400..404], &0i32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 1, stream, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        DATA + 128
    );
    invoke(
        &mut n,
        &mut a,
        [DATA + 400, stream + 172, 4, SERVICE + 144],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(&data[400..404], &(-1i32).to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [stream, 0, 0, SERVICE + 48],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );

    callsite(
        &mut n, &mut a, &mut real, 0x100200, &mut data, &mut errno, true, b"yes\n\0",
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [STDIN_FILE_ADDR, 0, 0, SERVICE + 48],
            &mut data,
            true,
            &mut errno
        )?,
        4
    );
    callsite(
        &mut n, &mut a, &mut real, 0x100240, &mut data, &mut errno, false, b"ab\n\0",
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [stream, 0, 0, SERVICE + 32],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    for (nbytes, text, eof) in [
        (3u64, &b"ab\0"[..], 0),
        (8, &b"\n\0"[..], 0),
        (8, &b"cdef\n\0"[..], 0),
        (8, &b"last\0"[..], 1),
    ] {
        data[128..160].fill(0xa5);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [DATA + 128, nbytes, stream, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            DATA + 128
        );
        assert_eq!(&data[128..128 + text.len()], text);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [stream, 0, 0, SERVICE + 80],
                &mut data,
                true,
                &mut errno
            )?,
            eof
        );
    }
    data[128..160].fill(0xa5);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 32, stream, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(&data[128..160], &[0xa5; 32]);
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 0x1234567800000001, stream, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        DATA + 128
    );
    assert_eq!(data[128], 0);
    for count in [0, u64::MAX, 0x80000000] {
        errno = 77u32.to_le_bytes();
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [u64::MAX, count, stream, SERVICE],
                &mut data,
                true,
                &mut errno
            )?,
            0
        );
        assert_eq!(errno, 22u32.to_le_bytes());
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [stream, 0, 0, SERVICE + 96],
                &mut data,
                true,
                &mut errno
            )?,
            1
        );
    }
    invoke(
        &mut n,
        &mut a,
        [stream, 0, 0, SERVICE + 112],
        &mut data,
        true,
        &mut errno,
    )?;
    invoke(
        &mut n,
        &mut a,
        [stream, 0, 0, SERVICE + 32],
        &mut data,
        true,
        &mut errno,
    )?;
    for (dst, writable) in [
        (DATA + 511, true),
        (DATA + 128, false),
        (stream, true),
        (STDIN_FILE_ADDR, true),
        (u64::MAX, true),
    ] {
        let before = data;
        let position = invoke(
            &mut n,
            &mut a,
            [stream, 0, 0, SERVICE + 48],
            &mut data,
            true,
            &mut errno,
        )?;
        assert!(
            invoke(
                &mut n,
                &mut a,
                [dst, 32, stream, SERVICE],
                &mut data,
                writable,
                &mut errno
            )
            .is_err()
        );
        assert_eq!(data, before);
        assert_eq!(
            invoke(
                &mut n,
                &mut a,
                [stream, 0, 0, SERVICE + 48],
                &mut data,
                true,
                &mut errno
            )?,
            position
        );
    }
    let mut short = [0xa5; 3];
    assert!(
        invoke(
            &mut n,
            &mut a,
            [u64::MAX, 0, stream, SERVICE],
            &mut data,
            true,
            &mut short
        )
        .is_err()
    );
    assert_eq!(short, [0xa5; 3]);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [stream, 0, 0, SERVICE + 96],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    let fd = invoke(
        &mut n,
        &mut a,
        [stream, 0, 0, SERVICE + 128],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(n.kernel.as_mut().unwrap().descriptors.close(fd), 0);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 1, stream, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        DATA + 128
    );
    data[128..160].fill(0xa5);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 32, stream, SERVICE],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(errno, 9u32.to_le_bytes());
    assert_eq!(&data[128..160], &[0xa5; 32]);
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [stream, 0, 0, SERVICE + 96],
            &mut data,
            true,
            &mut errno
        )?,
        1
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut a,
            [DATA + 128, 1, stream, SERVICE],
            &mut data,
            true,
            &mut []
        )?,
        DATA + 128
    );
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 16)?);
    assert!(
        invoke(
            &mut n,
            &mut a,
            [u64::MAX, 0, STDIN_FILE_ADDR, SERVICE],
            &mut data,
            true,
            &mut errno
        )
        .is_err()
    );
    assert!(n.configure_stdin(b"/app0/line").is_err());
    println!(
        "Native fgets normal streams and real __stdinp double-indirection callsite passed using explicit test input; no host assertion/input event exercised"
    );
    Ok(())
}
