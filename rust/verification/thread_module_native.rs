//! Retained initializer/start lifecycle while genuine child threads run and join.
use native_ps5::{
    module_executor::{NativeExecutor, Service},
    modules::{Arguments, EntryPoints, Modules, Phase, StartEvent, lifecycle_fixture},
};
use std::{collections::BTreeSet, path::Path};
fn call(code: &mut Vec<u8>, address: u64) {
    code.extend([0x48, 0xb8]);
    code.extend(address.to_le_bytes());
    code.extend([0xff, 0xd0]);
}
pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    for (status, exiting) in [(0u32, false), (u32::MAX, false), (0, true)] {
        let mut file = lifecycle_fixture(false);
        file.resize(0x600, 0);
        file[56..58].copy_from_slice(&3u16.to_le_bytes());
        for (i, value) in [0x600000001u64, 0x500, 0x500, 0, 0x100, 0x100, 1]
            .into_iter()
            .enumerate()
        {
            file[176 + i * 8..184 + i * 8].copy_from_slice(&value.to_le_bytes());
        }
        file[0x188..0x190].copy_from_slice(&0x300u64.to_le_bytes());
        let mut init = vec![0x48, 0x83, 0xec, 32, 0x48, 0xb8];
        init.extend(0xfedcba9876543210u64.to_le_bytes());
        init.extend([
            0x48, 0x89, 0x44, 0x24, 16, 0x48, 0x89, 0xe7, 0x31, 0xf6, 0x48, 0xba,
        ]);
        init.extend(0x100370u64.to_le_bytes());
        init.extend([0x48, 0x8d, 0x4c, 0x24, 16]);
        call(&mut init, 0x780000);
        init.extend([0x48, 0x8b, 0x3c, 0x24, 0x48, 0x8d, 0x74, 0x24, 8]);
        call(&mut init, 0x780010);
        init.extend([0x48, 0xb9]);
        init.extend(0x100500u64.to_le_bytes());
        init.extend([
            0x48, 0xff, 0x01, 0x48, 0x8b, 0x44, 0x24, 8, 0x48, 0x83, 0xc4, 32, 0xc3,
        ]);
        assert!(init.len() <= 0x70);
        file[0x300..0x300 + init.len()].copy_from_slice(&init);
        file[0x370..0x37d].copy_from_slice(&[
            0xb9, 20, 0, 0, 0, 0xff, 0xc9, 0x75, 0xfc, 0x48, 0x8b, 0x07, 0xc3,
        ]);
        if exiting {
            let mut child = vec![0xbf];
            child.extend((-7i32).to_le_bytes());
            call(&mut child, 0x780020);
            child.extend([0x0f, 0x0b]);
            file[0x370..0x370 + child.len()].copy_from_slice(&child);
        }
        let mut start = Vec::new();
        call(&mut start, 0x100300);
        start.push(0xb8);
        start.extend(status.to_le_bytes());
        start.push(0xc3);
        file[0x100..0x100 + start.len()].copy_from_slice(&start);
        let output = directory.join(format!("status-{status}-exit-{exiting}"));
        std::fs::create_dir_all(&output).map_err(|e| e.to_string())?;
        let mut native = NativeExecutor::new(0x10000000, 2 * 1024 * 1024, 10000, 10000)?;
        native.register_service(0x780000, Service::ThreadCreate)?;
        native.register_service(0x780010, Service::ThreadJoin)?;
        native.register_service(0x780020, Service::Exit)?;
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100, 0x300]),
                1000,
                archive,
                &output,
            )?;
            native.set_automatic_translation(true);
        }
        let mut modules = Modules::new(65536);
        let handle = modules.load_dynamic(
            "thread-init.prx",
            &file,
            0x100000,
            EntryPoints {
                start: Some(0x100),
                stop: None,
            },
        )?;
        assert!(matches!(
            modules.start_native(handle, &mut native, Arguments::default())?,
            StartEvent::ThreadRequest { thread: 1 }
        ));
        assert_eq!(modules.phase(handle), Some(Phase::Faulted));
        assert!(modules.resume_native(handle, &mut native, 0).is_err());
        native.tls.register_thread(2)?;
        native.select_guest_thread(2)?;
        assert!(
            modules
                .resume_native_threads(handle, &mut native, (2, 1000, 10000), 1)
                .is_err()
        );
        native.select_guest_thread(1)?;
        native.tls.retire_thread(2)?;
        let mut completed = false;
        let mut steps = 0;
        for _ in 0..1000 {
            steps += 1;
            match modules.resume_native_threads(handle, &mut native, (2, 1000, 10000), 1) {
                Ok(StartEvent::ThreadRequest { thread: 1 }) => {
                    assert_eq!(modules.phase(handle), Some(Phase::Faulted));
                    assert!(modules.resume_native(handle, &mut native, 0).is_err());
                }
                Ok(StartEvent::Started) => {
                    assert!(!exiting);
                    assert_eq!(status, 0);
                    completed = true;
                    break;
                }
                Err(error) => {
                    assert_eq!(status, u32::MAX, "Unexpected bridge error: {error}");
                    assert!(error.contains("status -1"), "{error}");
                    completed = true;
                    break;
                }
                Ok(StartEvent::ExitRequested(request)) => {
                    assert!(exiting);
                    assert_eq!(request.status, -7);
                    assert_eq!(request.kind, native_ps5::module_executor::ExitKind::Normal);
                    assert_eq!(modules.exit_request(), Some(request));
                    assert!(modules.resume_native(handle, &mut native, 0).is_err());
                    completed = true;
                    break;
                }
                _ => return Err("Unexpected module scheduling boundary".into()),
            }
        }
        assert!(completed);
        if !exiting {
            assert!(steps > 20);
            assert_eq!(native.guest_thread_id(), 1);
        }
        assert!(native.tls.contains_thread(1));
        assert_eq!(native.tls.live_threads(), if exiting { 2 } else { 1 });
        assert_eq!(
            modules.phase(handle),
            Some(if status == 0 && !exiting {
                Phase::Started
            } else {
                Phase::Faulted
            })
        );
        let image = modules.image(handle).unwrap();
        let segment = image
            .segments
            .iter()
            .find(|s| s.address <= 0x100500 && 0x100500 < s.address + s.bytes.len() as u64)
            .unwrap();
        let offset = (0x100500 - segment.address) as usize;
        assert_eq!(
            u64::from_le_bytes(segment.bytes[offset..offset + 8].try_into().unwrap()),
            if exiting { 0 } else { 2 }
        );
    }
    println!(
        "Module thread bridge verified: void constructor then status-bearing start each create/join actual workers, repeated retained boundaries, wrong identity/generic resume rejection, exactly-once lifecycle, parent TLS preserved and signed failure status"
    );
    Ok(())
}
