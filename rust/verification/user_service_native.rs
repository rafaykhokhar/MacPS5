use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
fn call(code: &mut Vec<u8>, target: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(target.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for (output, capacity) in [(0x100540u32, 32u32), (0x100540, 3), (0x100100, 32)] {
        let mut native = NativeExecutor::new(0x900000, 4096, 200, 200)?;
        native.user_service.register_user(1, b"Player 1")?;
        native.user_service.set_foreground_user(1)?;
        for (address, service) in [
            (0x780000, Service::UserInitialize),
            (0x780010, Service::UserGetForeground),
            (0x780020, Service::UserGetLoginList),
            (0x780030, Service::UserGetName),
        ] {
            native.register_service(address, service)?;
        }
        let mut file = memory_fixture();
        let mut code = vec![0xbb];
        code.extend(0x100500u32.to_le_bytes());
        code.extend([0x31, 0xff]);
        call(&mut code, 0x780000);
        code.extend([0x89, 0x03]);
        code.extend([0x31, 0xff]);
        call(&mut code, 0x780000);
        code.extend([0x89, 0x43, 4]);
        code.push(0xbf);
        code.extend(0x100510u32.to_le_bytes());
        call(&mut code, 0x780010);
        code.push(0xbf);
        code.extend(0x100520u32.to_le_bytes());
        call(&mut code, 0x780020);
        code.extend([0xbf, 1, 0, 0, 0, 0xbe]);
        code.extend(output.to_le_bytes());
        code.push(0xba);
        code.extend(capacity.to_le_bytes());
        call(&mut code, 0x780030);
        code.extend([0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        file[0x500..0x600].fill(0xa5);
        // SAFETY: exact trusted runtime archive supplied by the verifier.
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100]),
                200,
                archive,
                directory,
            )?;
        }
        let mut image = Image::load(&file, 0x100000, 4096)?;
        let result = native.run(&mut image, 0x100100, Arguments::default(), &mut []);
        let data = &image.segments[1].bytes;
        assert_eq!(&data[..4], &0u32.to_le_bytes());
        assert_eq!(&data[4..8], &0x80960003u32.to_le_bytes());
        assert_eq!(&data[0x10..0x14], &1i32.to_le_bytes());
        assert_eq!(&data[0x20..0x24], &1i32.to_le_bytes());
        assert_eq!(&data[0x24..0x30], &[0xff; 12]);
        assert!(native.user_service.initialized());
        if output == 0x100540 && capacity == 32 {
            assert!(matches!(result, Ok(Execution::Returned(0))));
            assert_eq!(&data[0x40..0x49], b"Player 1\0");
            assert_eq!(&data[0x49..0x60], &[0xa5; 23]);
        } else {
            assert!(result.is_err());
            assert_eq!(&data[0x40..0x60], &[0xa5; 32]);
        }
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
    }
    println!(
        "PASS: translated UserService initialization, foreground, four-slot login list and name use configured guest profile; short/readonly name writes remain atomic"
    );
    Ok(())
}
