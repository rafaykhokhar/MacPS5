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

/// Owned process environment. Guest addresses remain stable when host vectors
/// move. Displaced strings stay mapped until process teardown; shorter updates
/// reuse the current value's capacity, matching FreeBSD11.4 getenv.c.
/// putenv/unsetenv and direct replacement of environ are not supported here.
pub struct Environment {
    entries: Vec<Variable>,
    strings: Vec<u8>,
    used: usize,
    table: Vec<u8>,
    base: u64,
    budget: usize,
}
struct Variable {
    offset: usize,
    name_len: usize,
    capacity: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvError {
    InvalidName,
    NoMemory,
    Corrupt,
}
impl EnvError {
    pub fn errno(self) -> i32 {
        match self {
            Self::InvalidName => 22,
            Self::NoMemory => 12,
            Self::Corrupt => 14,
        }
    }
}
const TABLE_BYTES: usize = (MAX_ENTRIES as usize + 1) * 8;
impl Environment {
    /// `base` reserves TABLE_BYTES followed by at most `budget` bytes. The
    /// embedding executor must reject overlap with all other guest mappings.
    pub fn new(base: u64, budget: usize) -> Result<Self, EnvError> {
        if base == 0
            || budget > MAX_BYTES
            || base
                .checked_add(TABLE_BYTES as u64 + budget as u64)
                .is_none()
        {
            return Err(EnvError::NoMemory);
        }
        let mut table = Vec::new();
        table
            .try_reserve_exact(TABLE_BYTES)
            .map_err(|_| EnvError::NoMemory)?;
        table.resize(TABLE_BYTES, 0);
        let mut strings = Vec::new();
        strings
            .try_reserve_exact(budget)
            .map_err(|_| EnvError::NoMemory)?;
        strings.resize(budget, 0);
        Ok(Self {
            entries: Vec::new(),
            strings,
            used: 0,
            table,
            base,
            budget,
        })
    }
    pub fn table_address(&self) -> u64 {
        self.base
    }
    pub fn strings_address(&self) -> u64 {
        self.base + TABLE_BYTES as u64
    }
    pub fn allocated_bytes(&self) -> usize {
        self.table.len() + self.strings.len()
    }
    pub fn mappings(&mut self) -> [(u64, &mut [u8]); 2] {
        [
            (self.base, self.table.as_mut_slice()),
            (self.base + TABLE_BYTES as u64, self.strings.as_mut_slice()),
        ]
    }
    fn index(&self, name: &[u8]) -> Option<usize> {
        self.entries
            .iter()
            .position(|v| self.strings[v.offset..v.offset + v.name_len] == *name)
    }
    pub fn validate_contents(&self) -> Result<(), EnvError> {
        for (index, v) in self.entries.iter().enumerate() {
            let name = &self.strings[v.offset..v.offset + v.name_len];
            let begin = v.offset + v.name_len + 1;
            let expected = self.strings_address() + v.offset as u64;
            if name.contains(&0)
                || name.contains(&b'=')
                || self.strings[begin - 1] != b'='
                || !self.strings[begin..=begin + v.capacity].contains(&0)
                || self.table[index * 8..index * 8 + 8] != expected.to_le_bytes()
            {
                return Err(EnvError::Corrupt);
            }
        }
        if self.table[self.entries.len() * 8..].iter().any(|b| *b != 0) {
            return Err(EnvError::Corrupt);
        }
        Ok(())
    }
    pub fn lookup(&self, name: &[u8]) -> Option<u64> {
        self.index(name).map(|i| {
            self.strings_address() + (self.entries[i].offset + self.entries[i].name_len + 1) as u64
        })
    }
    /// Input slices exclude their terminating NUL. Copying inputs before this
    /// call supports values which alias existing environment storage.
    pub fn set(&mut self, name: &[u8], value: &[u8], overwrite: i32) -> Result<(), EnvError> {
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) {
            return Err(EnvError::InvalidName);
        }
        if value.contains(&0) {
            return Err(EnvError::Corrupt);
        }
        self.validate_contents()?;
        let index = self.index(name);
        if index.is_some() && overwrite == 0 {
            return Ok(());
        }
        if let Some(index) = index {
            let v = &self.entries[index];
            if value.len() <= v.capacity {
                let begin = v.offset + v.name_len + 1;
                self.strings[begin..begin + value.len()].copy_from_slice(value);
                self.strings[begin + value.len()] = 0;
                return Ok(());
            }
        } else if self.entries.len() == MAX_ENTRIES as usize {
            return Err(EnvError::NoMemory);
        }
        let size = name
            .len()
            .checked_add(value.len())
            .and_then(|n| n.checked_add(2))
            .ok_or(EnvError::NoMemory)?;
        if size > self.budget.saturating_sub(self.used) {
            return Err(EnvError::NoMemory);
        }
        if index.is_none() {
            self.entries
                .try_reserve(1)
                .map_err(|_| EnvError::NoMemory)?;
        }
        // All fallible work completed: publish bytes, metadata and table atomically.
        let offset = self.used;
        self.strings[offset..offset + name.len()].copy_from_slice(name);
        self.strings[offset + name.len()] = b'=';
        let begin = offset + name.len() + 1;
        self.strings[begin..begin + value.len()].copy_from_slice(value);
        self.strings[begin + value.len()] = 0;
        self.used += size;
        let variable = Variable {
            offset,
            name_len: name.len(),
            capacity: value.len(),
        };
        let slot = if let Some(i) = index {
            self.entries[i] = variable;
            i
        } else {
            self.entries.push(variable);
            self.entries.len() - 1
        };
        let pointer = self.strings_address() + offset as u64;
        self.table[slot * 8..slot * 8 + 8].copy_from_slice(&pointer.to_le_bytes());
        Ok(())
    }
    /// Copy the original guest table once, preserving its first duplicate.
    /// Caller publishes the new owner only after this constructor succeeds.
    pub fn import(
        memory: &impl Memory,
        table: u64,
        base: u64,
        budget: usize,
    ) -> Result<Self, Fault> {
        let mut owned = Self::new(base, budget).map_err(|_| Fault::Allocation)?;
        if table == 0 {
            return Ok(owned);
        }
        let mut scan = MAX_BYTES;
        for index in 0..MAX_ENTRIES {
            let mut pointer = [0; 8];
            memory.read_into(
                table.checked_add(index * 8).ok_or(Fault::Read)?,
                &mut pointer,
            )?;
            let pointer = u64::from_le_bytes(pointer);
            if pointer == 0 {
                return Ok(owned);
            }
            let bytes = read_string(memory, pointer, &mut scan)?;
            let split = bytes.iter().position(|b| *b == b'=').ok_or(Fault::Read)?;
            owned
                .set(&bytes[..split], &bytes[split + 1..], 0)
                .map_err(|e| match e {
                    EnvError::NoMemory => Fault::Allocation,
                    _ => Fault::Read,
                })?;
        }
        Err(Fault::Limit)
    }
}

/// Read name first, then value even for overwrite=0. Malformed names produce
/// the guest EINVAL result without reading value, as FreeBSD setenv does.
/// Guard faults remain explicit; dispatcher translates EnvError into -1/errno.
pub type SetenvInputs = Result<(Vec<u8>, Vec<u8>), EnvError>;
pub fn setenv_inputs(memory: &impl Memory, name: u64, value: u64) -> Result<SetenvInputs, Fault> {
    if name == 0 {
        return Ok(Err(EnvError::InvalidName));
    }
    let mut budget = MAX_BYTES;
    let name = read_string(memory, name, &mut budget)?;
    if name.is_empty() || name.contains(&b'=') {
        return Ok(Err(EnvError::InvalidName));
    }
    let value = read_string(memory, value, &mut budget)?;
    Ok(Ok((name, value)))
}

#[cfg(test)]
mod owned_tests {
    use super::*;
    #[test]
    fn replacement_reuses_capacity_and_retains_displaced_pointer() {
        let mut env = Environment::new(0x10000, 100).unwrap();
        env.set(b"A", b"first", 1).unwrap();
        let first = env.lookup(b"A").unwrap();
        env.set(b"A", b"xy", 1).unwrap();
        assert_eq!(env.lookup(b"A"), Some(first));
        env.set(b"A", b"longer value", -1).unwrap();
        assert_ne!(env.lookup(b"A"), Some(first));
        let offset = (first - env.strings_address()) as usize;
        assert_eq!(&env.strings[offset..offset + 3], b"xy\0");
        let old = env.lookup(b"A");
        env.set(b"A", b"ignored", 0).unwrap();
        assert_eq!(env.lookup(b"A"), old);
        env.set(b"\xff", b"\xfe=", 1).unwrap();
        assert!(env.lookup(b"\xff").is_some());
        let table_pointer = u64::from_le_bytes(env.table[..8].try_into().unwrap());
        assert_eq!(table_pointer + 2, env.lookup(b"A").unwrap());
        assert_eq!(&env.table[16..24], &[0; 8]);
    }
    #[test]
    fn bounded_failure_preserves_all_live_data() {
        let mut env = Environment::new(0x10000, 6).unwrap();
        env.set(b"A", b"123", 1).unwrap();
        let pointer = env.lookup(b"A");
        let bytes = env.strings.clone();
        let table = env.table.clone();
        assert_eq!(env.set(b"A", b"1234", 1), Err(EnvError::NoMemory));
        assert_eq!(env.set(b"B", b"", 1), Err(EnvError::NoMemory));
        assert_eq!(env.lookup(b"A"), pointer);
        assert_eq!(env.strings, bytes);
        assert_eq!(env.table, table);
        assert_eq!(env.set(b"bad=name", b"x", 1), Err(EnvError::InvalidName));
        assert_eq!(EnvError::NoMemory.errno(), 12);
        env.set(b"A", b"", 1).unwrap();
        assert_eq!(env.lookup(b"A"), pointer);
    }
    struct Bytes(Vec<u8>);
    impl Memory for Bytes {
        fn read(&self, p: u64) -> Result<u8, Fault> {
            self.0.get(p as usize).copied().ok_or(Fault::Read)
        }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Fault> {
            panic!("read only")
        }
    }
    #[test]
    fn import_duplicates_inputs_and_transactional_failure() {
        let mut memory = Bytes(vec![0; 100]);
        memory.0[8..16].copy_from_slice(&40u64.to_le_bytes());
        memory.0[16..24].copy_from_slice(&50u64.to_le_bytes());
        memory.0[40..44].copy_from_slice(b"A=1\0");
        memory.0[50..54].copy_from_slice(b"A=2\0");
        let env = Environment::import(&memory, 8, 0x10000, 100).unwrap();
        assert_eq!(env.entries.len(), 1);
        assert_eq!(&env.strings[..env.used], b"A=1\0");
        assert_eq!(
            setenv_inputs(&memory, 0, u64::MAX),
            Ok(Err(EnvError::InvalidName))
        );
        assert_eq!(
            setenv_inputs(&memory, 40, u64::MAX),
            Ok(Err(EnvError::InvalidName))
        );
        memory.0[60..62].copy_from_slice(b"A\0");
        assert_eq!(setenv_inputs(&memory, 60, u64::MAX), Err(Fault::Read));
        assert!(matches!(
            Environment::import(&memory, 8, 0x10000, 2),
            Err(Fault::Allocation)
        ));
        memory.0[50..54].copy_from_slice(b"bad\0");
        assert!(matches!(
            Environment::import(&memory, 8, 0x10000, 100),
            Err(Fault::Read)
        ));
    }
}

#[cfg(test)]
mod mutable_environment_tests {
    use super::*;
    #[test]
    fn guest_value_writes_observed_and_corrupt_metadata_rejected() {
        let mut env = Environment::new(0x10000, 100).unwrap();
        env.set(b"A", b"hello", 1).unwrap();
        let value = env.lookup(b"A").unwrap();
        {
            let [_, (_, strings)] = env.mappings();
            strings[2..8].copy_from_slice(b"world\0");
        }
        env.validate_contents().unwrap();
        env.set(b"A", b"ignored", 0).unwrap();
        assert_eq!(&env.strings[2..8], b"world\0");
        assert_eq!(env.lookup(b"A"), Some(value));
        env.strings[1] = b'!';
        assert_eq!(env.validate_contents(), Err(EnvError::Corrupt));
        assert_eq!(env.set(b"A", b"next", 1), Err(EnvError::Corrupt));
        env.strings[1] = b'=';
        env.table[0] ^= 1;
        assert_eq!(env.validate_contents(), Err(EnvError::Corrupt));
    }
}
