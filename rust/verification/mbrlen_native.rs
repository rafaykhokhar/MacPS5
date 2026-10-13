//! C/POSIX mbrlen, direct service and actual pinned mbrlen_l tail wrapper.
//! Stateless single-byte policy; no UTF-8 or Sony-libc binary equivalence claim.
use native_ps5::{
    guest_memory::DataRegion,
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_7b000001;
const STATE: u64 = 0x1_7c000003;
const TABLE: u64 = 0x7d000000;
const ERRNO: u64 = 0x7e000000;
const SERVICE: u64 = 0x870000;
fn invoke(
    n: &mut NativeExecutor,
    adapter: &mut Image,
    real: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut table: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    let before = data.to_vec();
    let mut state = [0xa5; 128];
    let mut errno = 77u32.to_le_bytes();
    let allocated = n.bytes_allocated();
    let mut regions = vec![
        DataRegion {
            address: TABLE,
            bytes: &mut table,
            writable: false,
        },
        DataRegion {
            address: STATE,
            bytes: &mut state,
            writable: false,
        },
        DataRegion {
            address: ERRNO,
            bytes: &mut errno,
            writable: false,
        },
    ];
    if !data.is_empty() {
        regions.push(DataRegion {
            address: DATA,
            bytes: data,
            writable: false,
        });
    }
    let result = n.run_worker_with_data(adapter, 0x100100, TABLE, &mut [real], &mut regions);
    drop(regions);
    assert_eq!(data, before);
    assert_eq!(state, [0xa5; 128]);
    assert_eq!(errno, 77u32.to_le_bytes());
    assert_eq!(n.bytes_allocated(), allocated);
    match result? {
        Execution::WorkerReturned(value) => Ok(value),
        _ => Err("mbrlen fixture suspended".into()),
    }
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut fixture = memory_fixture();
    let mut code = vec![
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0x48,
        0xb9,
    ];
    code.extend(u64::MAX.to_le_bytes()); // Actual forwarding thunk ignores locale argument RCX.
    code.extend([0x48, 0x83, 0xec, 8, 0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0xc3]);
    fixture[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut n = NativeExecutor::new(0x20000000, 65536, 10000, 10000)?;
    n.kernel = Some(Kernel::new(4096, ERRNO)?);
    n.register_service(SERVICE, Service::Mbrlen)?;
    n.register_service(SERVICE + 16, Service::Setlocale)?;
    let mut adapter = Image::load(&fixture, 0x100000, 65536)?;
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let got = BIAS + 0x26fb08;
    let segment = real
        .segments
        .iter_mut()
        .find(|s| got >= s.address && got + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing mbrlen GOT")?;
    let at = (got - segment.address) as usize;
    segment.bytes[at..at + 8].copy_from_slice(&SERVICE.to_le_bytes());
    unsafe {
        n.compile_image(
            &fixture,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x1162e0]),
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
                &mut locale.to_vec()
            )?,
            0
        );
        for target in [SERVICE, BIAS + 0x1162e0] {
            // Each call exposes exactly one source byte, even with huge n.
            for byte in 0..=255u16 {
                for length in [1, 2, 1u64 << 32, u64::MAX] {
                    let state = [0, STATE, u64::MAX, DATA][byte as usize % 4];
                    assert_eq!(
                        invoke(
                            &mut n,
                            &mut adapter,
                            &mut real,
                            [DATA, length, state, target],
                            &mut [byte as u8]
                        )?,
                        u64::from(byte != 0)
                    );
                    count += 1;
                }
            }
            for state in [0, STATE, u64::MAX, DATA] {
                for length in [0, 1, 1u64 << 32, u64::MAX] {
                    assert_eq!(
                        invoke(
                            &mut n,
                            &mut adapter,
                            &mut real,
                            [0, length, state, target],
                            &mut []
                        )?,
                        0
                    );
                    count += 1;
                }
                for source in [DATA, u64::MAX] {
                    assert_eq!(
                        invoke(
                            &mut n,
                            &mut adapter,
                            &mut real,
                            [source, 0, state, target],
                            &mut []
                        )?,
                        u64::MAX - 1
                    );
                    count += 1;
                    assert!(
                        invoke(
                            &mut n,
                            &mut adapter,
                            &mut real,
                            [source, 1, state, target],
                            &mut []
                        )
                        .is_err()
                    );
                    count += 1;
                }
            }
            // A known byte just before the unmapped boundary succeeds; the
            // next byte faults instead of wrapping/truncating the high pointer.
            assert_eq!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [DATA, 1, STATE, target],
                    &mut [0xff]
                )?,
                1
            );
            count += 1;
            assert!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [DATA + 1, 1, STATE, target],
                    &mut [0xff]
                )
                .is_err()
            );
            count += 1;
        }
    }
    // Success/incomplete/reset require no configured errno context at all.
    n.kernel = None;
    for target in [SERVICE, BIAS + 0x1162e0] {
        for (source, length, value) in [(DATA, u64::MAX, 1), (DATA, 0, u64::MAX - 1), (0, 0, 0)] {
            assert_eq!(
                invoke(
                    &mut n,
                    &mut adapter,
                    &mut real,
                    [source, length, u64::MAX, target],
                    &mut [0x80]
                )?,
                value
            );
            count += 1;
        }
    }
    println!(
        "PASS: {count} native C/POSIX mbrlen checks: direct and pinned wrapper, all256bytes, full64 counts/incomplete result, NULL/n0 precedence, one-byte bounds, ignored state and unchanged readonly source/state/errno/heap"
    );
    Ok(())
}
