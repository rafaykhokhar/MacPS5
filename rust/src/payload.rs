//! Byte layouts observed in the selected homebrew, not a general PS5 kernel ABI.
//! The fixture's nonzero attribute bit is synthetic process state; it does not
//! grant host privileges or claim to implement guest authorization policy.
pub const CREDENTIAL_SIZE: usize = 0x8b;
pub const PRISON_OFFSET: usize = 0x30;
pub const CAPS_OFFSET: usize = 0x60;
pub const ATTRIBUTES_OFFSET: usize = 0x83;
pub const MODULE_CONTAINER_SIZE: usize = 0x100;
pub const FIXTURE_ATTRIBUTES: u64 = 0x100;
/// Process header fields actually read by the selected payload. Pointers refer
/// to separately owned kernel records, never native host allocations.
pub struct ProcessRecord {
    pub bytes: [u8; 0x400],
}
impl ProcessRecord {
    pub fn new(pid: u32, next: u64, credentials: u64, modules: u64) -> crate::Result<Self> {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err("Invalid guest PID".into());
        }
        let mut record = Self { bytes: [0; 0x400] };
        for (offset, value) in [(0, next), (0x40, credentials), (0x3e8, modules)] {
            record.bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        record.bytes[0xbc..0xc0].copy_from_slice(&pid.to_le_bytes());
        Ok(record)
    }
}
/// Owned user-side startup storage for the observed homebrew ABI. Descriptor
/// IDs must be supplied by the process's descriptor table; this creates none.
/// The complete record is mapped at `address` and contains only guest pointers.
pub struct BootstrapPayload {
    pub address: u64,
    pub bytes: [u8; 88],
}
impl BootstrapPayload {
    pub fn new(
        address: u64,
        resolver: u64,
        descriptors: [u32; 4],
        pipe_control: u64,
        kernel_base: u64,
    ) -> crate::Result<Self> {
        if address == 0 || address.checked_add(88).is_none() || resolver == 0 {
            return Err("Invalid payload mapping or resolver address".into());
        }
        let mut record = Self {
            address,
            bytes: [0; 88],
        };
        // 64-byte argument record, 16 descriptor bytes, 8-byte return storage.
        for (index, value) in [
            resolver,
            address + 64,
            address + 72,
            pipe_control,
            kernel_base,
            address + 80,
        ]
        .into_iter()
        .enumerate()
        {
            record.bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            record.bytes[64 + index * 4..68 + index * 4].copy_from_slice(&descriptor.to_le_bytes());
        }
        Ok(record)
    }
}
#[repr(C)]
pub struct CredentialRecord {
    pub bytes: [u8; CREDENTIAL_SIZE],
}
impl CredentialRecord {
    pub fn new(prison: u64, attributes: u64) -> Self {
        let mut record = Self {
            bytes: [0; CREDENTIAL_SIZE],
        };
        record.bytes[PRISON_OFFSET..PRISON_OFFSET + 8].copy_from_slice(&prison.to_le_bytes());
        record.bytes[ATTRIBUTES_OFFSET..ATTRIBUTES_OFFSET + 8]
            .copy_from_slice(&attributes.to_le_bytes());
        record
    }
}
#[repr(C)]
pub struct ModuleContainer {
    pub bytes: [u8; MODULE_CONTAINER_SIZE],
}
impl Default for ModuleContainer {
    fn default() -> Self {
        Self {
            bytes: [0; MODULE_CONTAINER_SIZE],
        }
    }
}

// SAFETY: unique integer/byte-array C ABI exports; no pointers are dereferenced.
// Callers own the returned records and must keep mapped storage stationary.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn native_ps5_fixture_credentials_v1(prison: u64) -> CredentialRecord {
    CredentialRecord::new(prison, FIXTURE_ATTRIBUTES)
}
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn native_ps5_fixture_module_container_v1() -> ModuleContainer {
    ModuleContainer::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn process_record_fields_and_padding() {
        let record = ProcessRecord::new(1234, 0x1000, 0x2000, 0x3000).unwrap();
        assert_eq!(&record.bytes[..8], &0x1000u64.to_le_bytes());
        assert_eq!(&record.bytes[0x40..0x48], &0x2000u64.to_le_bytes());
        assert_eq!(&record.bytes[0xbc..0xc0], &1234u32.to_le_bytes());
        assert_eq!(&record.bytes[0x3e8..0x3f0], &0x3000u64.to_le_bytes());
        for (offset, byte) in record.bytes.iter().enumerate() {
            if ![0..8, 0x40..0x48, 0xbc..0xc0, 0x3e8..0x3f0]
                .iter()
                .any(|r| r.contains(&offset))
            {
                assert_eq!(*byte, 0);
            }
        }
        assert!(ProcessRecord::new(0, 0, 0, 0).is_err());
        assert!(ProcessRecord::new(u32::MAX, 0, 0, 0).is_err());
    }
    #[test]
    fn bootstrap_layout_uses_guest_addresses_and_keeps_result_separate() {
        let mut payload = BootstrapPayload::new(
            0x30000000,
            0x700020,
            [5, 6, 3, 4],
            0xffff800006100000,
            0xffff800000000000,
        )
        .unwrap();
        let mut memory = crate::guest_memory::Memory::default();
        memory
            .add(payload.address, &mut payload.bytes, true, false)
            .unwrap();
        assert_eq!(memory.load(0x30000000, 8).unwrap(), 0x700020);
        assert_eq!(memory.load(0x30000008, 8).unwrap(), 0x30000040);
        assert_eq!(memory.load(0x30000010, 8).unwrap(), 0x30000048);
        assert_eq!(memory.load(0x30000018, 8).unwrap(), 0xffff800006100000);
        assert_eq!(memory.load(0x30000020, 8).unwrap(), 0xffff800000000000);
        assert_eq!(memory.load(0x30000028, 8).unwrap(), 0x30000050);
        assert_eq!(memory.read(0x30000030, 16).unwrap(), &[0; 16]);
        for (i, expected) in [5, 6, 3, 4].into_iter().enumerate() {
            assert_eq!(memory.load(0x30000040 + i as u64 * 4, 4).unwrap(), expected);
        }
        memory.store(0x30000050, u64::MAX, 8).unwrap();
        assert_eq!(memory.load(0x3000004c, 4).unwrap(), 4);
        assert!(BootstrapPayload::new(u64::MAX - 87, 1, [0; 4], 0, 0).is_err());
        assert!(BootstrapPayload::new(0, 1, [0; 4], 0, 0).is_err());
        assert!(BootstrapPayload::new(4096, 0, [0; 4], 0, 0).is_err());
    }
    #[test]
    fn exact_observed_layout_and_nonidentity_prison() {
        let r = CredentialRecord::new(0xffff800006200000, 0x123456789abcdef0);
        assert_eq!(std::mem::size_of::<CredentialRecord>(), 0x8b);
        assert_eq!(&r.bytes[0x30..0x38], &0xffff800006200000u64.to_le_bytes());
        assert_eq!(&r.bytes[0x60..0x70], &[0u8; 16]);
        assert_eq!(&r.bytes[0x83..], &0x123456789abcdef0u64.to_le_bytes());
        for (i, &byte) in r.bytes.iter().enumerate() {
            if !(0x30..0x38).contains(&i) && !(0x83..0x8b).contains(&i) {
                assert_eq!(byte, 0);
            }
        }
    }
    #[test]
    fn guest_attribute_and_capability_updates_preserve_neighbors() {
        let mut r = CredentialRecord::new(0xffff800006200000, FIXTURE_ATTRIBUTES);
        let before = r.bytes;
        // Exact writes made by the selected payload's __patch_init.
        r.bytes[CAPS_OFFSET + 5] = 0x1c;
        r.bytes[CAPS_OFFSET + 7] = 0x40;
        r.bytes[CAPS_OFFSET + 15] |= 0x40;
        r.bytes[ATTRIBUTES_OFFSET..].copy_from_slice(&(FIXTURE_ATTRIBUTES | 0x80).to_le_bytes());
        for (i, &b) in r.bytes.iter().enumerate() {
            if ![0x65, 0x67, 0x6f, 0x83].contains(&i) {
                assert_eq!(b, before[i]);
            }
        }
        assert_eq!(
            u64::from_le_bytes(r.bytes[0x83..].try_into().unwrap()),
            0x180
        );
    }
    #[test]
    fn module_head_and_patch_fields_are_distinct() {
        let mut r = ModuleContainer::default();
        assert_eq!(std::mem::size_of::<ModuleContainer>(), 256);
        r.bytes[..8].copy_from_slice(&0xffff800005100000u64.to_le_bytes());
        r.bytes[0xf0..0xf8].copy_from_slice(&0u64.to_le_bytes());
        r.bytes[0xf8..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            u64::from_le_bytes(r.bytes[..8].try_into().unwrap()),
            0xffff800005100000
        );
        assert!(r.bytes[8..0xf8].iter().all(|&b| b == 0));
        assert!(r.bytes[0xf8..].iter().all(|&b| b == 255));
    }
}
