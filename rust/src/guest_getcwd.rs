//! libc getcwd over the explicitly registered virtual working directory.
//! ABI: pinned SDK 10b539de8f2b6773a9a78caa7576a97dbd99b29a/freebsd/unistd.h.
//! Policy sources: FreeBSD releng/11.4 lib/libc/gen/getcwd.c and getcwd.3:
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/getcwd.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/getcwd.3
//! NULL ignores size and allocates PATH_MAX. Caller buffers use full-width
//! capacity. This bounded virtual policy does not implement the filesystem
//! traversal fallback, its intermediate writes, permissions, deleted-directory
//! behavior or its kernel unsigned32 capacity truncation. Guard faults roll
//! back, while scalar failures return NULL and the specified guest errno.
use crate::{Result, guest_memory::Memory, kernel::Kernel};

pub const PATH_MAX: u64 = 1024;

pub fn call(
    memory: &mut Memory<'_>,
    kernel: &Kernel,
    destination: u64,
    size: u64,
    guard: impl Fn(&Memory<'_>, u64, usize) -> Result<()>,
) -> Result<u64> {
    let fail = |memory: &mut Memory<'_>, errno: u32| -> Result<u64> {
        let address = kernel.errno_address();
        guard(memory, address, 4)?;
        memory.validate_write(address, 4)?;
        memory.store(address, errno as u64, 4)?;
        Ok(0)
    };
    // These libc checks precede either the syscall or cwd lookup.
    if destination != 0 {
        if size == 0 {
            return fail(memory, 22);
        }
        if size == 1 {
            return fail(memory, 34);
        }
    }
    let path = kernel
        .working_directory_bytes()
        .ok_or("Guest working directory is not configured for getcwd")?;
    if destination != 0 && size < path.len() as u64 {
        return fail(memory, 34);
    }
    let allocated = destination == 0;
    let address = if allocated {
        match memory.malloc(PATH_MAX) {
            Ok(address) => address,
            Err(errno) => return fail(memory, errno),
        }
    } else {
        destination
    };
    // Successful calls never access errno. The output may legitimately alias
    // errno storage; those bytes then contain the path, not an errno rewrite.
    let result = (|| {
        guard(memory, address, path.len())?;
        memory.validate_write(address, path.len())?;
        memory.write(address, path)?;
        Ok(address)
    })();
    if result.is_err() && allocated {
        // Exact-base owned allocation created above, never published to guest.
        // free is infallible for this identity and does not alter guest errno.
        memory.free(address)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;

    #[test]
    fn scalar_errors_precede_lookup_and_preserve_destination() {
        let mut kernel = Kernel::new(0, 0x2000).unwrap();
        let mut errno = [0; 4];
        let mut output = [0xa5; 16];
        let mut memory = Memory::default();
        memory.add(0x2000, &mut errno, true, false).unwrap();
        memory.add(0x1_0000_1000, &mut output, true, false).unwrap();
        for (size, error) in [(0, 22), (1, 34)] {
            assert_eq!(
                call(&mut memory, &kernel, u64::MAX, size, |_, _, _| Ok(())).unwrap(),
                0
            );
            assert_eq!(memory.load(0x2000, 4).unwrap(), error);
        }
        assert!(call(&mut memory, &kernel, 0, 0, |_, _, _| Ok(())).is_err());
        kernel.register_working_directory(b"/app0").unwrap();
        assert_eq!(
            call(&mut memory, &kernel, 0x1_0000_1000, 5, |_, _, _| Ok(())).unwrap(),
            0
        );
        assert_eq!(memory.load(0x2000, 4).unwrap(), 34);
        assert_eq!(memory.read(0x1_0000_1000, 16).unwrap(), &[0xa5; 16]);
        for size in [6, 1024, 0x1_0000_0000, u64::MAX] {
            memory.store(0x2000, 77, 4).unwrap();
            assert_eq!(
                call(&mut memory, &kernel, 0x1_0000_1000, size, |_, _, _| Ok(())).unwrap(),
                0x1_0000_1000
            );
            assert_eq!(memory.read(0x1_0000_1000, 6).unwrap(), b"/app0\0");
            assert_eq!(memory.read(0x1_0000_1006, 10).unwrap(), &[0xa5; 10]);
            assert_eq!(memory.load(0x2000, 4).unwrap(), 77);
        }
    }

    #[test]
    fn null_ignores_size_owns_freeable_path_and_never_accesses_errno_on_success() {
        for map_errno in [false, true] {
            let mut kernel = Kernel::new(0, 0x2000).unwrap();
            kernel.register_working_directory(b"/").unwrap();
            let mut mappings = OwnedMappings::default();
            let mut readonly_errno = 91u32.to_le_bytes();
            let mut memory = Memory::default();
            memory.attach_owned(&mut mappings).unwrap();
            if map_errno {
                memory
                    .add(0x2000, &mut readonly_errno, false, false)
                    .unwrap();
            }
            for size in [0, 1, 2, u64::MAX] {
                let address = call(&mut memory, &kernel, 0, size, |_, address, _| {
                    assert_ne!(address, 0x2000);
                    Ok(())
                })
                .unwrap();
                assert!(address > u32::MAX as u64);
                assert_eq!(memory.heap_allocation_size(address).unwrap(), 1024);
                assert_eq!(memory.read(address, 2).unwrap(), b"/\0");
                memory.free(address).unwrap();
                assert!(memory.read(address, 1).is_err());
            }
            drop(memory);
            assert_eq!(mappings.bytes_allocated(), 0);
            assert_eq!(readonly_errno, 91u32.to_le_bytes());
        }
    }

    #[test]
    fn allocation_failure_errno_and_guard_failure_rollback() {
        let mut kernel = Kernel::new(0, 0x2000).unwrap();
        kernel.register_working_directory(b"/app0").unwrap();
        let mut errno = 77u32.to_le_bytes();
        let mut memory = Memory::default();
        memory.add(0x2000, &mut errno, true, false).unwrap();
        // No owned allocator: malloc fails without publishing a region.
        assert!(
            call(&mut memory, &kernel, 0, 0, |_, _, _| Err(
                "protected errno".into()
            ))
            .is_err()
        );
        assert_eq!(memory.load(0x2000, 4).unwrap(), 77);
        assert_eq!(
            call(&mut memory, &kernel, 0, 0, |_, _, _| Ok(())).unwrap(),
            0
        );
        assert_eq!(memory.load(0x2000, 4).unwrap(), 12);
        drop(memory);
        let mut mappings = OwnedMappings::default();
        let mut memory = Memory::default();
        memory.attach_owned(&mut mappings).unwrap();
        assert!(
            call(&mut memory, &kernel, 0, 0, |_, _, _| Err(
                "protected frame".into()
            ))
            .is_err()
        );
        assert!(memory.read(0x4_0000_0000, 1).is_err());
        drop(memory);
        assert_eq!(mappings.bytes_allocated(), 0);
    }

    #[test]
    fn caller_guard_faults_and_errno_alias_success() {
        let mut kernel = Kernel::new(0, 0x2000).unwrap();
        kernel.register_working_directory(b"/app0").unwrap();
        let mut buffer = [0xa5; 16];
        let mut readonly = [0xa5; 16];
        let mut memory = Memory::default();
        memory.add(0x2000, &mut buffer, true, false).unwrap();
        memory.add(0x3000, &mut readonly, false, false).unwrap();
        assert!(
            call(&mut memory, &kernel, 0x2000, 16, |_, _, _| Err(
                "protected frame".into()
            ))
            .is_err()
        );
        for destination in [0x3000, u64::MAX - 2] {
            assert!(call(&mut memory, &kernel, destination, 16, |_, _, _| Ok(())).is_err());
        }
        assert_eq!(memory.read(0x2000, 16).unwrap(), &[0xa5; 16]);
        assert_eq!(
            call(&mut memory, &kernel, 0x2000, 16, |_, _, _| Ok(())).unwrap(),
            0x2000
        );
        assert_eq!(memory.read(0x2000, 6).unwrap(), b"/app0\0");
    }
}
