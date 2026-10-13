//! Bounded, append-only executable storage for the direct ARM64 backend.
//! This does not decode or translate guest instructions.
#![allow(unsafe_code)]
use crate::Result;
use std::{cell::Cell, ffi::c_void, ptr::NonNull, rc::Rc};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    static OWNED: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" {
        fn mmap(
            address: *mut c_void,
            length: usize,
            protection: i32,
            flags: i32,
            fd: i32,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(address: *mut c_void, length: usize) -> i32;
        fn getpagesize() -> i32;
        fn pthread_jit_write_protect_supported_np() -> i32;
        fn pthread_jit_write_protect_np(enabled: i32);
        fn sys_icache_invalidate(start: *mut c_void, length: usize);
    }
    pub(super) fn allocate(capacity: usize) -> Result<NonNull<u8>> {
        let page = unsafe { getpagesize() };
        if page <= 0
            || capacity == 0
            || capacity % page as usize != 0
            || capacity > isize::MAX as usize
        {
            return Err("JIT capacity must be a positive whole number of host pages".into());
        }
        if unsafe { pthread_jit_write_protect_supported_np() } == 0 {
            return Err("Per-thread JIT write protection is unavailable".into());
        }
        if OWNED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("A JIT arena is already owned; share the existing arena".into());
        }
        // Darwin sys/mman.h: READ|WRITE|EXEC, PRIVATE|ANON|JIT. Per-thread
        // protection controls whether this thread can write or execute.
        let raw = unsafe {
            mmap(
                std::ptr::null_mut(),
                capacity,
                7,
                0x2 | 0x1000 | 0x800,
                -1,
                0,
            )
        };
        if raw as isize == -1 {
            let error = std::io::Error::last_os_error();
            OWNED.store(false, Ordering::Release);
            return Err(format!("MAP_JIT allocation failed: {error}"));
        }
        let Some(pointer) = NonNull::new(raw.cast()) else {
            unsafe {
                munmap(raw, capacity);
            }
            OWNED.store(false, Ordering::Release);
            return Err("MAP_JIT returned a null mapping".into());
        };
        unsafe {
            pthread_jit_write_protect_np(1);
        }
        Ok(pointer)
    }
    pub(super) unsafe fn write(destination: *mut u8, words: &[u32]) {
        struct Protect;
        impl Drop for Protect {
            fn drop(&mut self) {
                unsafe {
                    pthread_jit_write_protect_np(1);
                }
            }
        }
        unsafe {
            pthread_jit_write_protect_np(0);
        }
        let protect = Protect;
        // No allocation, user callback or fallible operation in the write window.
        unsafe {
            std::ptr::copy_nonoverlapping(
                words.as_ptr().cast::<u8>(),
                destination,
                std::mem::size_of_val(words),
            );
        }
        drop(protect);
        unsafe {
            sys_icache_invalidate(destination.cast(), std::mem::size_of_val(words));
        }
    }
    pub(super) unsafe fn release(pointer: NonNull<u8>, capacity: usize) {
        if unsafe { munmap(pointer.as_ptr().cast(), capacity) } == 0 {
            OWNED.store(false, Ordering::Release);
        }
        // On a failed unmap keep ownership locked: do not silently allocate
        // a second mapping beyond the caller's memory budget.
    }
}

struct Mapping {
    pointer: NonNull<u8>,
    capacity: usize,
}
impl Drop for Mapping {
    fn drop(&mut self) {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        unsafe {
            platform::release(self.pointer, self.capacity);
        }
    }
}

/// One host-thread-owned JIT region. Published allocations cannot be overwritten
/// or individually recycled. Rc ownership intentionally prevents Send and Sync.
pub struct JitArena {
    mapping: Rc<Mapping>,
    used: Cell<usize>,
}
impl JitArena {
    /// Allocate exactly `capacity` bytes, a positive multiple of the host page.
    ///
    /// # Safety
    /// The host must own JIT protection on this thread, use a signing policy
    /// permitting MAP_JIT and pthread_jit_write_protect_np, and coordinate with
    /// other JIT engines. JIT callbacks from signal handlers are not supported.
    pub unsafe fn new(capacity: usize) -> Result<Self> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let pointer = platform::allocate(capacity)?;
            Ok(Self {
                mapping: Rc::new(Mapping { pointer, capacity }),
                used: Cell::new(0),
            })
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = capacity;
            Err("JIT storage requires Apple Silicon macOS".into())
        }
    }
    pub fn capacity(&self) -> usize {
        self.mapping.capacity
    }
    pub fn used(&self) -> usize {
        self.used.get()
    }

    /// Publish fully relocated, trusted ARM64 code and flush its instruction cache.
    ///
    /// # Safety
    /// Words must be valid host code with valid targets and the expected ABI.
    /// No JIT code may execute on this thread during this call (including signal
    /// handlers). The host must retain any external targets/data used by the code.
    pub unsafe fn publish(&self, words: &[u32]) -> Result<JitCode> {
        let length = std::mem::size_of_val(words);
        let start = self.used.get();
        let end = checked_end(start, length, self.capacity())?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        unsafe {
            platform::write(self.mapping.pointer.as_ptr().add(start), words);
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("JIT storage requires Apple Silicon macOS".into());
        self.used.set(end);
        Ok(JitCode {
            mapping: self.mapping.clone(),
            start,
            length,
        })
    }
}

/// A published range pins the entire arena even after its allocator is dropped.
#[derive(Clone)]
pub struct JitCode {
    mapping: Rc<Mapping>,
    start: usize,
    length: usize,
}
impl JitCode {
    /// Returns an aligned entry within this allocation. Calling it is unsafe:
    /// the caller must establish the function ABI and keep this handle alive.
    pub fn entry(&self, byte_offset: usize) -> Result<*const c_void> {
        if byte_offset % 4 != 0 || byte_offset >= self.length {
            return Err("JIT entry must be an instruction within its allocation".into());
        }
        Ok(unsafe {
            self.mapping
                .pointer
                .as_ptr()
                .add(self.start + byte_offset)
                .cast()
        })
    }
}

fn checked_end(start: usize, length: usize, capacity: usize) -> Result<usize> {
    if length == 0 || length % 4 != 0 || start % 4 != 0 {
        return Err("JIT code must contain complete ARM64 instructions".into());
    }
    start
        .checked_add(length)
        .filter(|end| *end <= capacity)
        .ok_or_else(|| "JIT arena capacity exceeded".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_spans_reject_empty_unaligned_overflow_and_exhaustion() {
        assert_eq!(checked_end(4, 4, 8).unwrap(), 8);
        for (start, length, capacity) in [
            (0, 0, 8),
            (1, 4, 8),
            (0, 3, 8),
            (4, 8, 8),
            (usize::MAX - 3, 8, usize::MAX),
        ] {
            assert!(checked_end(start, length, capacity).is_err());
        }
    }
}
