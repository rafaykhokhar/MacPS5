//! Per-runtime guest heap. Only the mapping adapter exposes host backing pointers.
const MAX_ALLOCATION: u64 = 16 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_SLOTS: usize = 4096;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Exhausted,
    InvalidFree,
    Mapping,
}
pub trait Mapping {
    /// Register writable, non-executable backing. Must be atomic on failure.
    fn map(&mut self, backing: &mut [u128]) -> Result<u64, Error>;
    /// Remove exactly this mapping before the backing can be destroyed.
    fn unmap(&mut self, address: u64, backing: &mut [u128]) -> Result<(), Error>;
}
struct Allocation {
    address: u64,
    backing: Vec<u128>,
}
pub struct Heap<M: Mapping> {
    mapping: M,
    allocations: Vec<Allocation>,
    total: u64,
}
impl<M: Mapping> Heap<M> {
    pub fn new(mapping: M) -> Self {
        Self {
            mapping,
            allocations: Vec::new(),
            total: 0,
        }
    }
    pub fn bytes(&self) -> u64 {
        self.total
    }
    pub fn calloc(&mut self, count: u64, size: u64) -> Result<u64, Error> {
        self.malloc(count.checked_mul(size).ok_or(Error::Exhausted)?)
    }
    /// Owned backing is initialized for Rust safety. malloc callers must not rely
    /// on those initial bytes; calloc shares this zero-initialized allocation path.
    pub fn malloc(&mut self, size: u64) -> Result<u64, Error> {
        let requested = size.max(1);
        let rounded = requested.checked_add(15).ok_or(Error::Exhausted)? & !15;
        if rounded > MAX_ALLOCATION
            || rounded > MAX_TOTAL - self.total
            || self.allocations.len() >= MAX_SLOTS
        {
            return Err(Error::Exhausted);
        }
        self.allocations
            .try_reserve(1)
            .map_err(|_| Error::Exhausted)?;
        let mut backing = Vec::new();
        backing
            .try_reserve_exact((rounded / 16) as usize)
            .map_err(|_| Error::Exhausted)?;
        backing.resize((rounded / 16) as usize, 0u128);
        let address = self.mapping.map(&mut backing)?;
        self.allocations.push(Allocation { address, backing });
        self.total += rounded;
        Ok(address)
    }
    pub fn free(&mut self, address: u64) -> Result<(), Error> {
        if address == 0 {
            return Ok(());
        }
        let index = self
            .allocations
            .iter()
            .position(|a| a.address == address)
            .ok_or(Error::InvalidFree)?;
        let a = &mut self.allocations[index];
        self.mapping.unmap(a.address, &mut a.backing)?;
        self.total -= a.backing.len() as u64 * 16;
        self.allocations.swap_remove(index);
        Ok(())
    }
}
impl<M: Mapping> Drop for Heap<M> {
    fn drop(&mut self) {
        for mut a in self.allocations.drain(..) {
            if self.mapping.unmap(a.address, &mut a.backing).is_err() {
                // A broken adapter must never leave a mapping pointing at freed host memory.
                std::mem::forget(a.backing);
            }
        }
    }
}
#[allow(unsafe_code)]
pub mod ffi {
    use super::*;
    use crate::strings::ffi::Outcome;
    use std::ffi::c_void;
    #[repr(C)]
    pub struct Api {
        pub context: *mut c_void,
        pub map: unsafe extern "C" fn(*mut c_void, *mut u8, u64) -> u64,
        pub unmap: unsafe extern "C" fn(*mut c_void, u64, *mut u8, u64) -> u64,
    }
    pub struct Adapter(Api);
    impl Mapping for Adapter {
        fn map(&mut self, data: &mut [u128]) -> Result<u64, Error> {
            // SAFETY: constructor contract; backing remains owned and stationary until unmap.
            let address = unsafe {
                (self.0.map)(
                    self.0.context,
                    data.as_mut_ptr().cast(),
                    std::mem::size_of_val(data) as u64,
                )
            };
            if address == 0 {
                Err(Error::Mapping)
            } else {
                Ok(address)
            }
        }
        fn unmap(&mut self, address: u64, data: &mut [u128]) -> Result<(), Error> {
            // SAFETY: constructor contract; this is the exact live span originally registered.
            let status = unsafe {
                (self.0.unmap)(
                    self.0.context,
                    address,
                    data.as_mut_ptr().cast(),
                    std::mem::size_of_val(data) as u64,
                )
            };
            if status == 0 {
                Ok(())
            } else {
                Err(Error::Mapping)
            }
        }
    }
    /// # Safety
    /// Valid synchronous non-unwinding callbacks/context must outlive the heap.
    /// Map returns a unique nonzero guest address or zero without retaining data.
    /// Unmap removes the exact span; callers serialize all access to heap/backing.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_heap_new_v1(api: Api) -> *mut Heap<Adapter> {
        if api.context.is_null() {
            return std::ptr::null_mut();
        }
        Box::into_raw(Box::new(Heap::new(Adapter(api))))
    }
    /// # Safety
    /// Heap is a live pointer from heap_new, exclusively borrowed for this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_heap_call_v1(
        heap: *mut Heap<Adapter>,
        operation: u64,
        first: u64,
        second: u64,
    ) -> Outcome {
        // SAFETY: caller guarantees provenance, lifetime and exclusivity.
        let Some(heap) = (unsafe { heap.as_mut() }) else {
            return Outcome { value: 0, fault: 8 };
        };
        let result = match operation {
            6 => heap.calloc(first, second),
            7 => heap.free(first).map(|()| 0),
            9 => heap.malloc(first),
            _ => return Outcome { value: 0, fault: 8 },
        };
        match result {
            Ok(value) => Outcome { value, fault: 0 },
            Err(Error::Exhausted) => Outcome {
                value: 0,
                fault: 12,
            },
            Err(_) => Outcome { value: 0, fault: 8 },
        }
    }
    /// # Safety
    /// Pointer must come from heap_new and be destroyed exactly once while callbacks remain valid.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_heap_delete_v1(heap: *mut Heap<Adapter>) {
        if !heap.is_null() {
            unsafe {
                drop(Box::from_raw(heap));
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    #[derive(Default)]
    struct State {
        live: Vec<u64>,
        next: u64,
        reject: bool,
        fail_unmap: bool,
    }
    struct Map(Rc<RefCell<State>>);
    impl Mapping for Map {
        fn map(&mut self, bytes: &mut [u128]) -> Result<u64, Error> {
            assert!(bytes.iter().all(|v| *v == 0));
            assert_eq!(bytes.as_ptr() as usize % 16, 0);
            let mut s = self.0.borrow_mut();
            if s.reject {
                return Err(Error::Mapping);
            }
            s.next += 0x1000000;
            let address = s.next;
            s.live.push(address);
            Ok(address)
        }
        fn unmap(&mut self, address: u64, _: &mut [u128]) -> Result<(), Error> {
            let mut s = self.0.borrow_mut();
            if s.fail_unmap {
                return Err(Error::Mapping);
            }
            let index = s
                .live
                .iter()
                .position(|a| *a == address)
                .ok_or(Error::Mapping)?;
            s.live.swap_remove(index);
            Ok(())
        }
    }
    #[test]
    fn malloc_shares_budget_and_free_ownership_with_calloc() {
        let state = Rc::new(RefCell::new(State::default()));
        let mut heap = Heap::new(Map(state));
        let a = heap.malloc(17).unwrap();
        let b = heap.calloc(2, 9).unwrap();
        assert_ne!(a, b);
        assert_eq!(heap.bytes(), 64);
        assert_eq!(heap.malloc(u64::MAX), Err(Error::Exhausted));
        assert_eq!(heap.malloc(MAX_ALLOCATION + 1), Err(Error::Exhausted));
        assert_eq!(heap.bytes(), 64);
        heap.free(a).unwrap();
        heap.free(b).unwrap();
        assert_eq!(heap.bytes(), 0);
        let zero = heap.malloc(0).unwrap();
        assert_ne!(zero, 0);
        heap.free(zero).unwrap();
    }
    #[test]
    fn ownership_zero_alignment_overflow_and_teardown() {
        let state = Rc::new(RefCell::new(State::default()));
        {
            let mut heap = Heap::new(Map(state.clone()));
            let a = heap.calloc(3, 7).unwrap();
            assert_eq!(heap.bytes(), 32);
            let b = heap.calloc(0, u64::MAX).unwrap();
            assert_ne!(a, b);
            assert_eq!(heap.bytes(), 48);
            assert_eq!(heap.calloc(u64::MAX, 2), Err(Error::Exhausted));
            assert_eq!(heap.calloc(1, MAX_ALLOCATION + 1), Err(Error::Exhausted));
            assert_eq!(heap.free(a + 1), Err(Error::InvalidFree));
            assert_eq!(heap.bytes(), 48);
            heap.free(0).unwrap();
            heap.free(a).unwrap();
            assert_eq!(heap.free(a), Err(Error::InvalidFree));
            state.borrow_mut().reject = true;
            assert_eq!(heap.calloc(1, 4), Err(Error::Mapping));
            assert_eq!(heap.bytes(), 16);
            state.borrow_mut().fail_unmap = true;
            assert_eq!(heap.free(b), Err(Error::Mapping));
            assert_eq!(heap.bytes(), 16);
            state.borrow_mut().fail_unmap = false;
        }
        assert!(state.borrow().live.is_empty());
    }
    #[test]
    fn byte_budget_is_restored_after_free() {
        let state = Rc::new(RefCell::new(State::default()));
        let mut heap = Heap::new(Map(state));
        let first = heap.calloc(1, MAX_ALLOCATION).unwrap();
        for _ in 1..MAX_TOTAL / MAX_ALLOCATION {
            heap.calloc(1, MAX_ALLOCATION).unwrap();
        }
        assert_eq!(heap.bytes(), MAX_TOTAL);
        assert_eq!(heap.calloc(1, 1), Err(Error::Exhausted));
        heap.free(first).unwrap();
        heap.calloc(1, MAX_ALLOCATION).unwrap();
        assert_eq!(heap.bytes(), MAX_TOTAL);
    }
    #[test]
    fn slot_budget_and_reuse() {
        let state = Rc::new(RefCell::new(State::default()));
        let mut heap = Heap::new(Map(state));
        let mut first = 0;
        for i in 0..MAX_SLOTS {
            let a = heap.calloc(1, 1).unwrap();
            if i == 0 {
                first = a;
            }
        }
        assert_eq!(heap.calloc(1, 1), Err(Error::Exhausted));
        heap.free(first).unwrap();
        heap.calloc(1, 1).unwrap();
        assert_eq!(heap.bytes(), MAX_SLOTS as u64 * 16);
    }
}
