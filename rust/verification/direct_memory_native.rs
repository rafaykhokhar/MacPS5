//! Translated x86-to-ARM64 exercise of the pinned SDL direct-memory calls.
use native_ps5::{
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    const SIZE: u32 = 64 * 1024 * 1024;
    const VALUE: u64 = 0x0123456789abcdef;
    fn call(code: &mut Vec<u8>, address: u64) {
        code.extend([0x48, 0xb8]);
        code.extend(address.to_le_bytes());
        code.extend([0xff, 0xd0]);
    }
    fn immediate(code: &mut Vec<u8>, opcode: u8, value: u32) {
        code.push(opcode);
        code.extend(value.to_le_bytes());
    }
    for failure in 0..3 {
        let mut file = memory_fixture();
        let mut code = Vec::new();
        immediate(&mut code, 0xbb, 0x30000000); // RBX preserved output storage.
        immediate(&mut code, 0xbf, SIZE);
        immediate(&mut code, 0xbe, 0x20000);
        immediate(&mut code, 0xba, 3);
        immediate(
            &mut code,
            0xb9,
            if failure == 1 { 0x31000000 } else { 0x30000000 },
        );
        call(&mut code, 0x7b0000);
        immediate(
            &mut code,
            0xbf,
            if failure == 2 { 0x31000000 } else { 0x30000008 },
        );
        immediate(&mut code, 0xbe, SIZE);
        immediate(&mut code, 0xba, 0x33);
        code.extend([0x31, 0xc9, 0x4c, 0x8b, 0x03, 0x41, 0xb9]); // ECX0,R8=[RBX],R9Dalignment.
        code.extend(0x20000u32.to_le_bytes());
        call(&mut code, 0x7b0010);
        code.extend([0x48, 0x8b, 0x53, 8, 0x48, 0xb8]); // RDX=virtual address.
        code.extend(VALUE.to_le_bytes());
        code.extend([0x48, 0x89, 0x02, 0x48, 0x89, 0x82]);
        code.extend((SIZE - 8).to_le_bytes());
        code.extend([0x48, 0x8b, 0x02, 0x48, 0x89, 0x43, 16, 0x48, 0x8b, 0x82]);
        code.extend((SIZE - 8).to_le_bytes());
        code.extend([0x48, 0x89, 0x43, 24, 0x48, 0x8b, 0x3b]);
        immediate(&mut code, 0xbe, SIZE);
        call(&mut code, 0x7b0020);
        code.push(0xc3);
        assert!(code.len() <= 0x400);
        file[0x100..0x100 + code.len()].copy_from_slice(&code);
        let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
        for (address, service) in [
            (0x7b0000, Service::DirectAllocate),
            (0x7b0010, Service::DirectMap),
            (0x7b0020, Service::DirectRelease),
        ] {
            native.register_service(address, service)?;
        }
        // SAFETY: caller supplies the locally built matching runtime archive.
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
        let mut outputs = [0u8; 32];
        let mut readonly = [0u8; 8];
        let result = {
            let mut regions = [
                DataRegion {
                    address: 0x30000000,
                    bytes: &mut outputs,
                    writable: true,
                },
                DataRegion {
                    address: 0x31000000,
                    bytes: &mut readonly,
                    writable: false,
                },
            ];
            native.run_with_data(
                &mut image,
                0x100100,
                Arguments::default(),
                &mut [],
                &mut regions,
            )
        };
        if failure == 0 {
            assert!(matches!(result?, Execution::Returned(0)));
            let physical = u64::from_le_bytes(outputs[..8].try_into().unwrap());
            let virtual_address = u64::from_le_bytes(outputs[8..16].try_into().unwrap());
            assert_ne!(physical, 0);
            assert_ne!(virtual_address, 0);
            assert_eq!(physical % 0x20000, 0);
            assert_eq!(virtual_address % 0x20000, 0);
            assert_eq!(&outputs[16..24], &VALUE.to_le_bytes());
            assert_eq!(&outputs[24..32], &VALUE.to_le_bytes());
            assert_eq!(native.direct_memory.reserved_bytes(), 0);
        } else {
            assert!(
                result
                    .err()
                    .ok_or("Readonly direct-memory output unexpectedly succeeded")?
                    .contains("permission")
            );
            assert_eq!(
                native.direct_memory.reserved_bytes(),
                if failure == 1 { 0 } else { SIZE as u64 }
            );
            assert_eq!(&outputs[8..], &[0; 24]);
            if failure == 1 {
                assert_eq!(&outputs[..8], &[0; 8]);
            }
        }
        assert_eq!(native.bytes_allocated(), 0);
        assert_eq!(readonly, [0; 8]);
        assert_eq!(&image.segments[0].bytes[0x100..0x100 + code.len()], &code);
        assert_eq!(&image.segments[1].bytes, &file[0x500..0x600]);
    }
    println!(
        "PASS: translated SDL direct allocate/map64MiB/store/load/release; readonly outputs preserve reservation ownership and bytes"
    );
    Ok(())
}
