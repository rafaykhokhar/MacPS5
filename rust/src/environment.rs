//! Environment lookup over a guest-owned, NULL-terminated pointer table.
//! Never reads the host process environment.
use crate::strings::{Fault, LIMIT, Memory};
use std::rc::Rc;
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
        if matches_name(memory, pointer, &name, &mut budget)? {
            return pointer
                .checked_add(name.len() as u64 + 1)
                .ok_or(Fault::Read);
        }
    }
    Err(Fault::Limit)
}
fn matches_name(
    memory: &impl Memory,
    pointer: u64,
    name: &[u8],
    budget: &mut usize,
) -> Result<bool, Fault> {
    for (offset, expected) in name
        .iter()
        .copied()
        .chain(std::iter::once(b'='))
        .enumerate()
    {
        if *budget == 0 {
            return Err(Fault::Limit);
        }
        *budget -= 1;
        let actual = memory.read(pointer.checked_add(offset as u64).ok_or(Fault::Read)?)?;
        if actual != expected {
            return Ok(false);
        }
    }
    Ok(true)
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
        m.0[110_000..110_512].fill(b'q');
        m.0[40_000..40_512].fill(b'q');
        m.0[40_512] = b'x';
        // Prefix budget must stop before this otherwise-invalid later entry.
        m.0[8 + 3000 * 8..16 + 3000 * 8].copy_from_slice(&u64::MAX.to_le_bytes());
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
        assert_eq!(getenv(&m, 8, 16), Ok(0));
    }
}

/// Owned process environment. Guest addresses remain stable when host vectors
/// move. Displaced strings stay mapped until process teardown; shorter updates
/// reuse the current value's capacity, matching FreeBSD11.4 getenv.c.
/// putenv retains caller pointers. Direct table replacement and unsetenv remain
/// unsupported. Owned storage uses the existing bounded arena rather than the
/// source allocator/inactive-slot history; displaced owned bytes remain mapped.
/// Policy: FreeBSD releng/11.4 lib/libc/stdlib/getenv.c (putenv/__findenv).
/// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdlib/getenv.c
pub struct Environment {
    entries: Vec<Variable>,
    strings: Vec<u8>,
    used: usize,
    table: Vec<u8>,
    base: u64,
    budget: usize,
    owner: Rc<()>,
    revision: u64,
}
struct Variable {
    offset: usize,
    name_len: usize,
    capacity: usize,
    borrowed: Option<u64>,
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
            owner: Rc::new(()),
            revision: 0,
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
        self.entries.iter().position(|v| {
            v.borrowed.is_none() && self.strings[v.offset..v.offset + v.name_len] == *name
        })
    }
    pub fn validate_contents(&self) -> Result<(), EnvError> {
        for (index, v) in self.entries.iter().enumerate() {
            if let Some(pointer) = v.borrowed {
                if pointer == 0 || self.table[index * 8..index * 8 + 8] != pointer.to_le_bytes() {
                    return Err(EnvError::Corrupt);
                }
                continue;
            }
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
        if self.entries.iter().any(|entry| entry.borrowed.is_some()) {
            return Err(EnvError::Corrupt);
        }
        let index = self.index(name);
        self.set_at(name, value, overwrite, index, false)
    }
    fn set_at(
        &mut self,
        name: &[u8],
        value: &[u8],
        overwrite: i32,
        index: Option<usize>,
        newest_first: bool,
    ) -> Result<(), EnvError> {
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) {
            return Err(EnvError::InvalidName);
        }
        if value.contains(&0) {
            return Err(EnvError::Corrupt);
        }
        self.validate_contents()?;
        if index.is_some() && overwrite == 0 {
            return Ok(());
        }
        let revision = self.revision.checked_add(1).ok_or(EnvError::NoMemory)?;
        if let Some(index) = index {
            let v = &self.entries[index];
            if v.borrowed.is_none() && value.len() <= v.capacity {
                let begin = v.offset + v.name_len + 1;
                self.strings[begin..begin + value.len()].copy_from_slice(value);
                self.strings[begin + value.len()] = 0;
                self.revision = revision;
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
            borrowed: None,
        };
        let slot = if newest_first {
            if let Some(i) = index {
                self.entries.remove(i);
            }
            self.entries.insert(0, variable);
            0
        } else if let Some(i) = index {
            self.entries[i] = variable;
            i
        } else {
            self.entries.push(variable);
            self.entries.len() - 1
        };
        let pointer = self.strings_address() + offset as u64;
        self.table[slot * 8..slot * 8 + 8].copy_from_slice(&pointer.to_le_bytes());
        self.rebuild_table();
        self.revision = revision;
        Ok(())
    }
    fn rebuild_table(&mut self) {
        self.table.fill(0);
        for (index, variable) in self.entries.iter().enumerate() {
            let pointer = variable
                .borrowed
                .unwrap_or_else(|| self.strings_address() + variable.offset as u64);
            self.table[index * 8..index * 8 + 8].copy_from_slice(&pointer.to_le_bytes());
        }
    }
    /// Capture before mappings borrow this owner. Preparing an action reads
    /// live caller-owned prefixes, never copied putenv values. No guest code
    /// may run between preparing and committing the action.
    pub fn snapshot(&self) -> Result<Snapshot, EnvError> {
        self.validate_contents()?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(self.entries.len())
            .map_err(|_| EnvError::NoMemory)?;
        for variable in &self.entries {
            entries.push(match variable.borrowed {
                Some(pointer) => SnapshotEntry::Borrowed(pointer),
                None => {
                    let mut name = Vec::new();
                    name.try_reserve_exact(variable.name_len)
                        .map_err(|_| EnvError::NoMemory)?;
                    name.extend_from_slice(
                        &self.strings[variable.offset..variable.offset + variable.name_len],
                    );
                    SnapshotEntry::Owned(name)
                }
            });
        }
        Ok(Snapshot {
            entries: Rc::new(entries),
            owner: self.owner.clone(),
            revision: self.revision,
        })
    }
    pub fn commit(&mut self, action: Action) -> Result<(), EnvError> {
        if !Rc::ptr_eq(&self.owner, &action.owner) || self.revision != action.revision {
            return Err(EnvError::Corrupt);
        }
        self.validate_contents()?;
        for (entry, expected) in self.entries.iter().zip(action.expected.iter()) {
            if let SnapshotEntry::Owned(name) = expected {
                if entry.borrowed.is_some()
                    || self.strings[entry.offset..entry.offset + entry.name_len] != **name
                {
                    return Err(EnvError::Corrupt);
                }
            }
        }
        match action.operation {
            Operation::Set {
                name,
                value,
                overwrite,
                index,
            } => self.set_at(&name, &value, overwrite, index, true),
            Operation::Put { pointer, index } => {
                let revision = self.revision.checked_add(1).ok_or(EnvError::NoMemory)?;
                if index.is_none() {
                    if self.entries.len() == MAX_ENTRIES as usize {
                        return Err(EnvError::NoMemory);
                    }
                    self.entries
                        .try_reserve(1)
                        .map_err(|_| EnvError::NoMemory)?;
                }
                let variable = Variable {
                    offset: 0,
                    name_len: 0,
                    capacity: 0,
                    borrowed: Some(pointer),
                };
                if let Some(index) = index.filter(|&index| self.entries[index].borrowed.is_some()) {
                    self.entries[index] = variable;
                } else {
                    if let Some(index) = index {
                        self.entries.remove(index);
                    }
                    self.entries.insert(0, variable);
                }
                self.rebuild_table();
                self.revision = revision;
                Ok(())
            }
        }
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

enum SnapshotEntry {
    Owned(Vec<u8>),
    Borrowed(u64),
}
#[derive(Clone)]
pub struct Snapshot {
    entries: Rc<Vec<SnapshotEntry>>,
    owner: Rc<()>,
    revision: u64,
}
pub struct Action {
    expected: Rc<Vec<SnapshotEntry>>,
    owner: Rc<()>,
    revision: u64,
    operation: Operation,
}
enum Operation {
    Set {
        name: Vec<u8>,
        value: Vec<u8>,
        overwrite: i32,
        index: Option<usize>,
    },
    Put {
        pointer: u64,
        index: Option<usize>,
    },
}
impl Snapshot {
    fn index(&self, memory: &impl Memory, name: &[u8]) -> Result<Option<usize>, Fault> {
        let mut budget = MAX_BYTES;
        for (index, entry) in self.entries.iter().enumerate() {
            let matched = match entry {
                SnapshotEntry::Owned(owned) => owned == name,
                SnapshotEntry::Borrowed(pointer) => {
                    matches_name(memory, *pointer, name, &mut budget)?
                }
            };
            if matched {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
    fn action(&self, operation: Operation) -> Action {
        Action {
            expected: self.entries.clone(),
            owner: self.owner.clone(),
            revision: self.revision,
            operation,
        }
    }
    pub fn prepare_put(
        &self,
        memory: &impl Memory,
        pointer: u64,
    ) -> Result<Result<Action, EnvError>, Fault> {
        let name = match putenv_inputs(memory, pointer)? {
            Ok(name) => name,
            Err(error) => return Ok(Err(error)),
        };
        let index = self.index(memory, &name)?;
        Ok(Ok(self.action(Operation::Put { pointer, index })))
    }
    pub fn prepare_set(
        &self,
        memory: &impl Memory,
        name: &[u8],
        value: &[u8],
        overwrite: i32,
    ) -> Result<Result<Action, EnvError>, Fault> {
        if name.is_empty() || name.contains(&b'=') || name.contains(&0) {
            return Ok(Err(EnvError::InvalidName));
        }
        if value.contains(&0) {
            return Ok(Err(EnvError::Corrupt));
        }
        let index = self.index(memory, name)?;
        let mut owned_name = Vec::new();
        let mut owned_value = Vec::new();
        owned_name
            .try_reserve_exact(name.len())
            .map_err(|_| Fault::Allocation)?;
        owned_value
            .try_reserve_exact(value.len())
            .map_err(|_| Fault::Allocation)?;
        owned_name.extend_from_slice(name);
        owned_value.extend_from_slice(value);
        Ok(Ok(self.action(Operation::Set {
            name: owned_name,
            value: owned_value,
            overwrite,
            index,
        })))
    }
}
/// FreeBSD putenv checks only through the first '='. The caller retains the
/// string; neither installation nor name matching reads its value terminator.
pub fn putenv_inputs(
    memory: &impl Memory,
    pointer: u64,
) -> Result<Result<Vec<u8>, EnvError>, Fault> {
    if pointer == 0 {
        return Ok(Err(EnvError::InvalidName));
    }
    let mut name = Vec::new();
    for offset in 0..LIMIT {
        let byte = memory.read(pointer.checked_add(offset as u64).ok_or(Fault::Read)?)?;
        if byte == b'=' {
            return Ok(if name.is_empty() {
                Err(EnvError::InvalidName)
            } else {
                Ok(name)
            });
        }
        if byte == 0 {
            return Ok(Err(EnvError::InvalidName));
        }
        name.try_reserve(1).map_err(|_| Fault::Allocation)?;
        name.push(byte);
    }
    Err(Fault::Limit)
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

#[cfg(test)]
mod putenv_tests {
    use super::*;
    struct Mem(Vec<u8>);
    impl Memory for Mem {
        fn read(&self, address: u64) -> Result<u8, Fault> {
            self.0.get(address as usize).copied().ok_or(Fault::Read)
        }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), Fault> {
            panic!("No caller writes")
        }
    }
    fn put(env: &mut Environment, memory: &Mem, pointer: u64) {
        let action = env
            .snapshot()
            .unwrap()
            .prepare_put(memory, pointer)
            .unwrap()
            .unwrap();
        env.commit(action).unwrap();
    }
    fn pointers(env: &Environment) -> Vec<u64> {
        env.table[..env.entries.len() * 8]
            .chunks_exact(8)
            .map(|slot| u64::from_le_bytes(slot.try_into().unwrap()))
            .collect()
    }
    #[test]
    fn input_and_lookup_stop_before_unmapped_value() {
        let m = Mem(vec![0, b'A', b'=']);
        assert_eq!(putenv_inputs(&m, 1), Ok(Ok(b"A".to_vec())));
        let mut env = Environment::new(0x10000, 0).unwrap();
        put(&mut env, &m, 1);
        assert_eq!(pointers(&env), [1]);
        let action = env
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"A", b"ignored", 0)
            .unwrap()
            .unwrap();
        env.commit(action).unwrap();
        assert_eq!(pointers(&env), [1]);
        let mut lookup = Mem(vec![0; 35]);
        lookup.0[8..16].copy_from_slice(&33u64.to_le_bytes());
        lookup.0[24..26].copy_from_slice(b"A\0");
        lookup.0[33..35].copy_from_slice(b"A=");
        assert_eq!(getenv(&lookup, 8, 24), Ok(35));
        for bytes in [b"\0".as_slice(), b"=x\0", b"ABC\0"] {
            let mut data = vec![0];
            data.extend_from_slice(bytes);
            assert_eq!(putenv_inputs(&Mem(data), 1), Ok(Err(EnvError::InvalidName)));
        }
        assert_eq!(putenv_inputs(&m, 0), Ok(Err(EnvError::InvalidName)));
    }
    #[test]
    fn borrowed_renames_missing_equals_and_setenv_copy_are_live() {
        let mut env = Environment::new(0x10000, 100).unwrap();
        let mut m = Mem(vec![0; 64]);
        m.0[8..14].copy_from_slice(b"A=one\0");
        put(&mut env, &m, 8);
        m.0[8..14].copy_from_slice(b"B=two\0");
        env.validate_contents().unwrap();
        let action = env
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"A", b"own", 1)
            .unwrap()
            .unwrap();
        env.commit(action).unwrap();
        assert_eq!(env.entries.len(), 2);
        assert!(pointers(&env).contains(&8));
        let saved_caller = m.0.clone();
        let action = env
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"B", b"x", 1)
            .unwrap()
            .unwrap();
        env.commit(action).unwrap();
        assert!(!pointers(&env).contains(&8));
        assert_eq!(m.0, saved_caller);
        assert!(env.lookup(b"B").unwrap() >= env.strings_address());
        m.0[24..30].copy_from_slice(b"C=raw\0");
        put(&mut env, &m, 24);
        m.0[25] = 0; // Current entry has no '='; it no longer matches C.
        env.validate_contents().unwrap();
        let action = env
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"C", b"new", 1)
            .unwrap()
            .unwrap();
        env.commit(action).unwrap();
        assert!(pointers(&env).contains(&24));
        assert_eq!(env.entries.len(), 4);
    }
    #[test]
    fn replacement_order_and_renamed_collisions_preserve_borrowed_slot() {
        let mut env = Environment::new(0x10000, 100).unwrap();
        env.set(b"X", b"owned", 1).unwrap();
        let mut m = Mem(vec![0; 80]);
        for (address, bytes) in [
            (8, b"A=1\0"),
            (24, b"B=2\0"),
            (40, b"A=3\0"),
            (56, b"X=4\0"),
        ] {
            m.0[address..address + 4].copy_from_slice(bytes);
        }
        put(&mut env, &m, 8);
        put(&mut env, &m, 24);
        put(&mut env, &m, 40);
        assert_eq!(&pointers(&env)[..2], &[24, 40]); // Replacement retains A slot.
        m.0[24] = b'A'; // Highest-priority live B entry now also names A.
        put(&mut env, &m, 8);
        assert_eq!(&pointers(&env)[..2], &[8, 40]);
        put(&mut env, &m, 56); // Replacing owned X creates newest slot.
        assert_eq!(pointers(&env), [56, 8, 40]);
    }
    #[test]
    fn failures_and_stale_or_foreign_actions_preserve_environment() {
        let mut env = Environment::new(0x10000, 0).unwrap();
        let mut m = Mem(vec![0; 40]);
        m.0[8..12].copy_from_slice(b"A=x\0");
        put(&mut env, &m, 8);
        let before = env.table.clone();
        let action = env
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"A", b"x", 1)
            .unwrap()
            .unwrap();
        assert_eq!(env.commit(action), Err(EnvError::NoMemory));
        assert_eq!(env.table, before);
        let snapshot = env.snapshot().unwrap();
        let stale = snapshot.prepare_put(&m, 8).unwrap().unwrap();
        put(&mut env, &m, 8);
        assert_eq!(env.commit(stale), Err(EnvError::Corrupt));
        let foreign = env.snapshot().unwrap().prepare_put(&m, 8).unwrap().unwrap();
        let mut other = Environment::new(0x10000, 0).unwrap();
        assert_eq!(other.commit(foreign), Err(EnvError::Corrupt));
        env.table[0] ^= 1;
        assert!(matches!(env.snapshot(), Err(EnvError::Corrupt)));
        let mut owned = Environment::new(0x20000, 20).unwrap();
        owned.set(b"A", b"old", 1).unwrap();
        let action = owned
            .snapshot()
            .unwrap()
            .prepare_set(&m, b"A", b"new", 1)
            .unwrap()
            .unwrap();
        owned.strings[0] = b'B';
        assert_eq!(owned.commit(action), Err(EnvError::Corrupt));
        assert_eq!(&owned.strings[..6], b"B=old\0");
    }
}
