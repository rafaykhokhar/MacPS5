//! Connect typed printf argument fetching to the runtime's guarded mappings.
impl crate::printf_args::Memory for crate::guest_memory::Memory<'_> {
    type Error = String;

    fn read(&self, address: u64, out: &mut [u8]) -> Result<(), String> {
        out.copy_from_slice(crate::guest_memory::Memory::read(self, address, out.len())?);
        Ok(())
    }

    fn validate_write(&self, address: u64, length: usize) -> Result<(), String> {
        crate::guest_memory::Memory::validate_write(self, address, length)
    }

    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), String> {
        crate::guest_memory::Memory::write(self, address, bytes)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        guest_memory::Memory,
        printf_args::{Class, Cursor, Error, Value},
    };

    #[test]
    fn guarded_mapping_updates_only_consumed_cursor_field() {
        let mut descriptor = [0u8; 24];
        descriptor[4..8].copy_from_slice(&48u32.to_le_bytes());
        descriptor[16..24].copy_from_slice(&0x2000u64.to_le_bytes());
        let mut registers = [0u8; 64];
        registers[..8].copy_from_slice(&0x1234u64.to_le_bytes());
        registers[48..56].copy_from_slice(&0x4004_0000_0000_0000u64.to_le_bytes());
        let mut memory = Memory::default();
        memory.add(0x1000, &mut descriptor, true, false).unwrap();
        memory.add(0x2000, &mut registers, false, false).unwrap();
        let mut cursor = Cursor::new(0x1000, 3);
        assert_eq!(
            cursor
                .fetch(&mut memory, Class::Gp, |_, _, _| Ok(()))
                .unwrap(),
            Value::Gp(0x1234)
        );
        assert_eq!(
            cursor
                .fetch(&mut memory, Class::Binary64, |_, _, _| Ok(()))
                .unwrap(),
            Value::Binary64(0x4004_0000_0000_0000)
        );
        let before = memory.read(0x1000, 24).unwrap().to_vec();
        assert_eq!(&before[..8], &[8, 0, 0, 0, 64, 0, 0, 0]);
        assert!(matches!(
            cursor.fetch(&mut memory, Class::Gp, |_, _, _| Err("live frame".into())),
            Err(Error::Guard(_))
        ));
        assert_eq!(cursor.remaining(), 1);
        assert_eq!(memory.read(0x1000, 24).unwrap(), before);
    }

    #[test]
    fn unmapped_argument_preserves_writable_descriptor_and_budget() {
        let mut descriptor = [0u8; 24];
        descriptor[16..24].copy_from_slice(&0x3000u64.to_le_bytes());
        let before = descriptor;
        let mut memory = Memory::default();
        memory.add(0x1000, &mut descriptor, true, false).unwrap();
        let mut cursor = Cursor::new(0x1000, 1);
        assert!(matches!(
            cursor.fetch(&mut memory, Class::Gp, |_, _, _| Ok(())),
            Err(Error::Read(_))
        ));
        assert_eq!(memory.read(0x1000, 24).unwrap(), before);
        assert_eq!(cursor.remaining(), 1);
    }
}
