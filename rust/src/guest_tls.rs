//! Guest-thread-owned pthread keys; never host TLS or a threading substitute.
//! ABI: SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/{pthread.h,sys/_pthreadtypes.h,sys/errno.h}.
//! Semantics: FreeBSD releng/11.4/lib/libthr/thread/thr_spec.c.
//! Observed SDL call0x18a2f supplies a NULL destructor. Non-NULL callbacks remain
//! unsupported until translated cleanup scheduling exists; deletion never calls one.
use crate::{Result, guest_memory::Memory};
pub const KEY_LIMIT: usize = 256;
pub const THREAD_LIMIT: usize = 64;
pub const EAGAIN: i32 = 35;
pub const EINVAL: i32 = 22;
#[derive(Clone, Copy, Debug, Default)]
struct Key {
    allocated: bool,
    generation: u64,
}
#[derive(Debug)]
struct Thread {
    id: u64,
    values: [(u64, u64); KEY_LIMIT],
}
#[derive(Debug)]
pub struct GuestTls {
    keys: [Key; KEY_LIMIT],
    threads: Vec<Thread>,
    last_thread_id: u64,
}
impl Default for GuestTls {
    fn default() -> Self {
        Self {
            keys: [Key::default(); KEY_LIMIT],
            threads: Vec::new(),
            last_thread_id: 0,
        }
    }
}
impl GuestTls {
    /// Explicit scheduler identity; zero is reserved, and registration is not
    /// thread creation. No values are inherited from another guest thread.
    /// Identities must increase monotonically so retired contexts cannot alias
    /// new contexts. Failed registration does not consume the identity.
    pub fn register_thread(&mut self, id: u64) -> Result<()> {
        if id <= self.last_thread_id {
            return Err("Guest TLS thread identity must be new and increasing".into());
        }
        if self.threads.len() == THREAD_LIMIT {
            return Err("Guest TLS thread budget exhausted".into());
        }
        self.threads
            .try_reserve(1)
            .map_err(|_| "Cannot allocate guest TLS thread")?;
        self.threads.push(Thread {
            id,
            values: [(0, 0); KEY_LIMIT],
        });
        self.last_thread_id = id;
        Ok(())
    }
    /// Scheduler-only lifecycle operation: caller must establish that the
    /// thread has completed and no retained continuation can run it again.
    /// Values are non-owning guest pointers. Non-NULL key destructors remain
    /// unsupported, so retiring this table does not skip accepted callbacks.
    pub fn retire_thread(&mut self, id: u64) -> Result<()> {
        let index = self
            .threads
            .iter()
            .position(|thread| thread.id == id)
            .ok_or("Unregistered guest TLS thread")?;
        self.threads.remove(index);
        Ok(())
    }
    pub fn live_threads(&self) -> usize {
        self.threads.len()
    }
    pub fn contains_thread(&self, id: u64) -> bool {
        self.threads.iter().any(|t| t.id == id)
    }
    pub fn active_keys(&self) -> usize {
        self.keys.iter().filter(|k| k.allocated).count()
    }
    /// Caller validates the guest return target/frame before committing output.
    pub fn key_create(
        &mut self,
        memory: &mut Memory<'_>,
        output: u64,
        destructor: u64,
    ) -> Result<i32> {
        if destructor != 0 {
            return Err("Non-NULL pthread TLS destructors require guest cleanup scheduling".into());
        }
        memory.validate_write(output, 4)?;
        let Some(index) = self.keys.iter().position(|k| !k.allocated) else {
            return Ok(EAGAIN);
        };
        let generation = self.keys[index]
            .generation
            .checked_add(1)
            .ok_or("Guest TLS key generation exhausted")?;
        memory.write(output, &((index + 1) as i32).to_le_bytes())?;
        self.keys[index] = Key {
            allocated: true,
            generation,
        };
        Ok(0)
    }
    fn index(&self, key: i32) -> Option<usize> {
        let index = key.checked_sub(1)? as usize;
        self.keys.get(index).filter(|k| k.allocated).map(|_| index)
    }
    pub fn key_delete(&mut self, key: i32) -> i32 {
        let Some(index) = self.index(key) else {
            return EINVAL;
        };
        self.keys[index].allocated = false;
        0
    }
    pub fn getspecific(&self, thread: u64, key: i32) -> Result<u64> {
        let thread = self
            .threads
            .iter()
            .find(|t| t.id == thread)
            .ok_or("Unregistered guest TLS thread")?;
        let Some(index) = self.index(key) else {
            return Ok(0);
        };
        let (generation, value) = thread.values[index];
        Ok(if generation == self.keys[index].generation {
            value
        } else {
            0
        })
    }
    pub fn setspecific(&mut self, thread: u64, key: i32, value: u64) -> Result<i32> {
        let position = self
            .threads
            .iter()
            .position(|t| t.id == thread)
            .ok_or("Unregistered guest TLS thread")?;
        let Some(index) = self.index(key) else {
            return Ok(EINVAL);
        };
        self.threads[position].values[index] = (self.keys[index].generation, value);
        Ok(0)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_worker_retirement_reclaims_capacity_without_reusing_identity() {
        let mut slot = [0u8; 4];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut slot, true, false).unwrap();
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        tls.key_create(&mut memory, 0x1000, 0).unwrap();
        tls.setspecific(1, 1, 0xabcd).unwrap();
        for id in 2..200 {
            tls.register_thread(id).unwrap();
            assert_eq!(tls.getspecific(id, 1).unwrap(), 0);
            tls.setspecific(id, 1, u64::MAX).unwrap();
            tls.retire_thread(id).unwrap();
            assert_eq!(tls.live_threads(), 1);
            assert!(tls.getspecific(id, 1).is_err());
            assert!(tls.setspecific(id, 1, 0).is_err());
            assert!(tls.retire_thread(id).is_err());
            assert!(tls.register_thread(id).is_err());
            assert_eq!(tls.getspecific(1, 1).unwrap(), 0xabcd);
        }
        for id in 200..263 {
            tls.register_thread(id).unwrap();
        }
        assert_eq!(tls.live_threads(), THREAD_LIMIT);
        assert!(tls.register_thread(263).is_err());
        tls.retire_thread(200).unwrap();
        tls.register_thread(263).unwrap();
        assert_eq!(tls.getspecific(263, 1).unwrap(), 0);
    }
    #[test]
    fn explicit_threads_and_reused_keys_preserve_isolation() {
        let mut output = [0u8; 4];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut output, true, false).unwrap();
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        tls.register_thread(2).unwrap();
        assert!(tls.register_thread(1).is_err());
        assert!(tls.register_thread(0).is_err());
        assert_eq!(tls.key_create(&mut memory, 0x1000, 0).unwrap(), 0);
        let key = memory.load(0x1000, 4).unwrap() as i32;
        assert_eq!(key, 1);
        assert_eq!(tls.setspecific(1, key, 0x1234_5678_9abc_def0).unwrap(), 0);
        assert_eq!(tls.getspecific(1, key).unwrap(), 0x1234_5678_9abc_def0);
        assert_eq!(tls.getspecific(2, key).unwrap(), 0);
        assert_eq!(tls.setspecific(2, key, 99).unwrap(), 0);
        assert_eq!(tls.key_delete(key), 0);
        assert_eq!(tls.key_delete(key), EINVAL);
        assert_eq!(tls.getspecific(1, key).unwrap(), 0);
        assert_eq!(tls.key_create(&mut memory, 0x1000, 0).unwrap(), 0);
        assert_eq!(tls.getspecific(1, key).unwrap(), 0);
        assert_eq!(tls.getspecific(2, key).unwrap(), 0);
        assert_eq!(tls.setspecific(1, key, 17).unwrap(), 0);
        assert_eq!(tls.setspecific(1, key, 0).unwrap(), 0);
        assert_eq!(tls.getspecific(1, key).unwrap(), 0);
        for bad in [i32::MIN, -1, 0, 257, i32::MAX] {
            assert_eq!(tls.key_delete(bad), EINVAL);
            assert_eq!(tls.getspecific(1, bad).unwrap(), 0);
            assert_eq!(tls.setspecific(1, bad, 7).unwrap(), EINVAL);
        }
        assert!(tls.getspecific(3, key).is_err());
        assert!(tls.setspecific(3, key, 0).is_err());
    }
    #[test]
    fn guards_and_exhaustion_do_not_allocate_or_overwrite() {
        let mut output = [0xabu8; 4];
        let mut readonly = [0u8; 4];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut output, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        let mut tls = GuestTls::default();
        for (address, destructor) in [(0x1001, 0), (0x2000, 0), (u64::MAX, 0), (0x1000, 0x1234)] {
            assert!(tls.key_create(&mut memory, address, destructor).is_err());
            assert_eq!(tls.active_keys(), 0);
        }
        assert_eq!(memory.load(0x1000, 4).unwrap(), 0xabababab);
        for expected in 1..=KEY_LIMIT {
            assert_eq!(tls.key_create(&mut memory, 0x1000, 0).unwrap(), 0);
            assert_eq!(memory.load(0x1000, 4).unwrap(), expected as u64);
        }
        assert_eq!(tls.key_create(&mut memory, 0x1000, 0).unwrap(), EAGAIN);
        assert_eq!(memory.load(0x1000, 4).unwrap(), 256);
        for id in 1..=THREAD_LIMIT {
            tls.register_thread(id as u64).unwrap();
        }
        assert!(tls.register_thread(65).is_err());
    }
}
