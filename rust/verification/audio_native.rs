use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for format in [0u32, 1, 3, 4] {
        let mut file = memory_fixture();
        let mut code = Vec::new();
        fn call(code: &mut Vec<u8>, address: u64) {
            code.extend([0x48, 0xb8]);
            code.extend(address.to_le_bytes());
            code.extend([0xff, 0xd0]);
        }
        call(&mut code, 0x7a0000);
        code.extend([0xbf, 0xff, 0, 0, 0, 0x31, 0xf6, 0x31, 0xd2, 0xb9]);
        code.extend(256u32.to_le_bytes());
        code.extend([0x41, 0xb8]);
        code.extend(48000u32.to_le_bytes());
        code.extend([0x41, 0xb9]);
        code.extend(format.to_le_bytes());
        call(&mut code, 0x7a0010);
        code.extend([0x89, 0xc3]); // Keep returned handle in EBX.
        for _ in 0..4 {
            code.extend([0x89, 0xdf, 0x48, 0xbe]);
            code.extend(0x30000000u64.to_le_bytes());
            call(&mut code, 0x7a0020);
        }
        code.extend([0x89, 0xdf]);
        call(&mut code, 0x7a0030);
        code.push(0xc3);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
        for (address, service) in [
            (0x7a0000, Service::AudioInitialize),
            (0x7a0010, Service::AudioOpen),
            (0x7a0020, Service::AudioOutput),
            (0x7a0030, Service::AudioClose),
        ] {
            native.register_service(address, service)?;
        }
        // SAFETY: matching locally built trusted runtime archive.
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                1000,
                archive,
                directory,
            )?;
        }
        let mut image = Image::load(&file, 0x100000, 4096)?;
        let mut silence = vec![0u8; 2048];
        let mut data = [DataRegion {
            address: 0x30000000,
            bytes: &mut silence,
            writable: false,
        }];
        let result = native.run_with_data(
            &mut image,
            0x100100,
            Arguments {
                size: 0,
                pointer: 0,
            },
            &mut [],
            &mut data,
        )?;
        assert!(matches!(result, Execution::Returned(0)));
        assert!(native.audio.initialized());
        assert_eq!(native.audio.active_count(), 0);
        assert!(silence.iter().all(|&byte| byte == 0));
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
        assert_eq!(&image.segments[1].bytes, &file[0x500..0x600]);
    }
    println!(
        "PASS: translated AudioOut init/open/output/close submits four silent PCM fragments in each SDL format through macOS AudioQueue; source memory unchanged and ports closed; no audible-output claim"
    );
    Ok(())
}
