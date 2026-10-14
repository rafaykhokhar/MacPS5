#![no_std]
/// # Safety
/// Pointer addresses `count + 1` readable, writable f64 elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn compiled_float_guest(count: u64, pointer: *mut f64) -> u32 {
    let mut product = unsafe { pointer.read_unaligned() };
    for n in 0..count {
        let p = unsafe { pointer.add(n as usize + 1) };
        product *= unsafe { p.read_unaligned() };
        unsafe { p.write_unaligned(product) };
    }
    let bits = product.to_bits();
    (bits ^ (bits >> 32)) as u32
}
