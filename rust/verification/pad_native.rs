use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for (service, operation) in [
        (Service::PadInitialize, "scePadInit"),
        (Service::PadOpen, "scePadOpen"),
        (Service::PadClose, "scePadClose"),
        (Service::PadReadState, "scePadReadState"),
        (Service::PadSetLightBar, "scePadSetLightBar"),
        (Service::PadSetVibration, "scePadSetVibration"),
        (Service::PadSetVibrationMode, "scePadSetVibrationMode"),
    ] {
        let mut file = memory_fixture();
        let mut code = vec![0x48, 0xb8];
        code.extend(0x790000u64.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
        native.register_service(0x790000, service)?;
        // SAFETY: the caller supplies the matching trusted runtime archive.
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                archive,
                directory,
            )?;
        }
        let mut image = Image::load(&file, 0x100000, 4096)?;
        let args = Arguments {
            size: u64::MAX,
            pointer: 0,
        };
        if matches!(service, Service::PadInitialize) {
            assert!(matches!(
                native.run(&mut image, 0x100100, args, &mut [])?,
                Execution::Returned(0)
            ));
            assert!(native.pad.initialized());
            assert!(
                native
                    .run(&mut image, 0x100100, args, &mut [])
                    .err()
                    .ok_or("Repeated Pad init unexpectedly succeeded")?
                    .contains("repeated initialization")
            );
        } else {
            for initialized in [false, true] {
                if initialized {
                    native.pad.initialize()?;
                }
                let error = native
                    .run(&mut image, 0x100100, args, &mut [])
                    .err()
                    .ok_or("Absent Pad device unexpectedly succeeded")?;
                assert!(error.contains(operation));
                assert!(error.contains(if initialized {
                    "no connected controller backend"
                } else {
                    "not initialized"
                }));
            }
        }
        assert_eq!(native.pad.connected_count(), 0);
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
        assert_eq!(&image.segments[1].bytes, &file[0x500..0x600]);
    }
    println!(
        "PASS: seven translated Pad services preserve lifecycle and reject absent devices without fabricated handles or guest output"
    );
    verify_input(archive, directory)?;
    Ok(())
}

fn verify_input(archive: &Path, directory: &Path) -> Result<(), String> {
    for output in [0x100500u64, 0x100100] {
        let mut file = memory_fixture();
        let mut code = Vec::new();
        fn call(code: &mut Vec<u8>, target: u64) {
            code.extend([0x48, 0xb8]);
            code.extend(target.to_le_bytes());
            code.extend([0xff, 0xd0]);
        }
        call(&mut code, 0x790000);
        code.extend([0xbf, 1, 0, 0, 0, 0x31, 0xf6, 0x31, 0xd2, 0x31, 0xc9]);
        call(&mut code, 0x790010);
        code.extend([0x89, 0xc7, 0x48, 0xbe]);
        code.extend(output.to_le_bytes());
        call(&mut code, 0x790030);
        code.push(0xc3);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
        for (address, service) in [
            (0x790000, Service::PadInitialize),
            (0x790010, Service::PadOpen),
            (0x790030, Service::PadReadState),
        ] {
            native.register_service(address, service)?;
        }
        let state = native_ps5::pad_data::PadData {
            connected: true,
            buttons: native_ps5::pad_data::BUTTON_CROSS,
            left_stick: [0, 255],
            right_stick: [128, 64],
            triggers: [32, 192],
            ..Default::default()
        };
        native.pad.register_device(17, 1, state.clone())?;
        // SAFETY: caller provides this build's trusted runtime archive.
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                100,
                archive,
                directory,
            )?;
        }
        let mut image = Image::load(&file, 0x100000, 4096)?;
        let result = native.run(
            &mut image,
            0x100100,
            Arguments {
                size: 0,
                pointer: 0,
            },
            &mut [],
        );
        if output == 0x100500 {
            assert!(matches!(result?, Execution::Returned(0)));
            assert_eq!(&image.segments[1].bytes[..120], &state.encode());
            assert_eq!(&image.segments[1].bytes[120..], &file[0x578..0x600]);
        } else {
            assert!(
                result
                    .err()
                    .ok_or("Readonly Pad output unexpectedly succeeded")?
                    .contains("PadData output memory fault")
            );
            assert_eq!(&image.segments[1].bytes, &file[0x500..0x600]);
        }
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
    }
    println!(
        "PASS: translated Pad init/open/read copies an explicitly supplied controller snapshot to 120 guarded guest bytes; readonly output fails atomically"
    );
    Ok(())
}
