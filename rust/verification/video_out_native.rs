//! Synthetic guest VideoOut calls through translated ARM64 and actual Metal.
//! The injected flip-event fields are model policy, not console ABI evidence.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    video_out::FlipEventPolicy,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
const INPUT: u64 = 0x31000000;
const PIXELS: u64 = 0x32000000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    service: u64,
    args: [u64; 8],
    data: &mut [u8],
    pixels: &mut [u8],
) -> Result<i32, String> {
    let mut input = [0u8; 72];
    for (chunk, value) in input
        .chunks_exact_mut(8)
        .zip(args.into_iter().chain([service]))
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    match native.run_with_data(
        image,
        0x100100,
        Arguments {
            size: INPUT,
            pointer: 0,
        },
        &mut [],
        &mut [
            DataRegion {
                address: INPUT,
                bytes: &mut input,
                writable: false,
            },
            DataRegion {
                address: DATA,
                bytes: data,
                writable: true,
            },
            DataRegion {
                address: PIXELS,
                bytes: pixels,
                writable: false,
            },
        ],
    )? {
        Execution::Returned(value) => Ok(value),
        _ => Err("VideoOut fixture unexpectedly suspended".into()),
    }
}
fn word(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
fn main() -> Result<(), String> {
    let mut file = memory_fixture();
    // Save RBX; argument table in RDI. Push argument8, then argument7, then CALL.
    let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0xff, 0x73, 56, 0xff, 0x73, 48];
    for (reg, offset) in [(7, 0), (6, 8), (2, 16), (1, 24), (8, 32), (9, 40), (0, 64)] {
        code.extend([
            if reg >= 8 { 0x4c } else { 0x48 },
            0x8b,
            0x43 | ((reg & 7) << 3),
            offset,
        ]);
    }
    code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 16, 0x5b, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    for (address, service) in [
        (0x7b0000, Service::DirectAllocate),
        (0x7b0010, Service::DirectMap),
        (0x7b0020, Service::DirectRelease),
        (0x7b0030, Service::EqueueCreate),
        (0x7b0040, Service::EqueueWait),
        (0x7b0050, Service::EqueueDelete),
        (0x7c0000, Service::VideoOpen),
        (0x7c0010, Service::VideoClose),
        (0x7c0020, Service::VideoAddFlipEvent),
        (0x7c0030, Service::VideoDeleteFlipEvent),
        (0x7c0040, Service::VideoSetFlipRate),
        (0x7c0050, Service::VideoSetBufferAttribute),
        (0x7c0060, Service::VideoRegisterBuffers),
        (0x7c0070, Service::VideoSubmitFlip),
        (0x7d0000, Service::Memcpy),
    ] {
        native.register_service(address, service)?;
    }
    native
        .video_out
        .configure_event_policy(Some(FlipEventPolicy {
            ident: 41,
            filter: -13,
            flags: 1,
            fflags: 0,
        }))?;
    // SAFETY: local archive is built from the matching current Rust source.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100]),
            1000,
            Path::new("outputs/native-apple-silicon/rust/target/release/libnative_ps5.a"),
            Path::new("work/arm64-prototype/rust-checks"),
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 4096)?;
    let mut data = [0u8; 384];
    data[320..328].copy_from_slice(b"display\0");
    let mut pixels = vec![0; native_ps5::video_tile::required_bytes(128, 128)?];
    let allocation = 64 * 1024 * 1024u64;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7b0000,
            [allocation, 0x20000, 3, DATA, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    let physical = word(&data, 0);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7b0010,
            [DATA + 8, allocation, 0x33, 0, physical, 0x20000, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    let backing = word(&data, 8);
    for (slot, address) in [(64, backing), (96, backing + 32 * 1024 * 1024)] {
        data[slot..slot + 8].copy_from_slice(&address.to_le_bytes());
    }
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7b0030,
            [DATA + 16, DATA + 320, 0, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    let queue = word(&data, 16);
    let handle = invoke(
        &mut native,
        &mut image,
        0x7c0000,
        [255, 0, 0, 0, 0, 0, 0, 0],
        &mut data,
        &mut pixels,
    )? as u64;
    assert!(handle > 0 && handle <= i32::MAX as u64);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7c0020,
            [queue, handle, 0x1234, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7c0040,
            [handle, 0, 0, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    for argument in [6, 7] {
        let before = data;
        let mut invalid = [DATA + 128, 0x8000000022000000, 0, 128, 128, 0, 0, 0];
        invalid[argument] = 1;
        let error = invoke(
            &mut native,
            &mut image,
            0x7c0050,
            invalid,
            &mut data,
            &mut pixels,
        )
        .err()
        .ok_or("Nonzero attribute stack argument accepted")?;
        assert!(error.contains("Unsupported VideoOut attributes"), "{error}");
        assert_eq!(data, before);
    }
    invoke(
        &mut native,
        &mut image,
        0x7c0050,
        [DATA + 128, 0x8000000022000000, 0, 128, 128, 0, 0, 0],
        &mut data,
        &mut pixels,
    )?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7c0060,
            [handle, 0, 0, DATA + 64, 2, DATA + 128, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    let mut previous = 0.0;
    for frame in 0..2u64 {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(if frame == 0 {
                &[255, 64, 16, 255]
            } else {
                &[16, 128, 255, 255]
            });
        }
        invoke(
            &mut native,
            &mut image,
            0x7d0000,
            [
                backing + frame * 32 * 1024 * 1024,
                PIXELS,
                pixels.len() as u64,
                0,
                0,
                0,
                0,
                0,
            ],
            &mut data,
            &mut pixels,
        )?;
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x7c0070,
                [handle, frame, 1, frame + 1, 0, 0, 0, 0],
                &mut data,
                &mut pixels
            )?,
            0
        );
        let evidence = native
            .video_out
            .last_evidence()
            .ok_or("Missing presentation evidence")?;
        assert!(evidence.presented_time > previous);
        previous = evidence.presented_time;
        println!("PASS: translated VideoOut frame {frame} confirmed on host: {evidence:?}");
        assert_eq!(native.equeues.pending_count(queue)?, 1);
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x7b0040,
                [queue, DATA + 224, 1, DATA + 256, 0, 0, 0, 0],
                &mut data,
                &mut pixels
            )?,
            0
        );
        assert_eq!(&data[256..260], &1u32.to_le_bytes());
        assert_eq!(word(&data, 224), 41);
        assert_eq!(word(&data, 240), frame + 1);
        assert_eq!(word(&data, 248), 0x1234);
        assert_eq!(native.equeues.pending_count(queue)?, 0);
    }
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7c0030,
            [queue, handle, 0, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    invoke(
        &mut native,
        &mut image,
        0x7c0010,
        [handle, 0, 0, 0, 0, 0, 0, 0],
        &mut data,
        &mut pixels,
    )?;
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7b0050,
            [queue, 0, 0, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x7b0020,
            [physical, allocation, 0, 0, 0, 0, 0, 0],
            &mut data,
            &mut pixels
        )?,
        0
    );
    assert_eq!(native.direct_memory.reserved_bytes(), 0);
    assert_eq!(native.bytes_allocated(), 0);
    assert!(native.equeues.is_empty());
    println!(
        "PASS: synthetic guest VideoOut -> Metal -> Equeue path and cleanup; injected event policy, no game execution"
    );
    Ok(())
}
