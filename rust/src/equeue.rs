//! Process-owned event queues for explicit backend completions.
//! LP64 kevent layout comes from the PS5 payload SDK's FreeBSD11.4 header:
//! https://github.com/ps5-payload-dev/sdk/blob/master/include/freebsd/sys/event.h
//! PS5 SDL declares the Create/Wait/Delete wrappers in SDL_ps5video.h:
//! https://github.com/ps5-payload-dev/SDL/blob/8c56053f13ca13a0c050de613706ff69eb615836/src/video/ps5/SDL_ps5video.h
//! Empty waits yield Pending, never a made-up event or successful empty result.
//! Non-null timeout semantics and console error codes remain unsupported.
use crate::{Result, guest_memory::Memory};
use std::collections::{BTreeMap, VecDeque};

pub const EVENT_SIZE: usize = 32;
pub const MAX_QUEUES: usize = 16;
pub const MAX_EVENTS: usize = 256;
pub const MAX_NAME_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub ident: u64,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
    pub data: i64,
    pub udata: u64,
}
impl Event {
    pub fn encode(self) -> [u8; EVENT_SIZE] {
        let mut out = [0; EVENT_SIZE];
        out[0..8].copy_from_slice(&self.ident.to_le_bytes());
        out[8..10].copy_from_slice(&self.filter.to_le_bytes());
        out[10..12].copy_from_slice(&self.flags.to_le_bytes());
        out[12..16].copy_from_slice(&self.fflags.to_le_bytes());
        out[16..24].copy_from_slice(&self.data.to_le_bytes());
        out[24..32].copy_from_slice(&self.udata.to_le_bytes());
        out
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    /// Guest return value (zero). The actual event count is written to memory.
    Ready(i32),
    /// Executor must suspend/retry after a real completion, not report success.
    Pending,
}
struct Queue {
    name: Vec<u8>,
    events: VecDeque<Event>,
}
#[derive(Default)]
pub struct Equeues {
    queues: BTreeMap<u64, Queue>,
    last_handle: u64,
}
impl Equeues {
    pub fn len(&self) -> usize {
        self.queues.len()
    }
    pub fn is_empty(&self) -> bool {
        self.queues.is_empty()
    }
    pub fn pending_count(&self, handle: u64) -> Result<usize> {
        Ok(self
            .queues
            .get(&handle)
            .ok_or("Invalid or stale event queue")?
            .events
            .len())
    }
    pub fn name(&self, handle: u64) -> Result<&[u8]> {
        Ok(&self
            .queues
            .get(&handle)
            .ok_or("Invalid or stale event queue")?
            .name)
    }
    pub fn create(&mut self, memory: &mut Memory<'_>, output: u64, name: u64) -> Result<i32> {
        if output == 0 || name == 0 {
            return Err("Null event queue output/name".into());
        }
        memory.validate_write(output, 8)?;
        if self.queues.len() == MAX_QUEUES {
            return Err("Event queue resource limit".into());
        }
        let mut copied = Vec::new();
        for offset in 0..=MAX_NAME_BYTES {
            let address = name
                .checked_add(offset as u64)
                .ok_or("Event queue name address overflow")?;
            let byte = memory.load(address, 1)? as u8;
            if byte == 0 {
                let handle = self
                    .last_handle
                    .checked_add(1)
                    .ok_or("Event queue handle exhaustion")?;
                // Name is now owned, so source/output overlap cannot corrupt it.
                memory.store(output, handle, 8)?;
                self.queues.insert(
                    handle,
                    Queue {
                        name: copied,
                        events: VecDeque::new(),
                    },
                );
                self.last_handle = handle;
                return Ok(0);
            }
            if offset == MAX_NAME_BYTES {
                break;
            }
            copied.push(byte);
        }
        Err("Event queue name exceeds bounded length".into())
    }
    pub fn delete(&mut self, handle: u64) -> Result<i32> {
        self.queues
            .remove(&handle)
            .ok_or("Invalid or stale event queue")?;
        Ok(0)
    }
    /// Only the owner of an actual completed operation should call this.
    /// Filter/ident/data interpretation belongs to that producer, not this queue.
    pub fn enqueue(&mut self, handle: u64, event: Event) -> Result<()> {
        let queue = self
            .queues
            .get_mut(&handle)
            .ok_or("Invalid or stale event queue")?;
        if queue.events.len() == MAX_EVENTS {
            return Err("Event queue is full; completion not accepted".into());
        }
        queue.events.push_back(event);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)] // Mirrors the guest wrapper plus guarded memory.
    pub fn wait(
        &mut self,
        memory: &mut Memory<'_>,
        handle: u64,
        events: u64,
        maximum: i32,
        count: u64,
        timeout: u64,
    ) -> Result<WaitOutcome> {
        if timeout != 0 {
            return Err("Unsupported event queue non-null timeout semantics".into());
        }
        if !(1..=MAX_EVENTS as i32).contains(&maximum) || events == 0 || count == 0 {
            return Err("Invalid event queue output/count/capacity".into());
        }
        let queue = self
            .queues
            .get_mut(&handle)
            .ok_or("Invalid or stale event queue")?;
        let capacity = maximum as usize * EVENT_SIZE;
        let events_end = events
            .checked_add(capacity as u64)
            .ok_or("Event output address overflow")?;
        let count_end = count.checked_add(4).ok_or("Event count address overflow")?;
        if events < count_end && count < events_end {
            return Err("Overlapping event queue outputs".into());
        }
        // Concrete Memory has stable mappings throughout this call. Both writes
        // are validated before either is performed, and the queue commits last.
        memory.validate_write(events, capacity)?;
        memory.validate_write(count, 4)?;
        if queue.events.is_empty() {
            return Ok(WaitOutcome::Pending);
        }
        let delivered = queue.events.len().min(maximum as usize);
        let mut bytes = Vec::with_capacity(delivered * EVENT_SIZE);
        for event in queue.events.iter().take(delivered) {
            bytes.extend(event.encode());
        }
        memory.write(events, &bytes)?;
        memory.store(count, delivered as u64, 4)?;
        queue.events.drain(..delivered);
        Ok(WaitOutcome::Ready(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creation_guards_readonly_overflow_and_snapshots_aliased_names() {
        let mut queues = Equeues::default();
        let mut bytes = [0u8; 16];
        bytes[..5].copy_from_slice(b"flip\0");
        {
            let mut memory = Memory::default();
            memory.add(0x1000, &mut bytes, false, false).unwrap();
            assert!(queues.create(&mut memory, 0x1008, 0x1000).is_err());
            assert!(queues.is_empty());
        }
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        queues.last_handle = u64::MAX;
        assert!(queues.create(&mut memory, 0x1008, 0x1000).is_err());
        assert_eq!(memory.load(0x1008, 8).unwrap(), 0);
        assert!(queues.is_empty());
        queues.last_handle = 0;
        queues.create(&mut memory, 0x1000, 0x1000).unwrap();
        let handle = memory.load(0x1000, 8).unwrap();
        assert_eq!(queues.name(handle).unwrap(), b"flip");
    }
    fn event(ident: u64) -> Event {
        Event {
            ident,
            filter: -11,
            flags: 0x1234,
            fflags: 0xabcdef01,
            data: -2,
            udata: 0x1020304050607080,
        }
    }
    fn create(queues: &mut Equeues) -> u64 {
        let mut bytes = [0u8; 32];
        bytes[16..21].copy_from_slice(b"flip\0");
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        queues.create(&mut memory, 0x1000, 0x1010).unwrap();
        memory.load(0x1000, 8).unwrap()
    }
    #[test]
    fn event_layout_and_pending_delivery_are_exact() {
        let mut queues = Equeues::default();
        let handle = create(&mut queues);
        assert_eq!(queues.name(handle).unwrap(), b"flip");
        let mut bytes = [0xa5; 104];
        let mut memory = Memory::default();
        memory.add(0x2000, &mut bytes, true, false).unwrap();
        assert_eq!(
            queues
                .wait(&mut memory, handle, 0x2000, 3, 0x2060, 0)
                .unwrap(),
            WaitOutcome::Pending
        );
        assert_eq!(memory.read(0x2000, 104).unwrap(), &[0xa5; 104]);
        for id in [1, 2, 3] {
            queues.enqueue(handle, event(id)).unwrap();
        }
        assert_eq!(
            queues
                .wait(&mut memory, handle, 0x2000, 2, 0x2060, 0)
                .unwrap(),
            WaitOutcome::Ready(0)
        );
        assert_eq!(memory.load(0x2060, 4).unwrap(), 2);
        assert_eq!(queues.pending_count(handle).unwrap(), 1);
        let encoded = event(1).encode();
        assert_eq!(&encoded[..8], &1u64.to_le_bytes());
        assert_eq!(
            &encoded[8..16],
            &[0xf5, 0xff, 0x34, 0x12, 1, 0xef, 0xcd, 0xab]
        );
        assert_eq!(&encoded[16..24], &(-2i64).to_le_bytes());
        assert_eq!(&encoded[24..32], &0x1020304050607080u64.to_le_bytes());
        assert_eq!(memory.read(0x2000, 32).unwrap(), &encoded);
        assert_eq!(memory.read(0x2020, 32).unwrap(), &event(2).encode());
        assert_eq!(memory.read(0x2040, 32).unwrap(), &[0xa5; 32]);
        assert_eq!(
            queues
                .wait(&mut memory, handle, 0x2000, 1, 0x2060, 0)
                .unwrap(),
            WaitOutcome::Ready(0)
        );
        assert_eq!(memory.read(0x2000, 32).unwrap(), &event(3).encode());
    }
    #[test]
    fn queue_and_event_budgets_and_stale_handles_are_enforced() {
        let mut queues = Equeues::default();
        let handle = create(&mut queues);
        for i in 0..MAX_EVENTS {
            queues.enqueue(handle, event(i as u64)).unwrap();
        }
        assert!(queues.enqueue(handle, event(999)).is_err());
        assert_eq!(queues.pending_count(handle).unwrap(), MAX_EVENTS);
        queues.delete(handle).unwrap();
        assert!(queues.delete(handle).is_err());
        assert!(queues.enqueue(handle, event(0)).is_err());
        let next = create(&mut queues);
        assert!(next > handle);
        for _ in 1..MAX_QUEUES {
            create(&mut queues);
        }
        let mut bytes = [0u8; 16];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        assert!(queues.create(&mut memory, 0x1000, 0x1008).is_err());
        assert_eq!(memory.load(0x1000, 8).unwrap(), 0);
        assert_eq!(queues.len(), MAX_QUEUES);
    }
    #[test]
    fn failed_wait_never_writes_or_dequeues() {
        let mut queues = Equeues::default();
        let handle = create(&mut queues);
        queues.enqueue(handle, event(1)).unwrap();
        for (event_writable, count_writable) in [(true, true), (false, true), (true, false)] {
            let mut events = [0xa5; 64];
            let mut count = [0xa5; 4];
            let mut memory = Memory::default();
            memory
                .add(0x2000, &mut events, event_writable, false)
                .unwrap();
            memory
                .add(0x3000, &mut count, count_writable, false)
                .unwrap();
            for (out, max, n, timeout) in [
                (0, 1, 0x3000, 0),
                (u64::MAX, 1, 0x3000, 0),
                (0x2000, 0, 0x3000, 0),
                (0x2000, 257, 0x3000, 0),
                (0x2000, 1, 0, 0),
                (0x2000, 1, 0x2001, 0),
                (0x2021, 1, 0x3000, 0),
                (0x2000, 1, 0x3001, 0),
                (0x2000, 1, 0x3000, 1),
            ] {
                assert!(
                    queues
                        .wait(&mut memory, handle, out, max, n, timeout)
                        .is_err()
                );
            }
            if !event_writable || !count_writable {
                assert!(
                    queues
                        .wait(&mut memory, handle, 0x2000, 1, 0x3000, 0)
                        .is_err()
                );
            }
            assert_eq!(queues.pending_count(handle).unwrap(), 1);
            assert_eq!(memory.read(0x2000, 64).unwrap(), &[0xa5; 64]);
            assert_eq!(memory.read(0x3000, 4).unwrap(), &[0xa5; 4]);
        }
    }
    #[test]
    fn name_reads_and_handle_output_fail_without_creating_queue() {
        let mut queues = Equeues::default();
        let mut bytes = [b'x'; 272];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        for (output, name) in [
            (0, 0x1000),
            (0x1000, 0),
            (0x1000, u64::MAX),
            (0x1000, 0x1000),
            (0x110f, 0x1000),
        ] {
            assert!(queues.create(&mut memory, output, name).is_err());
            assert!(queues.is_empty());
            assert_eq!(memory.read(0x1000, 272).unwrap(), &[b'x'; 272]);
        }
        memory.store(0x10ff, 0, 1).unwrap();
        queues.create(&mut memory, 0x1100, 0x1000).unwrap();
        let handle = memory.load(0x1100, 8).unwrap();
        assert_eq!(queues.name(handle).unwrap(), &[b'x'; 255]);
    }
}
