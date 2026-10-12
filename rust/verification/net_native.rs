//! Actual translated service calls; only an explicitly enabled numeric address
//! is resolved. No DNS packet is needed and no fake hostname is returned.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
    net_dns::DnsBackend,
    net_resolver::Resolver,
};
use std::{collections::BTreeSet, path::Path, time::Duration};
const DATA: u64 = 0x30000000;
const INPUT: u64 = 0x31000000;
const BASE: u64 = 0x7f0000;
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    entry: u64,
    service: u64,
    args: [u64; 7],
    data: &mut [u8],
    writable: bool,
) -> Result<i32, String> {
    let mut input = [0; 64];
    for (chunk, value) in input
        .chunks_exact_mut(8)
        .zip(args.into_iter().chain([service]))
    {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    match native.run_with_data(
        image,
        entry,
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
                writable,
            },
        ],
    )? {
        Execution::Returned(value) => Ok(value),
        _ => Err("Net fixture unexpectedly suspended".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    // RBX owns the table; stack argument seven lies above the live return word.
    for (offset, overlap) in [(0x100, false), (0x140, true)] {
        let mut code = vec![0x53, 0x48, 0x89, 0xfb, 0xff, 0x73, 48];
        for (reg, off) in [(7, 0), (6, 8), (2, 16), (1, 24), (8, 32), (9, 40), (0, 56)] {
            code.extend([
                if reg >= 8 { 0x4c } else { 0x48 },
                0x8b,
                0x43 | ((reg & 7) << 3),
                off,
            ]);
        }
        if overlap {
            code.extend([0x48, 0x8d, 0x54, 0x24, 0xf8]);
        } // output = upcoming return word
        code.extend([0xff, 0xd0, 0x48, 0x83, 0xc4, 8, 0x5b, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    for (offset, store) in [(0x180, true), (0x1c0, false)] {
        let mut code = vec![0x48, 0xb8];
        code.extend(BASE.to_le_bytes());
        code.extend([0xff, 0xd0]);
        if store {
            code.extend([0xc7, 0x00, 42, 0, 0, 0]);
        }
        code.extend([0x8b, 0x00, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    let mut native = NativeExecutor::new(0x900000, 4096, 10000, 10000)?;
    for (i, service) in [
        Service::NetErrnoLoc,
        Service::NetPoolCreate,
        Service::NetPoolDestroy,
        Service::NetResolverCreate,
        Service::NetResolverDestroy,
        Service::NetResolverStartAton,
        Service::NetResolverStartNtoa,
    ]
    .into_iter()
    .enumerate()
    {
        native.register_service(BASE + i as u64 * 16, service)?;
    }
    // SAFETY: caller supplies locally built archive matching this source.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x180, 0x1c0]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 4096)?;
    for (entry, value) in [(0x1001c0, 0), (0x100180, 42), (0x1001c0, 42)] {
        assert!(
            matches!(native.run(&mut image,entry,Arguments::default(),&mut[])?,Execution::Returned(v)if v==value)
        );
    }
    let mut data = [0xa5; 128];
    data[..10].copy_from_slice(b"127.0.0.1\0");
    let pool = invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 16,
        [DATA, 16384, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    let resolver = invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 48,
        [DATA, pool as u64, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    assert!(pool > 0 && resolver > pool);
    let before = data;
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 96,
            [resolver as u64, DATA, DATA + 32, 0, 0, 0, 0],
            &mut data,
            true
        )
        .unwrap_err()
        .contains("disabled")
    );
    assert_eq!(data, before);
    data[32..36].copy_from_slice(&[127, 0, 0, 1]);
    let before = data;
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 80,
            [resolver as u64, DATA + 32, DATA + 48, 32, 0, 0, 0],
            &mut data,
            true
        )
        .unwrap_err()
        .contains("disabled")
    );
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 80,
            [resolver as u64, DATA + 32, DATA + 48, 32, 0, 0, 1],
            &mut data,
            true
        )
        .unwrap_err()
        .contains("timing/flags")
    );
    assert_eq!(data, before);
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 32,
            [pool as u64, 0, 0, 0, 0, 0, 0],
            &mut data,
            true
        )
        .is_err()
    );
    invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 64,
        [resolver as u64, 0, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 32,
        [pool as u64, 0, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    assert_eq!(
        (
            native.network.pool_count(),
            native.network.resolver_count(),
            native.network.reserved_bytes()
        ),
        (0, 0, 0)
    );
    // Explicit configuration only after closing prior resources. Numeric IPv4
    // parsing succeeds without entering the DNS-SD query path.
    native.network = Resolver::new(DnsBackend::enabled(Duration::from_millis(50))?);
    let pool = invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 16,
        [DATA, 16384, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    let resolver = invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 48,
        [DATA, pool as u64, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    data[32..36].fill(0xa5);
    let before = data;
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 96,
            [resolver as u64, DATA, DATA + 32, 0, 0, 0, 0],
            &mut data,
            false
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 96,
            [resolver as u64, DATA, DATA + 126, 0, 0, 0, 0],
            &mut data,
            true
        )
        .is_err()
    );
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100140,
            BASE + 96,
            [resolver as u64, DATA, 0, 0, 0, 0, 0],
            &mut data,
            true
        )
        .unwrap_err()
        .contains("live return frame")
    );
    assert_eq!(data, before);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 96,
            [resolver as u64, DATA, DATA + 32, 0, 0, 0, 0],
            &mut data,
            true
        )?,
        0
    );
    assert_eq!(&data[32..36], &[127, 0, 0, 1]);
    invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 64,
        [resolver as u64, 0, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100100,
            BASE + 96,
            [resolver as u64, DATA, DATA + 32, 0, 0, 0, 0],
            &mut data,
            true
        )
        .is_err()
    );
    invoke(
        &mut native,
        &mut image,
        0x100100,
        BASE + 32,
        [pool as u64, 0, 0, 0, 0, 0, 0],
        &mut data,
        true,
    )?;
    assert_eq!(native.network.reserved_bytes(), 0);
    println!(
        "PASS: translated Net lifecycle, persistent owned errno, default disabled forward/reverse, seventh flags, numeric IPv4, stale/readonly/boundary/return-frame failures; no DNS query performed"
    );
    Ok(())
}
