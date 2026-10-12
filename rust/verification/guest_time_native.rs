//! Actual SDL timing functions using gettimeofday plus direct nullable/guard ABI checks.
use native_ps5::{
    guest_memory::DataRegion,
    guest_time::{TimeSource, Timeval},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
};
use std::{collections::BTreeSet, path::Path};
const BIAS: u64 = 0x10000000;
const DATA: u64 = 0x1_31000000;
const ARGS: u64 = 0x32000000;
const TIME: u64 = 0x790000;
#[allow(clippy::too_many_arguments)]
fn invoke(
    n: &mut NativeExecutor,
    a: &mut Image,
    r: &mut Image,
    args: [u64; 4],
    data: &mut [u8],
    writable: bool,
    entry: u64,
) -> Result<u64, String> {
    let mut descriptor: Vec<u8> = args.into_iter().flat_map(u64::to_le_bytes).collect();
    match n.run_worker_with_data(
        a,
        entry,
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
        ],
    )? {
        Execution::WorkerReturned(v) => Ok(v),
        _ => Err("Time wrapper did not return".into()),
    }
}
fn image_bytes(image: &Image, address: u64, count: usize) -> Result<&[u8], String> {
    let s = image
        .segments
        .iter()
        .find(|s| {
            address >= s.address && address + count as u64 <= s.address + s.bytes.len() as u64
        })
        .ok_or("Missing clock output")?;
    let at = (address - s.address) as usize;
    Ok(&s.bytes[at..at + count])
}
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut f = memory_fixture();
    let code = [
        0x48, 0x8b, 0x47, 24, 0x48, 0x8b, 0x57, 16, 0x48, 0x8b, 0x77, 8, 0x48, 0x8b, 0x3f, 0xff,
        0xd0, 0xc3,
    ];
    f[0x100..0x100 + code.len()].copy_from_slice(&code);
    // Direct timeval output overlaps future CALL return word.
    let mut frame = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x31, 0xf6, 0x48, 0xb8];
    frame.extend(TIME.to_le_bytes());
    frame.extend([0xff, 0xd0, 0xc3]);
    f[0x140..0x140 + frame.len()].copy_from_slice(&frame);
    let mut n = NativeExecutor::new(0x20000000, 65536, 100000, 100000)?;
    n.register_service(TIME, Service::Gettimeofday)?;
    let sample = Timeval {
        seconds: 0x1_0000_0001,
        microseconds: 999999,
    };
    n.guest_time.set_source(TimeSource::Fixed(sample));
    let mut real = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    let mut adapter = Image::load(&f, 0x100000, 65536)?;
    let address = BIAS + 0x26f908;
    let s = real
        .segments
        .iter_mut()
        .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
        .ok_or("Missing gettimeofday GOT")?;
    let off = (address - s.address) as usize;
    s.bytes[off..off + 8].copy_from_slice(&TIME.to_le_bytes());
    unsafe {
        n.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x200f0, 0x20120, 0x201a0, 0x201f0]),
            1000,
            archive,
            &directory.join("real"),
        )?;
        n.compile_image(
            &f,
            0x100000,
            BTreeSet::from([0x100, 0x140]),
            1000,
            archive,
            &directory.join("adapter"),
        )?;
    }
    let mut data = [0xa5; 48];
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, 0, 0, TIME],
            &mut data,
            false,
            0x100100
        )?,
        0
    );
    let before = data;
    assert!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA, DATA + 24, 0, TIME],
            &mut data,
            true,
            0x100100
        )
        .is_err()
    );
    assert_eq!(data, before); // timezone unconfigured
    n.guest_time.configure_utc();
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [DATA + 1, DATA + 25, 0, TIME],
            &mut data,
            true,
            0x100100
        )?,
        0
    );
    assert_eq!(&data[1..17], &sample.bytes()?);
    assert_eq!(&data[25..33], &[0; 8]);
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, DATA, 0, TIME],
            &mut data,
            true,
            0x100100
        )?,
        0
    );
    assert_eq!(&data[..8], &[0; 8]);
    data.fill(0xa5);
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, DATA + 8, 0, TIME],
        &mut data,
        true,
        0x100100,
    )?;
    assert_eq!(&data[..8], &sample.seconds.to_le_bytes());
    assert_eq!(&data[8..16], &[0; 8]);
    for (tv, tz, writable, entry) in [
        (DATA, 0, false, 0x100100),
        (DATA + 40, 0, true, 0x100100),
        (DATA, DATA + 44, true, 0x100100),
        (u64::MAX, 0, true, 0x100100),
        (0, u64::MAX, true, 0x100100),
        (0, 0, true, 0x100140),
    ] {
        let before = data;
        assert!(
            invoke(
                &mut n,
                &mut adapter,
                &mut real,
                [tv, tz, 0, TIME],
                &mut data,
                writable,
                entry
            )
            .is_err()
        );
        assert_eq!(data, before);
    }
    // Real guest SDL stores the full16-byte timeval and computes elapsed ticks.
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 100,
        microseconds: 900000,
    }));
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, 0, 0, BIAS + 0x200f0],
            &mut data,
            true,
            0x100100
        )?,
        0
    );
    assert_eq!(image_bytes(&real, BIAS + 0x590170, 1)?, [1]);
    assert_eq!(
        image_bytes(&real, BIAS + 0x590178, 16)?,
        Timeval {
            seconds: 100,
            microseconds: 900000
        }
        .bytes()?
    );
    n.guest_time.set_source(TimeSource::Fixed(Timeval {
        seconds: 102,
        microseconds: 100000,
    }));
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, 0, 0, BIAS + 0x20120],
            &mut data,
            true,
            0x100100
        )?,
        1200
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, 0, 0, BIAS + 0x201a0],
            &mut data,
            true,
            0x100100
        )?,
        102100000
    );
    assert_eq!(
        invoke(
            &mut n,
            &mut adapter,
            &mut real,
            [0, 0, 0, BIAS + 0x201f0],
            &mut data,
            true,
            0x100100
        )?,
        1000000
    );
    // Host wall time is bounded by samples bracketing one native call; no monotonic claim.
    n.guest_time.set_source(TimeSource::HostRealtime);
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_micros();
    invoke(
        &mut n,
        &mut adapter,
        &mut real,
        [DATA, 0, 0, TIME],
        &mut data,
        true,
        0x100100,
    )?;
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_micros();
    let sec = i64::from_le_bytes(data[..8].try_into().unwrap());
    let us = i64::from_le_bytes(data[8..16].try_into().unwrap());
    assert!((0..1000000).contains(&us));
    let actual = sec as u128 * 1000000 + us as u128;
    assert!(before <= actual && actual <= after);
    println!(
        "PASS: gettimeofday nullable/full64/unaligned outputs, explicitUTCtimezone and TVthenTZalias, guardedfailure boundary; actual SDL_TicksInit/GetTicks64/GetPerformanceCounter/Frequency and host realtime sample. Kernel EFAULT/partialcopy notimplemented."
    );
    Ok(())
}
