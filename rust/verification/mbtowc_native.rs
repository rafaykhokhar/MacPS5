//! Native C/POSIX mbtowc and pinned mbtowc_l forwarding wrapper.
//! Incomplete input returns low32 int -1 and EILSEQ, not size_t -2.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_81000001;
const OUT: u64 = 0x1_82000003;
const TABLE: u64 = 0x83000000;
const ERRNO: u64 = 0x84000000;
const SERVICE: u64 = 0x880000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    output: &mut [u8],
    writable: bool,
    errno: &mut [u8],
    errno_writable: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let before = data.to_vec();
    let allocated = n.bytes_allocated();
    let mut regions = vec![DataRegion {
        address: TABLE,
        bytes: &mut table,
        writable: false,
    }];
    if !data.is_empty() {
        regions.push(DataRegion {
            address: DATA,
            bytes: data,
            writable: false,
        });
    }
    if !output.is_empty() {
        regions.push(DataRegion {
            address: OUT,
            bytes: output,
            writable,
        });
    }
    if !errno.is_empty() {
        regions.push(DataRegion {
            address: ERRNO,
            bytes: errno,
            writable: errno_writable,
        });
    }
    let result = n.run_worker_with_data(adapter, entry, TABLE, &mut [real], &mut regions);
    drop(regions);
    assert_eq!(data, before);
    assert_eq!(n.bytes_allocated(), allocated);
    match result? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("mbtowc fixture suspended".into()),
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
        code.extend(u64::MAX.to_le_bytes());
        code.extend([0x48, 0x83, 0xec, 8]);
        if frame {
            code.extend([0x48, 0x8d, 0x7c, 0x24, 0xf8]);
        } // Destination aliases imminent CALL return slot.
        code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3]);
        fixture[entry..entry + code.len()].copy_from_slice(&code);
    }
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Mbtowc)?;
    n.register_service(SERVICE + 16, Service::Setlocale)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb10;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing mbtowc GOT")?;
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
            BTreeSet::from([0x116310]),
            1000,
            archive,
            &directory.join("real-wrapper"),
        )?;
    }
    let mut count = 0;
    for locale in [b"C\0".as_slice(), b"POSIX\0".as_slice()] {
        assert_ne!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [2, DATA, 0, SERVICE + 16],
                &mut locale.to_vec(),
                &mut [],
                false,
                &mut [],
                false,
                0x100100
            )?,
            0
        );
        for target in [SERVICE, BIAS + 0x116310] {
            for byte in 0..=255u16 {
                for length in [1, 2, 1u64 << 32, u64::MAX] {
                    for destination in [0, OUT] {
                        let mut output = [0xa5; 4];
                        let mut errno = 77u32.to_le_bytes();
                        assert_eq!(
                            invoke(
                                &mut n,
                                &mut adapter,
                                &mut real,
                                [destination, DATA, length, target],
                                &mut [byte as u8],
                                &mut output,
                                true,
                                &mut errno,
                                false,
                                0x100100
                            )?,
                            u64::from(byte != 0)
                        );
                        assert_eq!(
                            output,
                            if destination == 0 {
                                [0xa5; 4]
                            } else {
                                [byte as u8, 0, 0xa5, 0xa5]
                            }
                        );
                        assert_eq!(errno, 77u32.to_le_bytes());
                        count += 1;
                    }
                }
            }
            for length in [0, 1, u64::MAX] {
                let mut errno = 77u32.to_le_bytes();
                assert_eq!(
                    invoke(
                        &mut n,
                        &mut adapter,
                        &mut real,
                        [u64::MAX, 0, length, target],
                        &mut [],
                        &mut [],
                        false,
                        &mut errno,
                        false,
                        0x100100
                    )?,
                    0
                );
                assert_eq!(errno, 77u32.to_le_bytes());
                count += 1;
            }
            for destination in [0, OUT, u64::MAX] {
                let mut output = [0xa5; 4];
                let mut errno = 77u32.to_le_bytes();
                assert_eq!(
                    invoke(
                        &mut n,
                        &mut adapter,
                        &mut real,
                        [destination, u64::MAX, 0, target],
                        &mut [],
                        &mut output,
                        false,
                        &mut errno,
                        true,
                        0x100100
                    )?,
                    u32::MAX as u64
                );
                assert_eq!(errno, 86u32.to_le_bytes());
                assert_eq!(output, [0xa5; 4]);
                count += 1;
            }
        }
    }
    for target in [SERVICE, BIAS + 0x116310] {
        // Output faults must not publish partial wchar or assign errno.
        for (destination, length, writable, entry) in [
            (OUT, 1, true, 0x100100),
            (OUT, 2, false, 0x100100),
            (OUT + 3, 4, true, 0x100100),
            (u64::MAX, 4, true, 0x100100),
            (DATA, 4, true, 0x100100),
            (OUT, 4, true, 0x100200),
        ] {
            let mut output = vec![0xa5; length];
            let mut errno = 77u32.to_le_bytes();
            assert!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [destination, DATA, 1, target],
                    &mut [0x81],
                    &mut output,
                    writable,
                    &mut errno,
                    true,
                    entry
                )
                .is_err()
            );
            assert_eq!(output, vec![0xa5; length]);
            assert_eq!(errno, 77u32.to_le_bytes());
        }
        for source in [DATA + 1, u64::MAX] {
            let mut output = [0xa5; 2];
            let mut errno = 77u32.to_le_bytes();
            assert!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [OUT, source, 1, target],
                    &mut [65],
                    &mut output,
                    true,
                    &mut errno,
                    true,
                    0x100100
                )
                .is_err()
            );
            assert_eq!(output, [0xa5; 2]);
            assert_eq!(errno, 77u32.to_le_bytes());
        }
        // Exact two-byte writable output is enough; n never controls output extent.
        let mut output = [0xa5; 2];
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [OUT, DATA, u64::MAX, target],
                &mut [0xff],
                &mut output,
                true,
                &mut [],
                false,
                0x100100
            )?,
            1
        );
        assert_eq!(output, [255, 0]);
        for (length, writable) in [(0, true), (3, true), (4, false)] {
            let mut errno = vec![0xa5; length];
            let mut output = [0xa5; 2];
            assert!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [OUT, u64::MAX, 0, target],
                    &mut [],
                    &mut output,
                    true,
                    &mut errno,
                    writable,
                    0x100100
                )
                .is_err()
            );
            assert_eq!(errno, vec![0xa5; length]);
            assert_eq!(output, [0xa5; 2]);
        }
    }
    // CALL frame is stack_end−24 after adapter's alignment subtraction.
    n.kernel = Some(Kernel::new(4096, 0x20000000 + 65536 - 24)?);
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [u64::MAX, u64::MAX, 0, BIAS + 0x116310],
            &mut [],
            &mut [],
            false,
            &mut [],
            false,
            0x100100
        )
        .is_err()
    );
    n.kernel = None;
    for target in [SERVICE, BIAS + 0x116310] {
        let mut output = [0xa5; 2];
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [OUT, DATA, 1, target],
                &mut [128],
                &mut output,
                true,
                &mut [],
                false,
                0x100100
            )?,
            1
        );
        assert_eq!(output, [128, 0]);
        assert_eq!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [u64::MAX, 0, 0, target],
                &mut [],
                &mut [],
                false,
                &mut [],
                false,
                0x100100
            )?,
            0
        );
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [u64::MAX, u64::MAX, 0, target],
                &mut [],
                &mut [],
                false,
                &mut [],
                false,
                0x100100
            )
            .is_err()
        );
    }
    println!(
        "PASS: {count} native direct/pinned mbtowc C/POSIX value cases plus source/output/errno/frame guards; all256bytes, full64 n, exact optional unsigned16 write/no padding, reset precedence, low32 int−1/EILSEQ incomplete and no-errno success"
    );
    Ok(())
}
