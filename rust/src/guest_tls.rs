//! Guest-thread-owned pthread keys; never host TLS or a threading substitute.
//! ABI: SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/{pthread.h,sys/_pthreadtypes.h,sys/errno.h}.
//! Semantics: FreeBSD releng/11.4/lib/libthr/thread/thr_spec.c.
//! Observed SDL call0x18a2f supplies a NULL destructor. Non-NULL callbacks remain
//! unsupported until translated cleanup scheduling exists; deletion never calls one.
use crate::{Result, guest_memory::Memory};
pub const KEY_LIMIT: usize = 256;
pub const THREAD_LIMIT: usize = 64;
/// Prototype storage budget including NUL; not a console ABI name limit.
pub const THREAD_NAME_LIMIT: usize = 1024;
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
    name: Vec<u8>,
    values: [(u64, u64); KEY_LIMIT],
    signal_mask: [u8; crate::guest_signal::SIZE],
    cancel_type: i32,
    cancel_enabled: bool,
    scheduling: (i32, i32),
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
            name: Vec::new(),
            values: [(0, 0); KEY_LIMIT],
            signal_mask: [0; crate::guest_signal::SIZE],
            cancel_type: 0,
            cancel_enabled: true,
            // FreeBSD11.4 thr_init.c _pthread_attr_default: OTHER, prio0.
            // This is the prototype's initial policy, not a console measurement.
            scheduling: (2, 0),
        });
        self.last_thread_id = id;
        Ok(())
    }
    /// pthread_create copies the creating thread's mask, not its TLS values.
    /// Primary: FreeBSD11.4 share/man/man3/pthread_create.3; kern_thr.c copies
    /// td_startcopy..td_endcopy, containing td_sigmask in sys/sys/proc.h.
    pub fn register_thread_inheriting(&mut self, id: u64, parent: u64) -> Result<()> {
        let mask = self.signal_mask(parent)?;
        let scheduling = self.scheduling(parent)?;
        self.register_thread(id)?;
        let child = self.threads.last_mut().expect("Just registered");
        child.signal_mask = mask;
        // Default PTHREAD_INHERIT_SCHED, FreeBSD11.4 thr_create.c.
        // Explicit scheduling attributes remain unsupported.
        child.scheduling = scheduling;
        Ok(())
    }
    pub fn scheduling(&self, id: u64) -> Result<(i32, i32)> {
        self.threads
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.scheduling)
            .ok_or_else(|| "Unregistered guest scheduling thread".into())
    }
    pub(crate) fn replace_scheduling(&mut self, id: u64, policy: i32, priority: i32) -> Result<()> {
        let thread = self
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Unregistered guest scheduling thread")?;
        thread.scheduling = (policy, priority);
        Ok(())
    }
    pub fn signal_mask(&self, id: u64) -> Result<[u8; crate::guest_signal::SIZE]> {
        self.threads
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.signal_mask)
            .ok_or_else(|| "Unregistered guest signal-mask thread".into())
    }
    /// Cancellation configuration only: requests, state changes and delivery
    /// remain unsupported. Every thread starts enabled and deferred (0), even
    /// when its creator has selected asynchronous mode (2).
    pub fn cancel_type(&self, id: u64) -> Result<i32> {
        self.threads
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.cancel_type)
            .ok_or_else(|| "Unregistered guest cancellation thread".into())
    }
    pub fn cancel_enabled(&self, id: u64) -> Result<bool> {
        self.threads
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.cancel_enabled)
            .ok_or_else(|| "Unregistered guest cancellation thread".into())
    }
    /// SDK pthread.h: DEFERRED=0, ASYNCHRONOUS=2. FreeBSD11.4 thr_cancel.c
    /// returns EINVAL without touching errno/output for invalid types. There is
    /// no accepted cancellation source, so no pending delivery can intervene.
    /// Caller validates return-frame aliases before committing this operation.
    pub fn set_cancel_type(
        &mut self,
        id: u64,
        kind: i32,
        previous: u64,
        memory: &mut Memory<'_>,
    ) -> Result<i32> {
        if !matches!(kind, 0 | 2) {
            return Ok(EINVAL);
        }
        let thread = self
            .threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Unregistered guest cancellation thread")?;
        if previous != 0 {
            memory.validate_write(previous, 4)?;
            memory.write(previous, &thread.cancel_type.to_le_bytes())?;
        }
        thread.cancel_type = kind;
        Ok(0)
    }
    pub(crate) fn replace_signal_mask(
        &mut self,
        id: u64,
        mask: [u8; crate::guest_signal::SIZE],
    ) -> Result<()> {
        self.threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or("Unregistered guest signal-mask thread")?
            .signal_mask = mask;
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
    /// FreeBSD11.4 thr_info.c: void ABI, NULL clears, absent/dead IDs ignored.
    /// Live IDs must be retired when execution completes. Names are copied raw
    /// bytes; malformed/over-budget memory stops explicitly before replacement.
    pub fn set_name(&mut self, memory: &Memory<'_>, id: u64, address: u64) -> Result<()> {
        let Some(thread) = self.threads.iter_mut().find(|thread| thread.id == id) else {
            return Ok(());
        };
        if address == 0 {
            thread.name.clear();
            return Ok(());
        }
        let mut name = Vec::new();
        name.try_reserve_exact(THREAD_NAME_LIMIT - 1)
            .map_err(|_| "Cannot allocate guest thread name")?;
        for index in 0..THREAD_NAME_LIMIT {
            let source = address
                .checked_add(index as u64)
                .ok_or("Thread name address overflow")?;
            let byte = memory.read(source, 1)?[0];
            if byte == 0 {
                thread.name = name;
                return Ok(());
            }
            if index + 1 < THREAD_NAME_LIMIT {
                name.push(byte);
            }
        }
        Err("Guest thread name exceeds prototype storage budget".into())
    }
    pub fn thread_name(&self, id: u64) -> Option<&[u8]> {
        self.threads
            .iter()
            .find(|thread| thread.id == id)
            .map(|thread| thread.name.as_slice())
    }
    pub fn next_thread_id(&self) -> Result<u64> {
        self.last_thread_id
            .checked_add(1)
            .ok_or("Guest TLS thread identity exhausted".into())
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
    fn scheduling_inheritance_is_owned_and_retired() {
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        assert_eq!(tls.scheduling(1).unwrap(), (2, 0));
        tls.replace_scheduling(1, 2, 17).unwrap();
        tls.register_thread_inheriting(2, 1).unwrap();
        assert_eq!(tls.scheduling(2).unwrap(), (2, 17));
        tls.replace_scheduling(2, 2, 23).unwrap();
        assert_eq!(tls.scheduling(1).unwrap(), (2, 17));
        tls.retire_thread(2).unwrap();
        assert!(tls.scheduling(2).is_err());
        assert!(tls.replace_scheduling(2, 2, 0).is_err());
        assert!(tls.register_thread_inheriting(3, 2).is_err());
        assert!(!tls.contains_thread(3));
        tls.register_thread(3).unwrap();
        assert_eq!(tls.scheduling(3).unwrap(), (2, 0));
    }
    #[test]
    fn cancellation_configuration_atomic_and_not_inherited() {
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        let mut bytes = [0x55; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        assert!(tls.cancel_enabled(1).unwrap());
        assert_eq!(tls.set_cancel_type(1, 2, 0x1000, &mut memory).unwrap(), 0);
        assert_eq!(
            memory.read(0x1000, 8).unwrap(),
            &[0, 0, 0, 0, 0x55, 0x55, 0x55, 0x55]
        );
        assert_eq!(
            tls.set_cancel_type(1, 1, u64::MAX, &mut memory).unwrap(),
            EINVAL
        );
        assert!(tls.set_cancel_type(1, 0, 0x1005, &mut memory).is_err());
        assert_eq!(tls.cancel_type(1).unwrap(), 2);
        tls.register_thread_inheriting(2, 1).unwrap();
        assert_eq!(tls.cancel_type(2).unwrap(), 0);
        assert!(tls.cancel_enabled(2).unwrap());
        assert_eq!(tls.set_cancel_type(2, 2, 0, &mut memory).unwrap(), 0);
        assert_eq!(tls.set_cancel_type(1, 0, 0x1000, &mut memory).unwrap(), 0);
        assert_eq!(memory.read(0x1000, 4).unwrap(), &[2, 0, 0, 0]);
        assert_eq!(tls.cancel_type(2).unwrap(), 2);
        tls.retire_thread(2).unwrap();
        assert!(tls.set_cancel_type(2, 0, 0, &mut memory).is_err());
    }
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

#[cfg(test)]
mod name_tests {
    use super::*;
    #[test]
    fn copied_names_atomic_failure_and_retirement() {
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        tls.register_thread(2).unwrap();
        let mut bytes = b"worker\0".to_vec();
        let mut m = Memory::default();
        m.add(0x1000, &mut bytes, true, false).unwrap();
        tls.set_name(&m, 1, 0x1000).unwrap();
        m.write(0x1000, b"change").unwrap();
        assert_eq!(tls.thread_name(1), Some(&b"worker"[..]));
        assert!(tls.set_name(&m, 1, 0xffff).is_err());
        assert_eq!(tls.thread_name(1), Some(&b"worker"[..]));
        tls.set_name(&m, 99, u64::MAX).unwrap();
        assert_eq!(tls.thread_name(2), Some(&b""[..]));
        tls.set_name(&m, 1, 0).unwrap();
        assert_eq!(tls.thread_name(1), Some(&b""[..]));
        let mut full = vec![0xff; THREAD_NAME_LIMIT];
        let mut long = Memory::default();
        long.add(0x2000, &mut full, true, false).unwrap();
        assert!(tls.set_name(&long, 1, 0x2000).is_err());
        assert_eq!(tls.thread_name(1), Some(&b""[..]));
        long.write(0x2000 + (THREAD_NAME_LIMIT - 1) as u64, &[0])
            .unwrap();
        tls.set_name(&long, 1, 0x2000).unwrap();
        assert_eq!(tls.thread_name(1).unwrap().len(), THREAD_NAME_LIMIT - 1);
        tls.retire_thread(1).unwrap();
        tls.set_name(&long, 1, u64::MAX).unwrap();
        assert_eq!(tls.thread_name(1), None);
        assert_eq!(tls.thread_name(2), Some(&b""[..]));
    }
}
