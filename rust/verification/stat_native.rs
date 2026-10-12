//! Actual translated syscall bridge, independent of the real payload's files.
use native_ps5::{
    kernel::Kernel,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    verify_process_paths(archive, directory)?;
    for (present, output, error) in [
        (true, 0x100550u32, 0u32),
        (false, 0x100550, 2),
        (true, 0x100100, 14),
        (true, 0x1005b0, 14),
    ] {
        let mut file = memory_fixture();
        // stat(path, output), then save the library result and carry indicator.
        let mut code = vec![0xbf];
        code.extend(0x100520u32.to_le_bytes());
        code.push(0xbe);
        code.extend(output.to_le_bytes());
        code.push(0xb8);
        code.extend(188u32.to_le_bytes());
        code.extend([0x49, 0xbb]);
        code.extend(0x71000au64.to_le_bytes());
        code.extend([0x41, 0xff, 0xd3, 0xbb]);
        code.extend(0x100500u32.to_le_bytes());
        code.extend([0x48, 0x89, 0x03, 0x0f, 0x92, 0x43, 8, 0x31, 0xc0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        file[0x520..0x529].copy_from_slice(b"/fixture\0");
        file[0x550..0x5c8].fill(0xa5);
        let mut kernel = Kernel::new(4096, 0x1005f0)?;
        if present {
            kernel.files.register(b"/fixture", b"payload".to_vec())?;
        }
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
        native.kernel = Some(kernel);
        native.register_service(0x71000a, Service::Syscall)?;
        // SAFETY: caller supplies this build's trusted runtime archive.
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
        assert!(matches!(
            native.run(&mut image, 0x100100, Arguments::default(), &mut [])?,
            Execution::Returned(0)
        ));
        let data = &image.segments[1].bytes;
        assert_eq!(
            &data[..8],
            &if error == 0 { 0u64 } else { u64::MAX }.to_le_bytes()
        );
        assert_eq!(data[8], u8::from(error != 0));
        assert_eq!(&data[0xf0..0xf4], &error.to_le_bytes());
        if error == 0 {
            assert_eq!(&data[0x98..0xa0], &7u64.to_le_bytes());
        } else {
            assert_eq!(&data[0x50..0xc8], &[0xa5; 120]);
        }
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], code);
    }
    println!(
        "PASS: translated stat syscall returns owned-file size; missing paths, readonly and crossing outputs preserve memory with guest errno/carry"
    );
    Ok(())
}

fn verify_process_paths(archive: &Path, directory: &Path) -> Result<(), String> {
    for (number, capacity, output, error) in [
        (602u32, 32u64, 0x100550u32, 0u32),
        (602, 3, 0x100550, 34),
        (602, 32, 0x100100, 14),
        (326, 32, 0x100550, 0),
        (326, 3, 0x100550, 12),
        (326, 32, 0x100100, 14),
    ] {
        let mut file = memory_fixture();
        let mut code = vec![0xbf];
        code.extend(if number == 602 { 0u32 } else { output }.to_le_bytes());
        code.push(0xbe);
        code.extend(
            if number == 602 {
                output
            } else {
                capacity as u32
            }
            .to_le_bytes(),
        );
        code.push(0xba);
        code.extend(0x100530u32.to_le_bytes());
        code.push(0xb8);
        code.extend(number.to_le_bytes());
        code.extend([0x49, 0xbb]);
        code.extend(0x71000au64.to_le_bytes());
        code.extend([0x41, 0xff, 0xd3, 0xbb]);
        code.extend(0x100500u32.to_le_bytes());
        code.extend([0x48, 0x89, 0x03, 0x0f, 0x92, 0x43, 8, 0x31, 0xc0, 0xc3]);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        file[0x530..0x538].copy_from_slice(&capacity.to_le_bytes());
        file[0x550..0x570].fill(0xa5);
        let mut kernel = Kernel::new(4096, 0x1005f0)?;
        kernel.register_sandbox_component(b"guest_sandbox")?;
        kernel.register_working_directory(b"/app0")?;
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100)?;
        native.kernel = Some(kernel);
        native.register_service(0x71000a, Service::Syscall)?;
        // SAFETY: caller supplies this build's trusted runtime archive.
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
        assert!(matches!(
            native.run(&mut image, 0x100100, Arguments::default(), &mut [])?,
            Execution::Returned(0)
        ));
        let data = &image.segments[1].bytes;
        assert_eq!(
            &data[..8],
            &if error == 0 { 0u64 } else { u64::MAX }.to_le_bytes()
        );
        assert_eq!(data[8], u8::from(error != 0));
        assert_eq!(&data[0xf0..0xf4], &error.to_le_bytes());
        let expected: &[u8] = if number == 602 {
            b"guest_sandbox\0"
        } else {
            b"/app0\0"
        };
        if error == 0 {
            assert_eq!(&data[0x50..0x50 + expected.len()], expected);
            assert!(
                data[0x50 + expected.len()..0x70]
                    .iter()
                    .all(|&byte| byte == 0xa5)
            );
        } else {
            assert_eq!(&data[0x50..0x70], &[0xa5; 32]);
        }
        let length = if error == 0 && number == 602 {
            expected.len() as u64
        } else {
            capacity
        };
        assert_eq!(&data[0x30..0x38], &length.to_le_bytes());
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], code);
    }
    println!(
        "PASS: translated sandbox-path/getcwd calls return explicit guest metadata; short and readonly outputs preserve bytes and report errno/carry"
    );
    Ok(())
}
