//! Translated keyboard lifecycle using explicitly supplied device snapshots.
use native_ps5::{
    guest_memory::DataRegion,
    keyboard::KeyboardState,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    for (offset, address) in [
        (0x100, 0x7d0000u64),
        (0x140, 0x7d0010),
        (0x180, 0x7d0020),
        (0x1c0, 0x7d0030),
    ] {
        let mut code = Vec::new();
        if offset == 0x140 {
            code.extend([0x48, 0x89, 0xf1, 0x31, 0xf6, 0x31, 0xd2]);
        } // opaque RCX <- RSI, port/index0
        code.extend([0x48, 0xb8]);
        code.extend(address.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    for (address, service) in [
        (0x7d0000, Service::KeyboardInitialize),
        (0x7d0010, Service::KeyboardOpen),
        (0x7d0020, Service::KeyboardReadState),
        (0x7d0030, Service::KeyboardClose),
    ] {
        native.register_service(address, service)?;
    }
    // SAFETY: caller supplies the matching locally built runtime archive.
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
    assert!(matches!(
        native.run(&mut image, 0x100100, Arguments::default(), &mut [])?,
        Execution::Returned(0)
    ));
    assert!(native.keyboard.initialized());
    let snapshot = KeyboardState {
        available: true,
        modifiers: 2,
        scancodes: [4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    };
    native.keyboard.register_device(1, 1, snapshot.clone())?;
    // Non-null opaque data remains unsupported; never read an uninitialized host/guest struct.
    assert!(
        native
            .run(
                &mut image,
                0x100140,
                Arguments {
                    size: 1,
                    pointer: 0xdeadbeef
                },
                &mut []
            )
            .is_err()
    );
    let handle = match native.run(
        &mut image,
        0x100140,
        Arguments {
            size: 1,
            pointer: 0,
        },
        &mut [],
    )? {
        Execution::Returned(value) if value > 0 => value,
        _ => return Err("Keyboard open did not return a handle".into()),
    };
    let mut output = [0xa5u8; 112];
    for writable in [false, true] {
        let outcome = native.run_with_data(
            &mut image,
            0x100180,
            Arguments {
                size: handle as u64,
                pointer: 0x30000008,
            },
            &mut [],
            &mut [DataRegion {
                address: 0x30000000,
                bytes: &mut output,
                writable,
            }],
        );
        if writable {
            assert!(matches!(outcome?, Execution::Returned(0)));
            assert_eq!(&output[8..104], &snapshot.encode()?);
            assert_eq!(&output[..8], &[0xa5; 8]);
            assert_eq!(&output[104..], &[0xa5; 8]);
        } else {
            assert!(
                outcome
                    .err()
                    .ok_or("Readonly keyboard output succeeded")?
                    .contains("Keyboard state output fault: Write")
            );
            assert_eq!(output, [0xa5; 112]);
        }
    }
    let released = KeyboardState {
        available: true,
        ..Default::default()
    };
    native.keyboard.update_device(1, released.clone())?;
    assert!(matches!(
        native.run_with_data(
            &mut image,
            0x100180,
            Arguments {
                size: handle as u64,
                pointer: 0x30000008
            },
            &mut [],
            &mut [DataRegion {
                address: 0x30000000,
                bytes: &mut output,
                writable: true
            }]
        )?,
        Execution::Returned(0)
    ));
    assert_eq!(&output[8..104], &released.encode()?);
    assert!(matches!(
        native.run(
            &mut image,
            0x1001c0,
            Arguments {
                size: handle as u64,
                pointer: 0
            },
            &mut []
        )?,
        Execution::Returned(0)
    ));
    assert!(
        native
            .run_with_data(
                &mut image,
                0x100180,
                Arguments {
                    size: handle as u64,
                    pointer: 0x30000008
                },
                &mut [],
                &mut [DataRegion {
                    address: 0x30000000,
                    bytes: &mut output,
                    writable: true
                }]
            )
            .is_err()
    );
    assert_eq!(&output[8..104], &released.encode()?);
    println!(
        "PASS: translated Keyboard init/open/read/release/close with explicit fixture snapshot; readonly/stale/opaque-argument failures preserve output; no host key-input claim"
    );
    Ok(())
}
