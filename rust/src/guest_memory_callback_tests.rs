//! Independent checks of return-slot protection at the shared memory boundary.
use super::Memory;

#[test]
fn callback_key_provenance_uses_owned_backing_not_borrowed_equal_bytes() {
    let mut owner = super::OwnedMappings::default();
    let mut external = [0u8; 16];
    let address;
    {
        let mut memory = Memory::default();
        memory.add(0x1000, &mut external, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        address = memory.malloc(1).unwrap();
        assert_eq!(memory.owned_region_range(0x1000), None);
        assert_eq!(memory.owned_region_range(address), Some((address, 16)));
        assert_eq!(memory.owned_region_range(address + 15), Some((address, 16)));
        assert_eq!(memory.owned_region_range(address + 16), None);
        assert_eq!(memory.owned_region_range(address - 1), None);
        assert_eq!(memory.owned_region_range(u64::MAX), None);
    }
    // Ownership survives a Memory view being rebuilt after suspension.
    let mut memory = Memory::default();
    memory.attach_owned(&mut owner).unwrap();
    assert_eq!(memory.owned_region_range(address + 15), Some((address, 16)));
    memory.free(address).unwrap();
    assert_eq!(memory.owned_region_range(address), None);
    // Identical bytes reintroduced by a caller are no longer owned backing.
    memory.add(address, &mut external, true, false).unwrap();
    assert_eq!(memory.owned_region_range(address), None);
}

#[test]
fn nested_frame_barriers_reject_every_overlapping_write_before_mutation() {
    const BASE: u64 = 0x1_0000_1000;
    for offset in 0..=64usize {
        for size in 0..=64 - offset {
            let mut storage = [0x5a; 64];
            let mut memory = Memory::default();
            memory.add(BASE, &mut storage, true, false).unwrap();
            memory.set_write_barriers(vec![(BASE + 16, 8), (BASE + 40, 8)]);
            let overlaps = size != 0
                && [(16, 24), (40, 48)]
                    .iter()
                    .any(|&(start, end)| offset < end && start < offset + size);
            let result = memory.write(BASE + offset as u64, &vec![0xa5; size]);
            assert_eq!(result.is_err(), overlaps, "offset={offset}, size={size}");
            let mut expected = [0x5a; 64];
            if !overlaps {
                expected[offset..offset + size].fill(0xa5);
            }
            assert_eq!(memory.read(BASE, 64).unwrap(), expected);
        }
    }
}

#[test]
fn scalar_copy_and_atomic_paths_share_return_slot_protection() {
    #[repr(align(16))]
    struct Aligned([u8; 64]);
    let mut storage = Aligned([0x33; 64]);
    let mut memory = Memory::default();
    memory.add(0x1000, &mut storage.0, true, false).unwrap();
    memory.set_write_barriers(vec![(0x1010, 8), (0x1028, 8)]);
    for size in [1, 2, 4, 8] {
        assert!(memory.store(0x1010, 0, size).is_err());
    }
    assert!(memory.store(0x100f, 0, 2).is_err());
    assert!(memory.memcpy(0x100c, 0x1038, 8).is_err());
    assert!(memory.atomic_xadd_u32(0x1010, 1).is_err());
    assert_eq!(memory.read(0x1000, 64).unwrap(), [0x33; 64]);
    // Exact end adjacency remains writable; ordinary reads remain permitted.
    assert_eq!(memory.load(0x1010, 8).unwrap(), 0x3333333333333333);
    memory.store(0x1008, 0, 8).unwrap();
    memory.store(0x1018, 0, 8).unwrap();
    assert_eq!(memory.atomic_xadd_u32(0x1020, 1).unwrap(), 0x33333333);
    // Removing an inner frame must retain the ancestor; retiring both allows writes.
    memory.set_write_barriers(vec![(0x1010, 8)]);
    memory.store(0x1028, 0, 8).unwrap();
    assert!(memory.store(0x1010, 0, 8).is_err());
    memory.set_write_barriers(vec![]);
    memory.store(0x1010, 0, 8).unwrap();
    assert_eq!(memory.load(0x1010, 8).unwrap(), 0);
    assert!(memory.validate_write(u64::MAX - 2, 8).is_err());
    memory.write(u64::MAX, &[]).unwrap();
}
