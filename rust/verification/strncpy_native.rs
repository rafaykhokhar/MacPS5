//! Native bounded strncpy plus the unmodified pinned TXT_StringCopy wrapper.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x1_77000001;
const OUT: u64 = 0x1_78000001;
const ARGS: u64 = 0x79000000;
const ERRNO: u64 = 0x7a000000;
const SERVICE: u64 = 0x860000;
const BIAS: u64 = 0x10000000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    output: &mut [u8],
    output_writable: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut errno = 91u32.to_le_bytes();
    let mut regions = vec![
        DataRegion {
            address: ARGS,
            bytes: &mut table,
            writable: false,
        },
        DataRegion {
            address: DATA,
            bytes: data,
            writable,
        },
        DataRegion {
            address: ERRNO,
            bytes: &mut errno,
            writable: false,
        },
    ];
    if !output.is_empty() {
        regions.push(DataRegion {
            address: OUT,
            bytes: output,
            writable: output_writable,
        });
    }
    let result = n.run_worker_with_data(a, entry, ARGS, &mut [real], &mut regions);
    drop(regions);
    assert_eq!(errno, 91u32.to_le_bytes());
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("strncpy fixture suspended".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    fixture[96..104].copy_from_slice(&0x400u64.to_le_bytes());
    fixture[104..112].copy_from_slice(&0x400u64.to_le_bytes());
    for (entry, frame) in [(0x100, false), (0x200, true)] {
        let mut code = vec![
            0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f,
            0x48, 0xb9,
        ];
        code.extend(0xdeadbeefu64.to_le_bytes()); // Unused fourth argument sentinel.
        code.extend([0x48, 0x83, 0xec, 8]);
        if frame {
            code.extend([0x48, 0x8d, 0x7c, 0x24, 0xf8]);
        }
        code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3]);
        fixture[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Strncpy)?;

    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    // Authoritative relocation names establish provenance independently of
    // neighboring import slots. 0x26fb60 is ftell, not strncpy.
    let mut inventory = Image::load(file, 0, 64 * 1024 * 1024)?;
    inventory.relocate(|_| None)?;
    assert!(
        inventory
            .unresolved
            .iter()
            .any(|import| import.address == 0x26fb70 && import.name == "strncpy")
    );
    assert!(
        inventory
            .unresolved
            .iter()
            .any(|import| import.address == 0x26fb60 && import.name == "ftell")
    );
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let instruction = BIAS + 0x1c80ad;
    let code_segment = real
        .segments
        .iter()
        .find(|s| instruction >= s.address && instruction + 6 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing TXT_StringCopy code")?;
    let offset = (instruction - code_segment.address) as usize;
    let bytes = &code_segment.bytes[offset..offset + 6];
    assert_eq!(&bytes[..2], &[0xff, 0x25]); // JMP qword [RIP+disp32].
    let displacement = i32::from_le_bytes(bytes[2..6].try_into().unwrap());
    let got = instruction
        .wrapping_add(6)
        .wrapping_add(displacement as i64 as u64);
    assert_eq!(got, BIAS + 0x26fb70);
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing strncpy GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SERVICE.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100, 0x200]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1c80a0, 0x1c80ad]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut count = 0;
    for target in [SERVICE, BIAS + 0x1c80ad] {
        for source in [
            &b"\0"[..],
            &b"a\0"[..],
            &b"abc\0ignored"[..],
            &b"\x80\xff\0"[..],
            &b"abcdefghi"[..],
        ] {
            for length in 0..=8usize {
                if !source.contains(&0) && length > source.len() {
                    continue;
                }
                let mut input = source.to_vec();
                let before = input.clone();
                let mut output = [0xa5; 12];
                let mut expected = output;
                let mut ended = false;
                for i in 0..length {
                    let v = if ended { 0 } else { source[i] };
                    ended |= v == 0;
                    expected[1 + i] = v;
                }
                assert_eq!(
                    invoke(
                        &mut n,
                        &mut adapter,
                        &mut real,
                        [OUT + 1, DATA, length as u64, target],
                        &mut input,
                        false,
                        &mut output,
                        true,
                        0x100100
                    )?,
                    OUT + 1
                );
                assert_eq!(output, expected);
                assert_eq!(input, before);
                count += 1;
            }
        }
    }
    for destination in [0, u64::MAX, OUT] {
        let mut input = [1];
        let mut output = [0xa5];
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [destination, u64::MAX, 0, SERVICE],
                &mut input,
                false,
                &mut output,
                false,
                0x100100
            )?,
            destination
        );
        assert_eq!(output, [0xa5]);
    }
    // NUL at last mapped byte: padding requires no further source reads.
    let mut input = [0];
    let mut output = [0xa5; 8];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [OUT, DATA, 8, SERVICE],
            &mut input,
            false,
            &mut output,
            true,
            0x100100
        )?,
        OUT
    );
    assert_eq!(output, [0; 8]);
    // An unterminated exact n-byte input succeeds, with no destination NUL added.
    let mut input = *b"abc";
    let mut output = [0xa5; 4];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [OUT, DATA, 3, SERVICE],
            &mut input,
            false,
            &mut output,
            true,
            0x100100
        )?,
        OUT
    );
    assert_eq!(output, *b"abc\xa5");
    for (destination, source, length, writable, entry) in [
        (OUT, DATA, 4, true, 0x100100),
        (OUT, DATA, 3, false, 0x100100),
        (OUT + 3, DATA, 3, true, 0x100100),
        (DATA, DATA, 3, true, 0x100100),
        (DATA + 1, DATA, 2, true, 0x100100),
        (OUT, DATA, 3, true, 0x100200),
        (OUT, DATA, 65537, true, 0x100100),
        (OUT, DATA, 1u64 << 32, true, 0x100100),
        (u64::MAX, DATA, 3, true, 0x100100),
    ] {
        let mut input = *b"abc";
        let mut output = [0xa5; 4];
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [destination, source, length, SERVICE],
                &mut input,
                true,
                &mut output,
                writable,
                entry
            )
            .is_err()
        );
        assert_eq!(input, *b"abc");
        assert_eq!(output, [0xa5; 4]);
    }
    // Overlap is based on bytes actually read: padding can occupy later mapped
    // bytes after a source NUL when the single read byte itself is disjoint.
    let mut input = [0, 0xa5, 0xa5, 0xa5];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 1, DATA, 3, SERVICE],
            &mut input,
            true,
            &mut [],
            false,
            0x100100
        )?,
        DATA + 1
    );
    assert_eq!(input, [0; 4]);
    // Complete TXT_StringCopy adds its own final NUL, then calls strncpy(n-1).
    for length in [1u64, 4] {
        let mut input = *b"abcdef";
        let mut output = [0xa5; 6];
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [OUT, DATA, length, BIAS + 0x1c80a0],
                &mut input,
                false,
                &mut output,
                true,
                0x100100
            )?,
            OUT
        );
        let mut expected = [0xa5; 6];
        let copied = length as usize - 1;
        expected[..copied].copy_from_slice(&input[..copied]);
        expected[copied] = 0;
        assert_eq!(output, expected);
    }
    println!(
        "PASS: {count} native strncpy direct/verified strncpy tail-jump cases plus complete TXT_StringCopy, full64 return/n0 no-access, bounded source reads, exact zero padding and unterminated truncation, readonly/short/output/frame/overlap/high-count atomic guards, unchanged errno"
    );
    Ok(())
}
