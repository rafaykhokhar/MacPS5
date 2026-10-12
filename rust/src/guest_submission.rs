//! Guarded snapshot of the AGC `Packet` descriptor and its PM4 command words.
//! Preparation performs no GPU work and does not report submission completion.
use crate::{Result, guest_memory::Memory, pm4};

pub struct Submission {
    pub command_address: u64,
    pub words: Vec<u32>,
    pub decoded: pm4::Submission,
}

/// Read the 16-byte guest descriptor: address:u64, DWORD count:u32,
/// flags:u8, reserved:[u8;3]. Reserved bytes have no interpreted semantics.
/// Both descriptor and nonempty command buffer must fit guarded readable regions.
pub fn prepare(memory: &Memory<'_>, descriptor: u64, initial: &pm4::State) -> Result<Submission> {
    if descriptor == 0 || descriptor & 7 != 0 || descriptor.checked_add(16).is_none() {
        return Err("Invalid guest DCB descriptor address".into());
    }
    let header = memory.read(descriptor, 16)?;
    let command_address = u64::from_le_bytes(header[..8].try_into().unwrap());
    let count = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if header[12] != 0 {
        return Err("Nonzero DCB submission flags unsupported".into());
    }
    if count > 65536 {
        return Err("Guest DCB exceeds 256 KiB limit".into());
    }
    let mut words = Vec::new();
    if count != 0 {
        if command_address == 0
            || command_address & 3 != 0
            || command_address.checked_add(count as u64 * 4).is_none()
        {
            return Err("Invalid guest command buffer address".into());
        }
        let bytes = memory.read(command_address, count * 4)?;
        words
            .try_reserve_exact(count)
            .map_err(|_| "Unable to snapshot guest command buffer")?;
        words.extend(
            bytes
                .chunks_exact(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap())),
        );
    }
    let decoded = pm4::decode(&words, initial)?;
    Ok(Submission {
        command_address,
        words,
        decoded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(address: u64, count: u32, flags: u8) -> [u8; 16] {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&address.to_le_bytes());
        bytes[8..12].copy_from_slice(&count.to_le_bytes());
        bytes[12] = flags;
        bytes
    }
    #[test]
    fn snapshots_real_descriptor_and_preserves_initial_state() {
        let mut header = descriptor(0x2000, 2, 0);
        let mut words = [0xc0002f00_u32.to_le_bytes(), 3_u32.to_le_bytes()].concat();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut header, false, false).unwrap();
        memory.add(0x2000, &mut words, true, false).unwrap();
        let initial = pm4::State::default();
        let snapshot = prepare(&memory, 0x1000, &initial).unwrap();
        memory.store(0x2004, 7, 4).unwrap();
        assert_eq!(snapshot.command_address, 0x2000);
        assert_eq!(snapshot.words, [0xc0002f00, 3]);
        assert_eq!(snapshot.decoded.final_state.instances, Some(3));
        assert_eq!(initial, pm4::State::default());
    }
    #[test]
    fn rejects_bad_pointers_sizes_flags_and_packets() {
        for (address, count, flags) in [
            (0, 1, 0),
            (0x2001, 1, 0),
            (u64::MAX - 3, 2, 0),
            (0x2000, 65537, 0),
            (0x2000, 1, 1),
            (0x2000, 2, 0),
            (0x3000, 1, 0),
        ] {
            let mut header = descriptor(address, count, flags);
            let mut command = 0x80000000_u32.to_le_bytes();
            let mut memory = Memory::default();
            memory.add(0x1000, &mut header, false, false).unwrap();
            memory.add(0x2000, &mut command, false, false).unwrap();
            assert!(prepare(&memory, 0x1000, &pm4::State::default()).is_err());
        }
        let mut header = descriptor(0, 0, 0);
        let mut memory = Memory::default();
        memory.add(0x1000, &mut header, false, false).unwrap();
        assert!(
            prepare(&memory, 0x1000, &pm4::State::default())
                .unwrap()
                .words
                .is_empty()
        );
        for address in [0, 0x1001, 0x1008, 0x2000, u64::MAX - 7] {
            assert!(prepare(&memory, address, &pm4::State::default()).is_err());
        }
        let mut header = descriptor(0x4000, 1, 0);
        let mut command = 0xffff_ffff_u32.to_le_bytes();
        let mut memory = Memory::default();
        memory.add(0x3000, &mut header, false, false).unwrap();
        memory.add(0x4000, &mut command, false, false).unwrap();
        assert!(prepare(&memory, 0x3000, &pm4::State::default()).is_err());
    }
}
