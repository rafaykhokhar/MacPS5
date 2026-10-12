//! Actual translated C-locale calls; owned readonly return storage survives yield.
use native_ps5::{
    guest_locale::EmptyEnvironmentPolicy,
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
const DATA: u64 = 0x30000000;
fn invoke(
    n: &mut NativeExecutor,
    i: &mut Image,
    target: u64,
    args: [u64; 3],
    data: &mut [u8],
) -> Result<u64, String> {
    let mut table = [0u8; 40];
    for (c, v) in table
        .chunks_exact_mut(8)
        .zip(args.into_iter().chain([target, 0]))
    {
        c.copy_from_slice(&v.to_le_bytes());
    }
    match n.run_with_data(
        i,
        0x100100,
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
        Execution::Returned(0) => Ok(u64::from_le_bytes(table[32..40].try_into().unwrap())),
        _ => Err("Locale fixture failed/suspended".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    let mut code = vec![0x53, 0x48, 0x89, 0xfb];
    for (reg, off) in [(7, 0), (6, 8), (2, 16), (0, 24)] {
        code.extend([0x48, 0x8b, 0x43 | (reg << 3), off]);
    }
    code.extend([0xff, 0xd0, 0x48, 0x89, 0x43, 32, 0x31, 0xc0, 0x5b, 0xc3]);
    file[0x100..0x100 + code.len()].copy_from_slice(&code);
    let mut yield_code = vec![0x48, 0xb8];
    yield_code.extend(0x721030u64.to_le_bytes());
    yield_code.extend([0xff, 0xd0, 0xc3]);
    file[0x180..0x180 + yield_code.len()].copy_from_slice(&yield_code);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    for (address, service) in [
        (0x721000, Service::Setlocale),
        (0x721010, Service::Memcpy),
        (0x721020, Service::Memset),
        (0x721030, Service::Dlsym),
    ] {
        native.register_service(address, service)?;
    }
    // SAFETY: caller supplies matching locally built runtime archive.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x180]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 4096)?;
    let mut bytes = [0xa5; 64];
    bytes[..21].copy_from_slice(b"C\0POSIX\0fr_FR.UTF-8\0\0");
    let name = invoke(&mut native, &mut image, 0x721000, [0, 0, 0], &mut bytes)?;
    assert!(name > u32::MAX as u64);
    for category in 0..=6 {
        for source in [0, DATA, DATA + 2] {
            assert_eq!(
                invoke(
                    &mut native,
                    &mut image,
                    0x721000,
                    [category, source, 0],
                    &mut bytes
                )?,
                name
            );
        }
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x721000,
                [category, DATA + 8, 0],
                &mut bytes
            )?,
            0
        );
    }
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x721000,
            [5, DATA + 20, 0],
            &mut bytes
        )
        .unwrap_err()
        .contains("explicit guest environment")
    );
    native
        .locale
        .configure_empty_environment(EmptyEnvironmentPolicy::C);
    assert_eq!(
        invoke(
            &mut native,
            &mut image,
            0x721000,
            [5, DATA + 20, 0],
            &mut bytes
        )?,
        name
    );
    for category in [u64::MAX, 7] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x721000,
                [category, 0, 0],
                &mut bytes
            )?,
            0
        );
    }
    for source in [1, DATA + 63, u64::MAX] {
        assert!(
            invoke(
                &mut native,
                &mut image,
                0x721000,
                [0, source, 0],
                &mut bytes
            )
            .is_err()
        );
    }
    assert!(invoke(&mut native, &mut image, 0x721020, [name, 65, 1], &mut bytes).is_err());
    invoke(
        &mut native,
        &mut image,
        0x721010,
        [DATA + 32, name, 2],
        &mut bytes,
    )?;
    assert_eq!(&bytes[32..34], b"C\0");
    let mut continuation = match native.run(&mut image, 0x100180, Arguments::default(), &mut [])? {
        Execution::Dlsym { continuation, .. } => continuation,
        _ => return Err("Expected locale suspension fixture".into()),
    };
    native.with_suspended_memory(&mut continuation, &mut image, &mut [], &mut [], |memory| {
        assert_eq!(memory.read(name, 2)?, b"C\0");
        assert!(memory.write(name, b"X").is_err());
        Ok(())
    })?;
    println!(
        "PASS: translated setlocale seven categories/C/POSIX/query/explicit-empty policy, NULL unavailablelocale, stable readonly guest name across suspended contexts; no host locale access"
    );
    Ok(())
}
