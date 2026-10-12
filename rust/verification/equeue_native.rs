//! Native translated queue calls; injected events are fixtures, not display evidence.
use native_ps5::{
    equeue::Event,
    guest_memory::DataRegion,
    loader::Image,
    module_executor::{Execution, NativeExecutor, Service, memory_fixture},
    modules::Arguments,
};
use std::{collections::BTreeSet, path::Path};

pub fn verify(archive: &Path, directory: &Path) -> Result<(), String> {
    fn call(address: u64) -> Vec<u8> {
        let mut code = vec![0x48, 0xb8];
        code.extend(address.to_le_bytes());
        code.extend([0xff, 0xd0, 0xc3]);
        code
    }
    for case in 0..4 {
        let mut file = memory_fixture();
        let create = call(0x7b0030);
        file[0x100..0x100 + create.len()].copy_from_slice(&create);
        let mut wait = vec![0xbe]; // RSI: event output
        wait.extend(0x30000010u32.to_le_bytes());
        wait.push(0xba); // EDX: maximum events
        wait.extend(1u32.to_le_bytes());
        wait.push(0xb9); // RCX: count output
        wait.extend(0x30000030u32.to_le_bytes());
        wait.extend([0x45, 0x31, 0xc0]); // R8: NULL timeout
        wait.extend(call(0x7b0040));
        file[0x140..0x140 + wait.len()].copy_from_slice(&wait);
        let delete = call(0x7b0050);
        file[0x180..0x180 + delete.len()].copy_from_slice(&delete);
        let mut native = NativeExecutor::new(0x900000, 4096, 1000, 1000)?;
        for (address, service) in [
            (0x7b0030, Service::EqueueCreate),
            (0x7b0040, Service::EqueueWait),
            (0x7b0050, Service::EqueueDelete),
        ] {
            native.register_service(address, service)?;
        }
        // SAFETY: caller supplies this build's trusted local archive.
        unsafe {
            native.compile_image(
                &file,
                0x100000,
                BTreeSet::from([0x100, 0x140, 0x180]),
                1000,
                archive,
                directory,
            )?;
        }
        let mut image = Image::load(&file, 0x100000, 4096)?;
        let mut output = [0xa5u8; 64];
        let mut name = *b"display\0";
        let created = native.run_with_data(
            &mut image,
            0x100100,
            Arguments {
                size: 0x30000000,
                pointer: 0x31000000,
            },
            &mut [],
            &mut [
                DataRegion {
                    address: 0x30000000,
                    bytes: &mut output,
                    writable: case != 2,
                },
                DataRegion {
                    address: 0x31000000,
                    bytes: &mut name,
                    writable: false,
                },
            ],
        );
        if case == 2 {
            assert!(
                created
                    .err()
                    .ok_or("Readonly create unexpectedly succeeded")?
                    .contains("permission")
            );
            assert!(native.equeues.is_empty());
            assert_eq!(output, [0xa5; 64]);
            continue;
        }
        assert!(matches!(created?, Execution::Returned(0)));
        let handle = u64::from_le_bytes(output[..8].try_into().unwrap());
        assert_eq!(native.equeues.name(handle)?, b"display");
        let event = Event {
            ident: 17,
            filter: -13,
            flags: 1,
            fflags: 2,
            data: -9,
            udata: 0x1234,
        };
        if case != 1 {
            native.equeues.enqueue(handle, event)?;
        }
        let before = output;
        let waited = native.run_with_data(
            &mut image,
            0x100140,
            Arguments {
                size: handle,
                pointer: 0,
            },
            &mut [],
            &mut [DataRegion {
                address: 0x30000000,
                bytes: &mut output,
                writable: case != 3,
            }],
        );
        if case == 0 {
            assert!(matches!(waited?, Execution::Returned(0)));
            assert_eq!(&output[16..48], &event.encode());
            assert_eq!(&output[48..52], &1u32.to_le_bytes());
            assert_eq!(&output[..16], &before[..16]);
            assert_eq!(&output[52..], &before[52..]);
            assert_eq!(native.equeues.pending_count(handle)?, 0);
            assert!(matches!(
                native.run(
                    &mut image,
                    0x100180,
                    Arguments {
                        size: handle,
                        pointer: 0
                    },
                    &mut []
                )?,
                Execution::Returned(0)
            ));
            assert!(native.equeues.is_empty());
        } else {
            let error = waited.err().ok_or("Invalid wait unexpectedly succeeded")?;
            assert!(
                error.contains(if case == 1 {
                    "event suspension"
                } else {
                    "permission"
                }),
                "{error}"
            );
            assert_eq!(output, before);
            assert_eq!(
                native.equeues.pending_count(handle)?,
                usize::from(case == 3)
            );
        }
        assert_eq!(name, *b"display\0");
    }
    println!(
        "PASS: translated Equeue create/wait/delete deliver an explicit fixture event; empty waits stop and readonly failures preserve queue/output"
    );
    Ok(())
}
