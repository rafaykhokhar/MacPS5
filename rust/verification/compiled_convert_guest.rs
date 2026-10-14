#![no_std]
/// # Safety
/// Pointer addresses `count` readable, writable i64 elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compiled_convert_guest(count: u64, pointer: *mut i64) -> u32 {
    let mut checksum = 0u64;
    for n in 0..count {
        let p = unsafe { pointer.add(n as usize) };
        let bits = (unsafe { p.read_unaligned() } as f64).to_bits();
        unsafe { p.write_unaligned(bits as i64) };
        checksum ^= bits;
    }
    (checksum ^ (checksum >> 32)) as u32
}
