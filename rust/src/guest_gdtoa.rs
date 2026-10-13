//! Owned FreeBSD11.4 gdtoa Balloc/Bfree/rv_alloc storage, not digit algorithms.
//! Policy: contrib/gdtoa/misc.c and dmisc.c, lib/libc/amd64/arith.h.
//! The 2304-byte private pool is library static data, never guest malloc.
//! Split Store to attach its pool to Memory at a reserved address. Keep all
//! pool under its RuntimeOwner; heap allocations are claimed immediately and
//! every internal mutation uses an owner-checked Memory operation. Heap caches
//! and pool survive individual printf calls; returned strings are call-local.
//! Bigint-operation allocation sequencing/pow5 cache integration remain separate.
use crate::guest_memory::Memory;
use std::rc::Rc;
pub const POOL_BYTES: usize = 2304;
const KMAX: u32 = 9;
const MAX_CLASS: u32 = 20;
const MAX_BLOCKS: usize = 4096;
#[derive(Debug)]
pub enum Error {
    Bound,
    HostAllocation,
    GuestAllocation(u32),
    Ownership,
    Corrupt,
    Memory(String),
}
fn mem<T>(r: crate::Result<T>) -> Result<T, Error> {
    r.map_err(Error::Memory)
}
#[derive(Debug)]
pub struct Ticket {
    owner: Rc<()>,
    index: usize,
    generation: u64,
}
#[derive(Debug)]
pub struct StringTicket {
    block: Ticket,
    pub address: u64,
    pub capacity: usize,
}
struct Block {
    address: u64,
    class: u32,
    length: usize,
    owned_length: usize,
    backing: usize,
    heap: bool,
    active: bool,
    string: bool,
    generation: u64,
    next: Option<usize>,
}
pub struct Store {
    pool: Vec<u8>,
    state: State,
}
pub struct State {
    runtime_owner: crate::guest_memory::RuntimeOwner,
    owner: Rc<()>,
    pool_address: u64,
    pool_backing: usize,
    pool_used: usize,
    blocks: Vec<Block>,
    heads: [Option<usize>; 10],
    next_generation: u64,
}
impl Store {
    pub fn new(pool_address: u64) -> Result<Self, Error> {
        if pool_address == 0
            || pool_address & 7 != 0
            || pool_address.checked_add(POOL_BYTES as u64).is_none()
        {
            return Err(Error::Bound);
        }
        let mut pool = Vec::new();
        pool.try_reserve_exact(POOL_BYTES)
            .map_err(|_| Error::HostAllocation)?;
        pool.resize(POOL_BYTES, 0);
        let backing = pool.as_ptr() as usize;
        Ok(Self {
            pool,
            state: State {
                runtime_owner: crate::guest_memory::RuntimeOwner::new(),
                owner: Rc::new(()),
                pool_address,
                pool_backing: backing,
                pool_used: 0,
                blocks: Vec::new(),
                heads: [None; 10],
                next_generation: 1,
            },
        })
    }
    pub fn split_mut(&mut self) -> (&mut [u8], &mut State) {
        (&mut self.pool, &mut self.state)
    }
}
impl State {
    pub fn runtime_owner(&self) -> &crate::guest_memory::RuntimeOwner {
        &self.runtime_owner
    }
    pub fn pool_address(&self) -> u64 {
        self.pool_address
    }
    pub fn pool_used(&self) -> usize {
        self.pool_used
    }
    /// Includes cached heap blocks and the whole static pool, not just active
    /// strings. Executor guards must retain these until process teardown.
    pub fn owned_ranges(&self) -> impl Iterator<Item = (u64, usize)> + '_ {
        std::iter::once((self.pool_address, POOL_BYTES)).chain(
            self.blocks
                .iter()
                .filter(|b| b.heap && b.backing != 0)
                .map(|b| (b.address, b.owned_length)),
        )
    }
    fn pool_valid(&self, memory: &Memory<'_>) -> Result<(), Error> {
        if mem(memory.read(self.pool_address, POOL_BYTES))?.as_ptr() as usize != self.pool_backing {
            return Err(Error::Ownership);
        }
        Ok(())
    }
    fn index(&self, ticket: &Ticket) -> Result<usize, Error> {
        if !Rc::ptr_eq(&self.owner, &ticket.owner) {
            return Err(Error::Ownership);
        }
        let b = self.blocks.get(ticket.index).ok_or(Error::Ownership)?;
        if !b.active || b.generation != ticket.generation {
            return Err(Error::Ownership);
        }
        Ok(ticket.index)
    }
    fn block_valid(&self, memory: &Memory<'_>, index: usize) -> Result<(), Error> {
        self.pool_valid(memory)?;
        let b = &self.blocks[index];
        if b.heap
            && mem(memory.runtime_heap_allocation_size(b.address, &self.runtime_owner))? < b.length
        {
            return Err(Error::Ownership);
        }
        if mem(memory.read(b.address, b.length))?.as_ptr() as usize != b.backing {
            return Err(Error::Ownership);
        }
        if mem(memory.load(b.address + 8, 4))? != u64::from(b.class)
            || mem(memory.load(b.address + 12, 4))? != 1u64 << b.class
        {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
    pub fn allocation_size(class: u32) -> Result<usize, Error> {
        if class > MAX_CLASS {
            return Err(Error::Bound);
        }
        // amd64 Bigint: next8,k4,maxwds4,sign4,wds4,x[1]4,padding4.
        Ok((32 + ((1usize << class) - 1) * 4 + 7) & !7)
    }
    pub fn balloc(&mut self, memory: &mut Memory<'_>, class: u32) -> Result<Ticket, Error> {
        let length = Self::allocation_size(class)?;
        self.pool_valid(memory)?;
        let generation = self.next_generation;
        let next_generation = generation.checked_add(1).ok_or(Error::Bound)?;
        let cached = if class <= KMAX {
            self.heads[class as usize]
        } else {
            None
        };
        let index = if let Some(index) = cached {
            self.block_valid(memory, index)?;
            let b = &self.blocks[index];
            let expected = b.next.map_or(0, |i| self.blocks[i].address);
            if b.active || mem(memory.load(b.address, 8))? != expected {
                return Err(Error::Corrupt);
            }
            mem(memory.validate_runtime_write(b.address, 24, &self.runtime_owner))?;
            mem(memory.runtime_store(b.address + 16, 0, 8, &self.runtime_owner))?;
            self.heads[class as usize] = b.next;
            index
        } else {
            if self.blocks.len() >= MAX_BLOCKS {
                return Err(Error::Bound);
            }
            self.blocks
                .try_reserve(1)
                .map_err(|_| Error::HostAllocation)?;
            let pool = class <= KMAX && self.pool_used + length <= POOL_BYTES;
            let address = if pool {
                self.pool_address + self.pool_used as u64
            } else {
                memory
                    .malloc(length as u64)
                    .map_err(Error::GuestAllocation)?
            };
            if !pool {
                if let Err(error) = memory.mark_runtime_heap(address, &self.runtime_owner) {
                    mem(memory.free(address))?;
                    return Err(Error::Memory(error));
                }
            }
            let initialize = (|| {
                mem(memory.validate_runtime_write(address, 24, &self.runtime_owner))?;
                let backing = mem(memory.read(address, length))?.as_ptr() as usize;
                let owned_length = if pool {
                    length
                } else {
                    mem(memory.runtime_heap_allocation_size(address, &self.runtime_owner))?
                };
                mem(memory.runtime_store(address + 8, u64::from(class), 4, &self.runtime_owner))?;
                mem(memory.runtime_store(address + 12, 1u64 << class, 4, &self.runtime_owner))?;
                mem(memory.runtime_store(address + 16, 0, 8, &self.runtime_owner))?;
                Ok((backing, owned_length))
            })();
            let (backing, owned_length) = match initialize {
                Ok(v) => v,
                Err(error) => {
                    if !pool {
                        mem(memory.runtime_free(address, &self.runtime_owner))?;
                    }
                    return Err(error);
                }
            };
            let index = self.blocks.len();
            self.blocks.push(Block {
                address,
                class,
                length,
                owned_length,
                backing,
                heap: !pool,
                active: false,
                string: false,
                generation: 0,
                next: None,
            });
            if pool {
                self.pool_used += length;
            }
            index
        };
        let b = &mut self.blocks[index];
        b.active = true;
        b.string = false;
        b.generation = generation;
        b.next = None;
        self.next_generation = next_generation;
        Ok(Ticket {
            owner: self.owner.clone(),
            index,
            generation,
        })
    }
    pub fn address(&self, ticket: &Ticket) -> Result<u64, Error> {
        Ok(self.blocks[self.index(ticket)?].address)
    }
    pub fn bfree(&mut self, memory: &mut Memory<'_>, ticket: &Ticket) -> Result<(), Error> {
        let index = self.index(ticket)?;
        self.block_valid(memory, index)?;
        if self.blocks[index].string {
            return Err(Error::Ownership);
        }
        self.release(memory, index)
    }
    fn release(&mut self, memory: &mut Memory<'_>, index: usize) -> Result<(), Error> {
        let b = &self.blocks[index];
        if b.class > KMAX {
            mem(memory.runtime_free(b.address, &self.runtime_owner))?;
            self.blocks[index].backing = 0;
        } else {
            let head = self.heads[b.class as usize];
            let next = head.map_or(0, |i| self.blocks[i].address);
            mem(memory.runtime_store(b.address, next, 8, &self.runtime_owner))?;
            self.blocks[index].next = head;
            self.heads[self.blocks[index].class as usize] = Some(index);
        }
        self.blocks[index].active = false;
        Ok(())
    }
    pub fn rv_alloc(
        &mut self,
        memory: &mut Memory<'_>,
        requested: usize,
    ) -> Result<StringTicket, Error> {
        let mut class = 0;
        while 24 + (4usize << class) <= requested {
            class += 1;
            if class > MAX_CLASS {
                return Err(Error::Bound);
            }
        }
        let block = self.balloc(memory, class)?;
        let index = block.index;
        let address = self.blocks[index].address;
        // Balloc already validated this whole header; source rv_alloc overwrites
        // low next-pointer bytes with k. The rest is output capacity.
        mem(memory.runtime_store(address, u64::from(class), 4, &self.runtime_owner))?;
        self.blocks[index].string = true;
        Ok(StringTicket {
            block,
            address: address + 4,
            capacity: self.blocks[index].length - 4,
        })
    }
    pub fn write_string(
        &self,
        memory: &mut Memory<'_>,
        ticket: &StringTicket,
        bytes: &[u8],
    ) -> Result<(), Error> {
        let index = self.index(&ticket.block)?;
        self.string_valid(memory, index, ticket)?;
        let count = bytes
            .len()
            .checked_add(1)
            .filter(|n| *n <= ticket.capacity)
            .ok_or(Error::Bound)?;
        mem(memory.validate_runtime_write(ticket.address, count, &self.runtime_owner))?;
        // String data overlays the former Bigint fields exactly as rv_alloc does.
        if !bytes.is_empty() {
            mem(memory.runtime_write(ticket.address, bytes, &self.runtime_owner))?;
        }
        mem(memory.runtime_store(
            ticket.address + bytes.len() as u64,
            0,
            1,
            &self.runtime_owner,
        ))
    }
    fn string_valid(
        &self,
        memory: &Memory<'_>,
        index: usize,
        ticket: &StringTicket,
    ) -> Result<(), Error> {
        self.pool_valid(memory)?;
        let b = &self.blocks[index];
        if !b.string || ticket.address != b.address + 4 || ticket.capacity != b.length - 4 {
            return Err(Error::Ownership);
        }
        if mem(memory.read(b.address, b.length))?.as_ptr() as usize != b.backing {
            return Err(Error::Ownership);
        }
        if b.heap
            && mem(memory.runtime_heap_allocation_size(b.address, &self.runtime_owner))? < b.length
        {
            return Err(Error::Ownership);
        }
        if mem(memory.load(b.address, 4))? != u64::from(b.class) {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
    pub fn freedtoa(
        &mut self,
        memory: &mut Memory<'_>,
        ticket: &StringTicket,
    ) -> Result<(), Error> {
        let index = self.index(&ticket.block)?;
        self.string_valid(memory, index, ticket)?;
        let b = &self.blocks[index];
        mem(memory.validate_runtime_write(b.address, 16, &self.runtime_owner))?;
        mem(memory.runtime_store(b.address + 8, u64::from(b.class), 4, &self.runtime_owner))?;
        mem(memory.runtime_store(b.address + 12, 1u64 << b.class, 4, &self.runtime_owner))?;
        self.release(memory, index)
    }
}
/// One vfprintf invocation retains its latest dtoa string through subsequent
/// non-floating fields. Release before fetching the next float, and at cleanup.
#[derive(Default)]
pub struct Retained {
    value: Option<StringTicket>,
}
impl Retained {
    pub fn current(&self) -> Option<&StringTicket> {
        self.value.as_ref()
    }
    pub fn install(&mut self, value: StringTicket) -> Result<(), StringTicket> {
        if self.value.is_some() {
            Err(value)
        } else {
            self.value = Some(value);
            Ok(())
        }
    }
    pub fn release(&mut self, state: &mut State, memory: &mut Memory<'_>) -> Result<(), Error> {
        if let Some(value) = self.value.as_ref() {
            state.freedtoa(memory, value)?;
            self.value = None;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_memory::OwnedMappings;
    #[test]
    fn pool_class_sizes_alignment_and_lifo_reuse() {
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let a = state.balloc(&mut memory, 0).unwrap();
        let b = state.balloc(&mut memory, 0).unwrap();
        assert_eq!(state.address(&a).unwrap(), 0x10000);
        assert_eq!(state.address(&b).unwrap(), 0x10020);
        assert_eq!(state.pool_used(), 64);
        state.bfree(&mut memory, &a).unwrap();
        state.bfree(&mut memory, &b).unwrap();
        let c = state.balloc(&mut memory, 0).unwrap();
        assert_eq!(state.address(&c).unwrap(), 0x10020);
        assert!(matches!(
            state.bfree(&mut memory, &b),
            Err(Error::Ownership)
        ));
        assert_eq!(State::allocation_size(1).unwrap(), 40);
        assert_eq!(State::allocation_size(9).unwrap(), 2080);
    }
    #[test]
    fn exhausted_pool_uses_real_heap_and_small_classes_stay_cached() {
        let mut owned = OwnedMappings::default();
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory.attach_owned(&mut owned).unwrap();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let a = state.balloc(&mut memory, 9).unwrap();
        assert_eq!(state.address(&a).unwrap(), 0x10000);
        let b = state.balloc(&mut memory, 9).unwrap();
        let heap = state.address(&b).unwrap();
        assert!(
            memory
                .runtime_heap_allocation_size(heap, state.runtime_owner())
                .is_ok()
        );
        assert!(state.owned_ranges().any(|(base, size)| {
            base == heap
                && size
                    == memory
                        .runtime_heap_allocation_size(heap, state.runtime_owner())
                        .unwrap()
        }));
        state.bfree(&mut memory, &b).unwrap();
        assert!(
            memory
                .runtime_heap_allocation_size(heap, state.runtime_owner())
                .is_ok()
        );
        let reused = state.balloc(&mut memory, 9).unwrap();
        assert_eq!(state.address(&reused).unwrap(), heap);
        let big = state.balloc(&mut memory, 10).unwrap();
        let address = state.address(&big).unwrap();
        state.bfree(&mut memory, &big).unwrap();
        assert!(
            memory
                .runtime_heap_allocation_size(address, state.runtime_owner())
                .is_err()
        );
        assert!(memory.read(address, 1).is_err());
        assert_eq!(state.owned_ranges().count(), 2);
        for _ in 0..5 {
            state.balloc(&mut memory, 1).unwrap();
        }
        let padded = state.balloc(&mut memory, 1).unwrap();
        let address = state.address(&padded).unwrap();
        assert_eq!(State::allocation_size(1).unwrap(), 40);
        assert_eq!(
            memory
                .runtime_heap_allocation_size(address, state.runtime_owner())
                .unwrap(),
            48
        );
        assert!(state.owned_ranges().any(|range| range == (address, 48)));
        assert!(memory.write(address + 47, &[1]).is_err());
        assert!(memory.free(address).is_err());
        assert!(memory.heap_allocation_size(address).is_err());
        state.bfree(&mut memory, &padded).unwrap();
        assert!(memory.write(address + 47, &[1]).is_err());
        assert!(memory.free(address).is_err());
        let again = state.balloc(&mut memory, 1).unwrap();
        assert_eq!(state.address(&again).unwrap(), address);
    }
    #[test]
    fn string_overlays_header_retains_until_release_and_restores_class() {
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let string = state.rv_alloc(&mut memory, 27).unwrap();
        assert_eq!(string.address, 0x10004);
        assert_eq!(string.capacity, 28);
        state
            .write_string(&mut memory, &string, b"012345678901234567890123456")
            .unwrap();
        let mut retained = Retained::default();
        retained.install(string).unwrap();
        assert!(retained.current().is_some());
        retained.release(state, &mut memory).unwrap();
        assert!(retained.current().is_none());
        retained.release(state, &mut memory).unwrap();
        let again = state.balloc(&mut memory, 0).unwrap();
        assert_eq!(state.address(&again).unwrap(), 0x10000);
        assert_eq!(memory.load(0x10008, 4).unwrap(), 0);
        assert_eq!(memory.load(0x1000c, 4).unwrap(), 1);
        let next = state.rv_alloc(&mut memory, 28).unwrap();
        assert_eq!(next.capacity, 36);
    }
    #[test]
    fn allocation_and_write_failures_leave_owned_state_recoverable() {
        let mut store = Store::new(0x10000).unwrap();
        {
            let (pool, state) = store.split_mut();
            let mut memory = Memory::default();
            memory.add(0x10000, pool, false, false).unwrap();
            assert!(matches!(
                state.balloc(&mut memory, 0),
                Err(Error::Memory(_))
            ));
            assert_eq!(state.pool_used(), 0);
            assert_eq!(state.owned_ranges().count(), 1);
        }
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let first = state.balloc(&mut memory, 0).unwrap();
        for _ in 1..72 {
            state.balloc(&mut memory, 0).unwrap();
        }
        assert_eq!(state.pool_used(), POOL_BYTES);
        assert!(matches!(
            state.balloc(&mut memory, 0),
            Err(Error::GuestAllocation(12))
        ));
        assert_eq!(state.pool_used(), POOL_BYTES);
        state.bfree(&mut memory, &first).unwrap();
        let string = state.rv_alloc(&mut memory, 3).unwrap();
        let address = string.address;
        let mut retained = Retained::default();
        retained.install(string).unwrap();
        memory
            .runtime_store(address - 4, 17, 4, state.runtime_owner())
            .unwrap();
        assert!(matches!(
            retained.release(state, &mut memory),
            Err(Error::Corrupt)
        ));
        assert!(retained.current().is_some());
        memory
            .runtime_store(address - 4, 0, 4, state.runtime_owner())
            .unwrap();
        retained.release(state, &mut memory).unwrap();
        assert_eq!(state.pool_used(), POOL_BYTES);
    }
    #[test]
    fn cached_heap_survives_dispatch_memory_reconstruction() {
        let mut owned = OwnedMappings::default();
        let mut store = Store::new(0x10000).unwrap();
        let address;
        {
            let (pool, state) = store.split_mut();
            let mut memory = Memory::default();
            memory.attach_owned(&mut owned).unwrap();
            memory
                .add_runtime_data(0x10000, pool, state.runtime_owner())
                .unwrap();
            state.balloc(&mut memory, 9).unwrap();
            let heap = state.balloc(&mut memory, 9).unwrap();
            address = state.address(&heap).unwrap();
            state.bfree(&mut memory, &heap).unwrap();
        }
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory.attach_owned(&mut owned).unwrap();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        assert!(memory.write(address, &[0]).is_err());
        assert!(memory.free(address).is_err());
        let reused = state.balloc(&mut memory, 9).unwrap();
        assert_eq!(state.address(&reused).unwrap(), address);
    }

    #[test]
    fn wrong_mapping_foreign_tickets_corruption_and_bounds_fail() {
        let mut store = Store::new(0x10000).unwrap();
        let (_, state) = store.split_mut();
        let mut impostor = [0u8; POOL_BYTES];
        let mut memory = Memory::default();
        memory.add(0x10000, &mut impostor, true, false).unwrap();
        assert!(matches!(
            state.balloc(&mut memory, 0),
            Err(Error::Ownership)
        ));
        drop(memory);
        let mut other = Store::new(0x20000).unwrap();
        let (pool, state) = store.split_mut();
        let (pool2, state2) = other.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        memory
            .add_runtime_data(0x20000, pool2, state2.runtime_owner())
            .unwrap();
        let a = state.balloc(&mut memory, 0).unwrap();
        assert!(matches!(
            state2.bfree(&mut memory, &a),
            Err(Error::Ownership)
        ));
        memory
            .runtime_store(state.address(&a).unwrap() + 8, 9, 4, state.runtime_owner())
            .unwrap();
        assert!(matches!(state.bfree(&mut memory, &a), Err(Error::Corrupt)));
        assert!(matches!(state.balloc(&mut memory, 21), Err(Error::Bound)));
        assert!(matches!(
            state.rv_alloc(&mut memory, usize::MAX),
            Err(Error::Bound)
        ));
    }
}
