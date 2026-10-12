//! Translated IME lifecycle with explicit synthetic backend completions.
use native_ps5::{
    guest_memory::DataRegion,
    ime_dialog::{Backend, Completion, Request},
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
    path::Path,
    rc::Rc,
};
struct Fixture(Rc<RefCell<VecDeque<Option<Completion>>>>);
impl Backend for Fixture {
    fn available(&self) -> bool {
        true
    }
    fn begin(&mut self, request: &Request) -> Result<(), String> {
        assert_eq!(request.user, 1);
        assert_eq!(request.title, vec![84]);
        assert_eq!(request.capacity_units, 16);
        Ok(())
    }
    fn poll(&mut self) -> Result<Option<Completion>, String> {
        Ok(self.0.borrow_mut().pop_front().flatten())
    }
    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}
fn invoke(
    native: &mut NativeExecutor,
    image: &mut Image,
    entry: u64,
    pointer: u64,
    data: &mut [u8],
    writable: bool,
) -> Result<i32, String> {
    match native.run_with_data(
        image,
        entry,
        Arguments {
            size: pointer,
            pointer: 0,
        },
        &mut [],
        &mut [DataRegion {
            address: 0x30000000,
            bytes: data,
            writable,
        }],
    )? {
        Execution::Returned(value) => Ok(value),
        _ => Err("IME fixture unexpectedly suspended".into()),
    }
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    let mut file = memory_fixture();
    for (offset, address) in [
        (0x100, 0x7e0000u64),
        (0x140, 0x7e0010),
        (0x180, 0x7e0020),
        (0x1c0, 0x7e0030),
    ] {
        let mut code = vec![0x48, 0xb8];
        code.extend(address.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        file[offset..offset + code.len()].copy_from_slice(&code);
    }
    // Deliberately point the result at the return address CALL will push.
    let mut alias = vec![0x48, 0x8d, 0x7c, 0x24, 0xf8, 0x48, 0xb8];
    alias.extend(0x7e0020u64.to_le_bytes());
    alias.extend([0xff, 0xd0, 0xc3]);
    file[0x1e0..0x1e0 + alias.len()].copy_from_slice(&alias);
    let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
    for (address, service) in [
        (0x7e0000, Service::ImeGetStatus),
        (0x7e0010, Service::ImeInitialize),
        (0x7e0020, Service::ImeGetResult),
        (0x7e0030, Service::ImeTerminate),
    ] {
        native.register_service(address, service)?;
    }
    // SAFETY: caller supplies the matching locally built runtime archive.
    unsafe {
        native.compile_image(
            &file,
            0x100000,
            BTreeSet::from([0x100, 0x140, 0x180, 0x1c0, 0x1e0]),
            1000,
            archive,
            directory,
        )?;
    }
    let mut image = Image::load(&file, 0x100000, 4096)?;
    let mut data = [0u8; 1024];
    data[0..4].copy_from_slice(&1i32.to_le_bytes());
    data[36..40].copy_from_slice(&16u32.to_le_bytes());
    data[40..48].copy_from_slice(&0x30000100u64.to_le_bytes());
    data[72..80].copy_from_slice(&0x30000200u64.to_le_bytes());
    data[512..514].copy_from_slice(&84u16.to_le_bytes());
    assert_eq!(
        invoke(&mut native, &mut image, 0x100100, 0, &mut data, true)?,
        0
    );
    assert!(
        invoke(
            &mut native,
            &mut image,
            0x100140,
            0x30000000,
            &mut data,
            true
        )
        .err()
        .ok_or("Missing IME backend succeeded")?
        .contains("no UI backend")
    );
    let completions = Rc::new(RefCell::new(VecDeque::new()));
    native
        .ime_dialog
        .configure_backend(Fixture(completions.clone()))?;
    for completion in [
        Completion::Accepted(vec![65, 0xd83d, 0xde00]),
        Completion::Cancelled,
        Completion::Aborted,
    ] {
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x100140,
                0x30000000,
                &mut data,
                true
            )?,
            0
        );
        assert_eq!(
            invoke(&mut native, &mut image, 0x100100, 0, &mut data, true)?,
            1
        );
        completions.borrow_mut().push_back(Some(completion.clone()));
        assert_eq!(
            invoke(&mut native, &mut image, 0x100100, 0, &mut data, true)?,
            2
        );
        let before = data;
        let error = invoke(&mut native, &mut image, 0x1001e0, 0, &mut data, true)
            .err()
            .ok_or("IME accepted an output over its return frame")?;
        assert!(error.contains("live return frame"), "{error}");
        assert_eq!(data, before);
        assert_eq!(
            invoke(&mut native, &mut image, 0x100100, 0, &mut data, true)?,
            2
        );
        assert!(
            invoke(
                &mut native,
                &mut image,
                0x100180,
                0x30000300,
                &mut data,
                false
            )
            .is_err()
        );
        assert_eq!(data, before);
        if matches!(completion, Completion::Accepted(_)) {
            assert!(
                invoke(
                    &mut native,
                    &mut image,
                    0x100180,
                    0x30000100,
                    &mut data,
                    true
                )
                .is_err()
            );
            assert_eq!(data, before);
        }
        assert_eq!(
            invoke(
                &mut native,
                &mut image,
                0x100180,
                0x30000300,
                &mut data,
                true
            )?,
            0
        );
        let result = u32::from_le_bytes(data[768..772].try_into().unwrap());
        match completion {
            Completion::Accepted(units) => {
                assert_eq!(result, 0);
                let expected: Vec<u8> = units
                    .into_iter()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect();
                assert_eq!(&data[256..256 + expected.len()], &expected);
            }
            Completion::Cancelled => {
                assert_eq!(result, 1);
                assert_eq!(&data[256..288], &before[256..288]);
            }
            Completion::Aborted => {
                assert_eq!(result, 2);
                assert_eq!(&data[256..288], &before[256..288]);
            }
        }
        assert_eq!(&data[772..784], &[0; 12]);
        assert_eq!(
            invoke(&mut native, &mut image, 0x1001c0, 0, &mut data, true)?,
            0
        );
        assert_eq!(
            invoke(&mut native, &mut image, 0x100100, 0, &mut data, true)?,
            0
        );
    }
    println!(
        "PASS: translated IME None/running/finished/terminate, UTF16 acceptance and cancellation; failed output writes preserve both ranges; synthetic backend only"
    );
    Ok(())
}
