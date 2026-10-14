#![no_std]
/// # Safety
/// Pointer must address `count` readable, writable u64 elements; alignment is not required.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compiled_memory_guest(count: u64, pointer: *mut u64) -> u32 {
    let mut sum = 0u64;
    for n in 0..count {
        let p = unsafe { pointer.add(n as usize) };
        let value = unsafe { p.read_unaligned() }.wrapping_add(count);
        unsafe {
            p.write_unaligned(value ^ (value >> 13));
        }
        sum = sum.wrapping_add(value);
    }
    (sum ^ (sum >> 32)) as u32
}
