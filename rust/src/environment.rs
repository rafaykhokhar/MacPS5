//! Environment lookup over a guest-owned, NULL-terminated pointer table.
//! Never reads the host process environment.
use crate::strings::{Fault, LIMIT, Memory};
const MAX_ENTRIES: u64 = 4096;
const MAX_BYTES: usize = 1024 * 1024;
fn read_string(memory: &impl Memory, address: u64, budget: &mut usize) -> Result<Vec<u8>, Fault> {
    let mut bytes = Vec::new();
    for offset in 0..LIMIT {
        if *budget == 0 {
            return Err(Fault::Limit);
        }
        *budget -= 1;
        let b = memory.read(address.checked_add(offset as u64).ok_or(Fault::Read)?)?;
        if b == 0 {
            return Ok(bytes);
        }
        bytes.try_reserve(1).map_err(|_| Fault::Allocation)?;
        bytes.push(b);
    }
    Err(Fault::Limit)
}
pub fn getenv(memory: &impl Memory, environment: u64, name: u64) -> Result<u64, Fault> {
    let mut budget = MAX_BYTES;
    let name = read_string(memory, name, &mut budget)?;
    if name.is_empty() || name.contains(&b'=') || environment == 0 {
        return Ok(0);
    }
    for index in 0..MAX_ENTRIES {
        let address = environment.checked_add(index * 8).ok_or(Fault::Read)?;
        let mut pointer = [0u8; 8];
        memory.read_into(address, &mut pointer)?;
        let pointer = u64::from_le_bytes(pointer);
        if pointer == 0 {
            return Ok(0);
        }
        let entry = read_string(memory, pointer, &mut budget)?;
        if entry.get(..name.len()) == Some(name.as_slice()) && entry.get(name.len()) == Some(&b'=')
        {
            return pointer
                .checked_add(name.len() as u64 + 1)
                .ok_or(Fault::Read);
        }
    }
    Err(Fault::Limit)
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Mem(Vec<u8>);
    impl Memory for Mem {
        fn read(&self, address: u64) -> Result<u8, Fault> {
            self.0.get(address as usize).copied().ok_or(Fault::Read)
        }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Fault> {
            panic!("getenv must not write")
        }
    }
    #[test]
    fn exact_names_empty_values_duplicates_and_raw_bytes() {
        let mut m = Mem(vec![0; 256]);
        for (i, address) in [64u64, 80, 96, 112, 128].iter().enumerate() {
            m.0[8 + i * 8..16 + i * 8].copy_from_slice(&address.to_le_bytes());
        }
        m.0[64..71].copy_from_slice(b"AB=bad\0");
        m.0[80..84].copy_from_slice(b"A=\0\0");
        m.0[96..103].copy_from_slice(b"A=later");
        m.0[112..117].copy_from_slice(b"B=\xff\xfe\0");
        m.0[128..138].copy_from_slice(b"malformed\0");
        m.0[200..202].copy_from_slice(b"A\0");
        assert_eq!(getenv(&m, 8, 200), Ok(82));
        m.0[200] = b'B';
        assert_eq!(getenv(&m, 8, 200), Ok(114));
        m.0[200] = b'C';
        assert_eq!(getenv(&m, 8, 200), Ok(0));
        m.0[200] = b'=';
        assert_eq!(getenv(&m, u64::MAX, 200), Ok(0));
        m.0[200] = 0;
        assert_eq!(getenv(&m, u64::MAX, 200), Ok(0));
    }
    #[test]
    fn bounds_entry_count_and_total_scan_work() {
        let mut m = Mem(vec![0; 120_000]);
        for index in 0..MAX_ENTRIES as usize {
            m.0[8 + index * 8..16 + index * 8].copy_from_slice(&40_000u64.to_le_bytes());
        }
        m.0[110_000] = b'A';
        m.0[40_000..40_004].copy_from_slice(b"B=1\0");
        assert_eq!(getenv(&m, 8, 110_000), Err(Fault::Limit));
        // Individually valid entries must still obey the aggregate scan budget.
        m.0[40_000..40_000 + LIMIT - 1].fill(b'x');
        assert_eq!(getenv(&m, 8, 110_000), Err(Fault::Limit));
    }
    #[test]
    fn guards_input_table_entries_and_scan_budget() {
        let mut m = Mem(vec![0; LIMIT + 128]);
        m.0[16] = b'A';
        assert_eq!(getenv(&m, 0, 16), Ok(0));
        assert_eq!(getenv(&m, 8, u64::MAX), Err(Fault::Read));
        assert_eq!(getenv(&m, u64::MAX, 16), Err(Fault::Read));
        m.0[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(getenv(&m, 8, 16), Err(Fault::Read));
        m.0[8..16].copy_from_slice(&64u64.to_le_bytes());
        m.0[64..64 + LIMIT].fill(b'x');
        assert_eq!(getenv(&m, 8, 16), Err(Fault::Limit));
    }
}
