use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for (service, options) in [
        (Service::SystemHideSplashScreen, 0),
        (Service::SystemLaunchWebBrowser, 0),
        (Service::SystemLaunchWebBrowser, u64::MAX),
    ] {
        let mut file = memory_fixture();
        let mut code = vec![0x48, 0xb8];
        code.extend(0x770000u64.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
        native.register_service(0x770000, service)?;
        // SAFETY: caller supplies this build's trusted local runtime archive.
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
            pointer: options,
        };
        let outcome = native.run(&mut image, 0x100100, args, &mut []);
        if matches!(service, Service::SystemHideSplashScreen) {
            assert!(matches!(outcome, Ok(Execution::Returned(0))));
            assert!(!native.system_service.splash_visible());
            assert!(matches!(
                native.run(&mut image, 0x100100, args, &mut [])?,
                Execution::Returned(0)
            ));
        } else {
            let error = outcome.err().ok_or("Browser unexpectedly succeeded")?;
            assert!(error.contains(if options == 0 {
                "backend unavailable"
            } else {
                "non-null options ABI"
            }));
            assert!(native.system_service.splash_visible());
        }
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
        assert_eq!(&image.segments[1].bytes, &file[0x500..0x600]);
    }
    println!(
        "PASS: translated SystemService calls hide owned splash state idempotently; unavailable browser/options stop explicitly without guest memory changes"
    );
    Ok(())
}
