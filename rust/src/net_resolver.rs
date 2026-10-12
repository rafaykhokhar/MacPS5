//! Owned subset observed in SDK libc/netdb.c and pinned resolve_aton21bb40 /
//! resolve_ntoa21bcf0: pool16KiB, flags0, resolver timing arguments allzero.
//! https://github.com/ps5-payload-dev/sdk/blob/master/libc/netdb.c
//! Resource/deadline bounds are explicit HLE policies, not console error ABI.
//! Host failures are diagnostic Err, never invented SceNet error integers.
use crate::{Result, guest_memory::Memory};
use std::collections::BTreeMap;
pub trait Backend {
    fn forward(&mut self, name: &str) -> Result<[u8; 4]>;
    fn reverse(&mut self, address: [u8; 4]) -> Result<Vec<u8>>;
}
struct Pool {
    _name: Vec<u8>,
    bytes: Vec<u8>,
}
pub struct Resolver<B: Backend> {
    backend: B,
    pools: BTreeMap<i32, Pool>,
    resolvers: BTreeMap<i32, i32>,
    next: i32,
}
impl<B: Backend> Resolver<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            pools: BTreeMap::new(),
            resolvers: BTreeMap::new(),
            next: 0,
        }
    }
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }
    pub fn resolver_count(&self) -> usize {
        self.resolvers.len()
    }
    pub fn reserved_bytes(&self) -> usize {
        self.pools.values().map(|p| p.bytes.len()).sum()
    }
    fn handle(&mut self) -> Result<i32> {
        self.next = self.next.checked_add(1).ok_or("Net handle exhaustion")?;
        Ok(self.next)
    }
    pub fn pool_create(
        &mut self,
        memory: &Memory<'_>,
        name: u64,
        size: i32,
        flags: i32,
    ) -> Result<i32> {
        if flags != 0 || size <= 0 || size > 1024 * 1024 {
            return Err("Unsupported network pool configuration".into());
        }
        let name = cstring(memory, name, 255)?;
        if self.pools.len() >= 16 || self.reserved_bytes() + size as usize > 4 * 1024 * 1024 {
            return Err("Network pool budget exhausted".into());
        }
        let bytes = vec![0; size as usize];
        let id = self.handle()?;
        self.pools.insert(id, Pool { _name: name, bytes });
        Ok(id)
    }
    pub fn pool_destroy(&mut self, id: i32) -> Result<i32> {
        if self.resolvers.values().any(|&p| p == id) {
            return Err("Network pool still owns resolvers".into());
        }
        self.pools.remove(&id).ok_or("Unknown network pool")?;
        Ok(0)
    }
    pub fn resolver_create(
        &mut self,
        memory: &Memory<'_>,
        name: u64,
        pool: i32,
        flags: i32,
    ) -> Result<i32> {
        cstring(memory, name, 255)?;
        if flags != 0 || !self.pools.contains_key(&pool) {
            return Err("Unsupported resolver configuration/pool".into());
        }
        if self.resolvers.len() >= 32 {
            return Err("Resolver budget exhausted".into());
        }
        let id = self.handle()?;
        self.resolvers.insert(id, pool);
        Ok(id)
    }
    pub fn resolver_destroy(&mut self, id: i32) -> Result<i32> {
        self.resolvers.remove(&id).ok_or("Unknown resolver")?;
        Ok(0)
    }
    fn validate(&self, id: i32, timeout: i32, retries: i32, flags: i32) -> Result<()> {
        if !self.resolvers.contains_key(&id) {
            return Err("Unknown resolver".into());
        }
        if timeout != 0 || retries != 0 || flags != 0 {
            return Err("Only observed zero resolver timing/flags supported; host deadline is configured separately".into());
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn start_ntoa(
        &mut self,
        memory: &mut Memory<'_>,
        id: i32,
        name: u64,
        output: u64,
        timeout: i32,
        retries: i32,
        flags: i32,
    ) -> Result<i32> {
        self.validate(id, timeout, retries, flags)?;
        if output == 0 {
            return Err("Null IPv4 output".into());
        }
        memory.validate_write(output, 4)?;
        let name = cstring(memory, name, 253)?;
        let name = std::str::from_utf8(&name).map_err(|_| "Resolver hostname is not UTF8")?;
        if name.is_empty() {
            return Err("Empty resolver hostname".into());
        }
        let address = self.backend.forward(name)?;
        memory.write(output, &address)?;
        Ok(0)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn start_aton(
        &mut self,
        memory: &mut Memory<'_>,
        id: i32,
        address: u64,
        output: u64,
        capacity: i32,
        timeout: i32,
        retries: i32,
        flags: i32,
    ) -> Result<i32> {
        self.validate(id, timeout, retries, flags)?;
        if address == 0 || output == 0 || !(1..=1025).contains(&capacity) {
            return Err("Invalid reverse resolver buffers".into());
        }
        let address: [u8; 4] = memory.read(address, 4)?.try_into().unwrap();
        memory.validate_write(output, capacity as usize)?;
        let mut hostname = self.backend.reverse(address)?;
        if hostname.is_empty()
            || hostname.len() > 253
            || hostname.contains(&0)
            || !hostname.is_ascii()
        {
            return Err("Invalid host reverse resolver response".into());
        }
        hostname.push(0);
        if hostname.len() > capacity as usize {
            return Err("Reverse resolver output too short".into());
        }
        memory.write(output, &hostname)?;
        Ok(0)
    }
}
fn cstring(memory: &Memory<'_>, address: u64, max: usize) -> Result<Vec<u8>> {
    if address == 0 {
        return Err("Null resolver string".into());
    }
    let mut out = Vec::new();
    for i in 0..=max {
        let byte = memory.read(
            address
                .checked_add(i as u64)
                .ok_or("Resolver string overflow")?,
            1,
        )?[0];
        if byte == 0 {
            return Ok(out);
        }
        if i < max {
            out.push(byte)
        }
    }
    Err("Resolver string exceeds bound".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct TestBackend {
        fail: bool,
    }
    impl Backend for TestBackend {
        fn forward(&mut self, _: &str) -> Result<[u8; 4]> {
            if self.fail {
                Err("real backend failure".into())
            } else {
                Ok([127, 0, 0, 1])
            }
        }
        fn reverse(&mut self, _: [u8; 4]) -> Result<Vec<u8>> {
            Ok(b"localhost".to_vec())
        }
    }
    #[test]
    fn ownership_and_atomic_outputs() {
        let mut storage = [0xabu8; 128];
        storage[..11].copy_from_slice(b"localhost\0\0");
        let mut memory = Memory::default();
        memory.add(0x1000, &mut storage, true, false).unwrap();
        let mut r = Resolver::new(TestBackend { fail: false });
        let pool = r.pool_create(&memory, 0x100a, 0x4000, 0).unwrap();
        let resolver = r.resolver_create(&memory, 0x100a, pool, 0).unwrap();
        assert_eq!(r.pool_count(), 1);
        assert_eq!(r.resolver_count(), 1);
        assert_eq!(r.reserved_bytes(), 0x4000);
        assert!(r.pool_destroy(pool).is_err());
        assert!(
            r.start_ntoa(&mut memory, resolver, 0x1000, 0x107e, 0, 0, 0)
                .is_err()
        );
        assert_eq!(memory.read(0x107e, 2).unwrap(), [0xab; 2]);
        r.start_ntoa(&mut memory, resolver, 0x1000, 0x1020, 0, 0, 0)
            .unwrap();
        assert_eq!(memory.read(0x1020, 4).unwrap(), [127, 0, 0, 1]);
        assert!(
            r.start_aton(&mut memory, resolver, 0x1020, 0x1040, 3, 0, 0, 0)
                .is_err()
        );
        assert_eq!(memory.read(0x1040, 10).unwrap(), [0xab; 10]);
        r.start_aton(&mut memory, resolver, 0x1020, 0x1040, 10, 0, 0, 0)
            .unwrap();
        assert_eq!(memory.read(0x1040, 10).unwrap(), b"localhost\0");
        r.backend.fail = true;
        assert!(
            r.start_ntoa(&mut memory, resolver, 0x1000, 0x1020, 0, 0, 0)
                .is_err()
        );
        assert_eq!(memory.read(0x1020, 4).unwrap(), [127, 0, 0, 1]);
        r.resolver_destroy(resolver).unwrap();
        assert!(r.resolver_destroy(resolver).is_err());
        r.pool_destroy(pool).unwrap();
        assert_eq!(r.reserved_bytes(), 0);
    }
}
