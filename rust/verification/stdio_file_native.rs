//! Registered-file contents through actual pinned Doom/SDL stdio wrappers.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{memory_fixture, Execution, NativeExecutor, Service},
    stdio_file::Operation,
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
const ERRNO: u64 = 0x33000000;
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 5],
    data: &mut [u8],
    writable: bool,
    errno: &mut [u8],
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        0x100100,
        ARGS,
        &mut [r],
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
            DataRegion {
                address: ERRNO,
                bytes: errno,
                writable: true,
            },
        ],
    )? {
        Execution::WorkerReturned(value) => Ok(value),
        _ => Err("FILE call did not return".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 32, 0x48, 0x8b, 0x4f, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8,
        0x48, 0x8b, 0x3f, 0xff, 0xd0, 0xc3,
    ];
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.kernel
        .as_mut()
        .unwrap()
        .files
        .register(b"/app0/data", b"abcde".to_vec())?;
    n.kernel
        .as_mut()
        .unwrap()
        .files
        .register_writable(b"/app0/save", b"old".to_vec())?;
    n.stdio.configure_capture(128)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    for (index, (op, got)) in [
        (Operation::Open, Some(0x26f928)),
        (Operation::Read, Some(0x26f968)),
        (Operation::Seek, Some(0x26f958)),
        (Operation::Tell, Some(0x26f960)),
        (Operation::Close, Some(0x26f950)),
        (Operation::Fileno, Some(0x26f938)),
        (Operation::Eof, None),
        (Operation::Error, Some(0x26f970)),
        (Operation::Clearerr, None),
        (Operation::Write, Some(0x26f978)),
    ]
    .into_iter()
    .enumerate()
    {
        let target = 0x790000 + 16 * index as u64;
        n.register_service(target, Service::File(op))?;
        if let Some(got) = got {
            let address = BIAS + got;
            let s = real
                .segments
                .iter_mut()
                .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
                .ok_or("Missing FILE GOT")?;
            let at = (address - s.address) as usize;
            s.bytes[at..at + 8].copy_from_slice(&target.to_le_bytes());
        }
    }
    n.register_service(0x7900a0, Service::Fflush)?;
    let threaded_got = BIAS + 0x26f930;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| threaded_got >= s.address && threaded_got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing __isthreaded GOT")?;
    let offset = (threaded_got - segment.address) as usize;
    segment.bytes[offset..offset + 8].copy_from_slice(&(DATA + 240).to_le_bytes());
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x118470, 0x1184d0, 0x318f0, 0x31950, 0x319c0]),
            20000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut data = [0u8; 256];
    data[..11].copy_from_slice(b"/app0/data\0");
    data[16..19].copy_from_slice(b"rb\0");
    let mut errno = 77u32.to_le_bytes();
    let stream = invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, DATA + 16, 0, 0, BIAS + 0x118470],
        &mut data,
        true,
        &mut errno,
    )?;
    assert!(stream > u32::MAX as u64);
    assert_eq!(errno, 77u32.to_le_bytes());
    data[128 + 56..128 + 64].copy_from_slice(&stream.to_le_bytes());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 128, DATA + 32, 2, 3, BIAS + 0x31950],
            &mut data,
            true,
            &mut errno
        )?,
        2
    );
    assert_eq!(&data[32..37], b"abcde");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790060],
            &mut data,
            true,
            &mut errno
        )?,
        1
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 128, 0, 0, 0, BIAS + 0x318f0],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    // Bad destination cannot consume bytes or modify FILE metadata.
    let before = data;
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA + 254, 1, 5, stream, 0x790010],
        &mut data,
        true,
        &mut errno
    )
    .is_err());
    assert_eq!(data, before);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790030],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA + 32, 1, 2, stream, 0x790010],
        &mut data,
        false,
        &mut errno
    )
    .is_err());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 32, 1, 2, stream, 0x790010],
            &mut data,
            true,
            &mut errno
        )?,
        2
    );
    assert_eq!(&data[32..34], b"ab");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 32, u64::MAX, 2, stream, 0x790010],
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
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790070],
            &mut data,
            true,
            &mut errno
        )?,
        1
    );
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [stream, 0, 0, 0, 0x790080],
        &mut data,
        true,
        &mut errno,
    )?;
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790070],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    let fd = invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [stream, 0, 0, 0, 0x790050],
        &mut data,
        true,
        &mut errno,
    )?;
    // A raw descriptor seek must be visible to the FILE cursor.
    let token = n
        .kernel
        .as_ref()
        .unwrap()
        .descriptors
        .file_token(fd)
        .map_err(|e| e.to_string())?;
    n.kernel
        .as_mut()
        .unwrap()
        .descriptors
        .seek_file(&token, 4, 0)
        .map_err(|e| e.to_string())?;
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790030],
            &mut data,
            true,
            &mut errno
        )?,
        4
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [stream, 0, 0, 0, 0x790040],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [stream, 0, 0, 0, 0x790030],
        &mut data,
        true,
        &mut errno
    )
    .is_err());
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, 0, 0, 0, BIAS + 0x1184d0],
            &mut data,
            true,
            &mut errno
        )?,
        1
    );
    data[..11].copy_from_slice(b"/app0/nope\0");
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA + 16, 0, 0, BIAS + 0x118470],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    assert_eq!(errno, 2u32.to_le_bytes());
    data[16] = b'w';
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA + 16, 0, 0, BIAS + 0x118470],
            &mut data,
            true,
            &mut errno
        )?,
        0
    );
    // File registration materializes /app0 as a readonly directory.
    // Missing reads remain ENOENT; creation is rejected with EROFS.
    assert_eq!(errno, 30u32.to_le_bytes());
    macro_rules! call {
        ($args:expr) => {
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                $args,
                &mut data,
                true,
                &mut errno,
            )?
        };
    }
    data[..11].copy_from_slice(b"/app0/save\0");
    data[16..20].copy_from_slice(b"w+b\0");
    let writable = call!([DATA, DATA + 16, 0, 0, BIAS + 0x118470]);
    assert!(writable > u32::MAX as u64);
    assert_eq!(call!([writable, 0, 0, 0, 0x790030]), 0);
    data[80..86].copy_from_slice(b"abcdef");
    data[128 + 56..128 + 64].copy_from_slice(&writable.to_le_bytes());
    assert_eq!(call!([DATA + 128, DATA + 80, 2, 3, BIAS + 0x319c0]), 3);
    assert_eq!(
        call!([DATA + 128, u64::MAX, 0, u64::MAX, BIAS + 0x319c0]),
        0
    );
    assert_eq!(call!([writable, 0, 0, 0, 0x7900a0]), 0);
    assert_eq!(call!([writable, 0, 0, 0, 0x790020]), 0);
    assert_eq!(call!([DATA + 96, 1, 6, writable, 0x790010]), 6);
    assert_eq!(&data[96..102], b"abcdef");
    // Invalid source cannot advance the shared descriptor or modify file bytes.
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA + 255, 1, 2, writable, 0x790090],
        &mut data,
        true,
        &mut errno
    )
    .is_err());
    assert_eq!(call!([writable, 0, 0, 0, 0x790030]), 6);
    assert_eq!(call!([writable, 0, 0, 0, 0x790040]), 0);
    data[16..20].copy_from_slice(b"a+b\0");
    let append = call!([DATA, DATA + 16, 0, 0, BIAS + 0x118470]);
    assert_eq!(call!([append, 0, 0, 0, 0x790030]), 6);
    assert_eq!(call!([append, 0, 0, 0, 0x790020]), 0);
    assert_eq!(call!([DATA + 80, 1, 2, append, 0x790090]), 2);
    assert_eq!(call!([append, 0, 0, 0, 0x790030]), 8);
    assert_eq!(call!([append, 0, 0, 0, 0x790020]), 0);
    assert_eq!(call!([DATA + 96, 1, 8, append, 0x790010]), 8);
    assert_eq!(&data[96..104], b"abcdefab");
    assert_eq!(call!([append, 0, 0, 0, 0x790040]), 0);
    data[16..19].copy_from_slice(b"rb\0");
    let readonly = call!([DATA, DATA + 16, 0, 0, BIAS + 0x118470]);
    assert_eq!(call!([u64::MAX, 0, u64::MAX, readonly, 0x790090]), 0);
    // Overflow needs errno; truncated errno must stop before FILE error/orient
    // changes, independently of the deliberately inaccessible source pointer.
    assert!(invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [u64::MAX, 2, u64::MAX, readonly, 0x790090],
        &mut data,
        true,
        &mut errno[..3]
    )
    .is_err());
    assert_eq!(call!([readonly, 0, 0, 0, 0x790070]), 0);
    assert_eq!(call!([readonly, 0, 0, 0, 0x790030]), 0);
    assert_eq!(call!([u64::MAX, 1, 1, readonly, 0x790090]), 0);
    assert_eq!(errno, 9u32.to_le_bytes());
    assert_eq!(call!([readonly, 0, 0, 0, 0x790070]), 1);
    let fd = call!([readonly, 0, 0, 0, 0x790050]);
    let token = n
        .kernel
        .as_ref()
        .unwrap()
        .descriptors
        .file_token(fd)
        .map_err(|e| e.to_string())?;
    assert_eq!(n.kernel.as_mut().unwrap().descriptors.close_file(&token), 0);
    assert_eq!(call!([readonly, 0, 0, 0, 0x7900a0]), 0);
    assert_eq!(call!([0, 0, 0, 0, 0x7900a0]), 0);
    assert_eq!(call!([readonly, 0, 0, 0, 0x790040]), u32::MAX as u64);
    assert_eq!(
        call!([
            DATA + 80,
            2,
            3,
            native_ps5::guest_stdio::FILE_ADDR,
            0x790090
        ]),
        3
    );
    assert_eq!(n.stdio.bytes(), b"abcdef");
    assert_eq!(call!([0, 0, 0, 0, 0x7900a0]), 0);
    println!(
        "PASS: actual M_fopen/M_FileExists and SDL stdio read/seek, registered bytes, writable update/append/truncate/readback, stderr capture/flush, partial elements/EOF, shared cursor, errno/error/clearerr/close and atomic memory guards."
    );
    Ok(())
}
