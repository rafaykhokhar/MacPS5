//! Owned subset used by pinned PS5 SDL: type3 allocations and prot0x33 mappings.
//! No physical host addresses are exposed. Aliases, subrange mappings and fixed
//! addresses are explicit unsupported cases. Reservations share a 256 MiB cap.
use crate::{Result, guest_memory::Memory};

struct Allocation {
    physical: u64,
    size: u64,
    alignment: u64,
    virtual_address: Option<u64>,
    pins: u32,
}
pub struct DirectMemory {
    allocations: Vec<Allocation>,
    next: u64,
}
impl Default for DirectMemory {
    fn default() -> Self {
        Self {
            allocations: Vec::new(),
            next: 0x10000000,
        }
    }
}
impl DirectMemory {
    pub fn reserved_bytes(&self) -> u64 {
        self.allocations.iter().map(|a| a.size).sum()
    }
    pub fn allocate(
        &mut self,
        memory: &mut Memory<'_>,
        size: u64,
        alignment: u64,
        kind: i32,
        output: u64,
    ) -> Result<i32> {
        if kind != 3
            || size == 0
            || size > 64 * 1024 * 1024
            || size % 16384 != 0
            || !alignment.is_power_of_two()
            || !(16384..=0x20000).contains(&alignment)
        {
            return Err("Unsupported direct-memory allocation parameters".into());
        }
        if output == 0 {
            return Err("Null direct-memory allocation output".into());
        }
        memory.validate_write(output, 8)?;
        if self.allocations.len() >= 16 || size > 256 * 1024 * 1024 - self.reserved_bytes() {
            return Err("Direct-memory reservation budget exhausted".into());
        }
        let physical = self
            .next
            .checked_add(alignment - 1)
            .ok_or("Direct-memory offset overflow")?
            & !(alignment - 1);
        let next = physical
            .checked_add(size)
            .filter(|&n| n <= i64::MAX as u64)
            .ok_or("Direct-memory offset overflow")?;
        memory.store(output, physical, 8)?;
        self.allocations.push(Allocation {
            physical,
            size,
            alignment,
            virtual_address: None,
            pins: 0,
        });
        self.next = next;
        Ok(0)
    }
    // Mirrors the six-argument guest map ABI, plus the guarded memory owner.
    #[allow(clippy::too_many_arguments)]
    pub fn map(
        &mut self,
        memory: &mut Memory<'_>,
        output: u64,
        size: u64,
        protection: i32,
        flags: i32,
        physical: u64,
        alignment: u64,
    ) -> Result<i32> {
        if protection != 0x33 || flags != 0 {
            return Err("Unsupported direct-memory map protection/flags".into());
        }
        if output == 0 {
            return Err("Null direct-memory mapping output".into());
        }
        memory.validate_write(output, 8)?;
        if memory.load(output, 8)? != 0 {
            return Err("Unsupported fixed direct-memory map".into());
        }
        let allocation = self
            .allocations
            .iter_mut()
            .find(|a| a.physical == physical)
            .ok_or("Unknown direct-memory reservation")?;
        if allocation.size != size
            || allocation.alignment != alignment
            || allocation.virtual_address.is_some()
        {
            return Err("Unsupported direct-memory subrange/alias/alignment".into());
        }
        let address = memory.map_direct_owned(size, alignment)?;
        // Output was validated before any backing allocation; allocator excludes
        // all existing mappings, so it cannot invalidate the output pointer.
        if let Err(error) = memory.store(output, address, 8) {
            memory.unmap_direct_owned(address, size)?;
            return Err(error);
        }
        allocation.virtual_address = Some(address);
        Ok(0)
    }
    pub fn release(&mut self, memory: &mut Memory<'_>, physical: u64, size: u64) -> Result<i32> {
        let index = self
            .allocations
            .iter()
            .position(|a| a.physical == physical && a.size == size)
            .ok_or("Unknown direct-memory release range")?;
        let allocation = &self.allocations[index];
        if allocation.pins != 0 {
            return Err("Direct memory is pinned by a consumer".into());
        }
        if let Some(address) = allocation.virtual_address {
            memory.unmap_direct_owned(address, size)?;
        }
        self.allocations.remove(index);
        Ok(0)
    }
    /// Pin an existing virtual range for a registered display buffer. Returns
    /// the opaque reservation identity to pass to unpin after consumer teardown.
    pub fn pin(&mut self, address: u64, size: u64) -> Result<u64> {
        let end = address
            .checked_add(size)
            .filter(|_| size != 0)
            .ok_or("Invalid direct-memory pin range")?;
        let allocation = self
            .allocations
            .iter_mut()
            .find(|a| {
                a.virtual_address
                    .is_some_and(|base| address >= base && end <= base + a.size)
            })
            .ok_or("Display range is not owned direct memory")?;
        allocation.pins = allocation
            .pins
            .checked_add(1)
            .ok_or("Direct-memory pin count overflow")?;
        Ok(allocation.physical)
    }
    pub fn unpin(&mut self, physical: u64) -> Result<()> {
        let allocation = self
            .allocations
            .iter_mut()
            .find(|a| a.physical == physical)
            .ok_or("Unknown direct-memory pin")?;
        allocation.pins = allocation
            .pins
            .checked_sub(1)
            .ok_or("Direct-memory pin underflow")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    #[test]
    fn backing_survives_suspension_and_pins_prevent_release() {
        let mut direct = DirectMemory::default();
        let mut owner = OwnedMappings::default();
        let mut outputs = [0u8; 16];
        let physical;
        let address;
        {
            let mut memory = Memory::default();
            memory.add(0x1000, &mut outputs, true, false).unwrap();
            memory.attach_owned(&mut owner).unwrap();
            direct
                .allocate(&mut memory, 64 * 1024 * 1024, 0x20000, 3, 0x1000)
                .unwrap();
            physical = memory.load(0x1000, 8).unwrap();
            direct
                .map(
                    &mut memory,
                    0x1008,
                    64 * 1024 * 1024,
                    0x33,
                    0,
                    physical,
                    0x20000,
                )
                .unwrap();
            address = memory.load(0x1008, 8).unwrap();
            assert_eq!(address % 0x20000, 0);
            memory
                .store(address + 64 * 1024 * 1024 - 8, 0x12345678, 8)
                .unwrap();
            assert!(memory.free(address).is_err());
            assert!(memory.unmap_anonymous(address, 64 * 1024 * 1024).is_err());
        }
        assert_eq!(owner.bytes_allocated(), 64 * 1024 * 1024);
        let mut memory = Memory::default();
        memory.attach_owned(&mut owner).unwrap();
        assert_eq!(
            memory.load(address + 64 * 1024 * 1024 - 8, 8).unwrap(),
            0x12345678
        );
        assert_eq!(
            direct
                .pin(address + 32 * 1024 * 1024, 32 * 1024 * 1024)
                .unwrap(),
            physical
        );
        assert!(
            direct
                .release(&mut memory, physical, 64 * 1024 * 1024)
                .is_err()
        );
        direct.unpin(physical).unwrap();
        direct
            .release(&mut memory, physical, 64 * 1024 * 1024)
            .unwrap();
        assert!(memory.read(address, 1).is_err());
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0);
        assert_eq!(direct.reserved_bytes(), 0);
    }
    #[test]
    fn failures_preserve_reservations_outputs_and_backing() {
        let mut null_storage = [0u8; 8];
        let mut direct = DirectMemory::default();
        let mut owner = OwnedMappings::default();
        let mut outputs = [0u8; 16];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut outputs, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        memory.add(0, &mut null_storage, true, false).unwrap();
        assert!(direct.allocate(&mut memory, 16384, 16384, 3, 0).is_err());
        assert!(
            direct
                .allocate(&mut memory, 16384, 16384, 3, 0x1009)
                .is_err()
        );
        assert_eq!(direct.reserved_bytes(), 0);
        direct
            .allocate(&mut memory, 16384, 16384, 3, 0x1000)
            .unwrap();
        let physical = memory.load(0x1000, 8).unwrap();
        assert!(
            direct
                .map(&mut memory, 0, 16384, 0x33, 0, physical, 16384)
                .is_err()
        );
        assert!(
            direct
                .map(&mut memory, 0x1009, 16384, 0x33, 0, physical, 16384)
                .is_err()
        );
        assert!(
            direct
                .map(&mut memory, 0x1008, 32768, 0x33, 0, physical, 16384)
                .is_err()
        );
        assert_eq!(memory.load(0x1008, 8).unwrap(), 0);
        direct
            .map(&mut memory, 0x1008, 16384, 0x33, 0, physical, 16384)
            .unwrap();
        let address = memory.load(0x1008, 8).unwrap();
        assert!(direct.pin(address + 16383, 2).is_err());
        assert!(direct.unpin(physical).is_err());
        assert!(direct.release(&mut memory, physical, 32768).is_err());
        assert!(memory.read(address, 16384).is_ok());
        direct.release(&mut memory, physical, 16384).unwrap();
        assert!(direct.release(&mut memory, physical, 16384).is_err());
    }
}
