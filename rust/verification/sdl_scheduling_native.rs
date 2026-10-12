//! Pinned ELF SDL_SYS_SetThreadPriority and real SDL hint helpers, empty environment.
use native_ps5::{
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};
pub fn verify(file: &[u8], archive: &Path, directory: &Path) -> Result<(), String> {
    const BIAS: u64 = 0x10000000;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut native = NativeExecutor::new(0x20000000, 65536, 1000000, 1000000)?;
    let mut image = Image::load(file, BIAS, 64 * 1024 * 1024)?;
    for (got, target, service) in [
        (0x26f768, 0x780000u64, Service::ThreadSelf),
        (0x26f798, 0x780010, Service::ThreadGetschedparam),
        (0x26f7a0, 0x780020, Service::SchedPriorityMax),
        (0x26f7a8, 0x780030, Service::SchedPriorityMin),
        (0x26f7b0, 0x780040, Service::ThreadSetschedparam),
        (0x26f7d0, 0x780050, Service::Getenv),
    ] {
        native.register_service(target, service)?;
        let address = BIAS + got;
        let segment = image
            .segments
            .iter_mut()
            .find(|s| address >= s.address && address + 8 <= s.address + s.bytes.len() as u64)
            .ok_or("Missing real SDL GOT")?;
        let offset = (address - segment.address) as usize;
        segment.bytes[offset..offset + 8].copy_from_slice(&target.to_le_bytes());
    }
    unsafe {
        native.compile_image(
            file,
            BIAS,
            BTreeSet::from([0x190d0]),
            100000,
            archive,
            directory,
        )?;
        native.set_automatic_translation(true);
    }
    assert_eq!(native.environment, 0);
    for (level, priority) in [(0, 0), (1, 51), (2, 76), (3, 103)] {
        match native.run_with_data(
            &mut image,
            BIAS + 0x190d0,
            Arguments {
                size: level,
                pointer: 0,
            },
            &mut [],
            &mut [],
        )? {
            Execution::Returned(0) => {}
            _ => return Err("Actual SDL priority did not return success".into()),
        }
        assert_eq!(native.tls.scheduling(1)?, (2, priority));
    }
    println!(
        "PASS: actual pinned SDL_SYS_SetThreadPriority0x190d0 and real hint helpers execute low/normal/high/timecritical -> OTHER0/51/76/103 using empty guest environment; no realtime policy claim."
    );
    Ok(())
}
