#![no_std]

/// Freestanding Rust workload compiled to x86-64, then run by the direct ARM64 backend.
#[unsafe(no_mangle)]
pub extern "C" fn compiled_integer_guest(mut value: u64, rounds: u64) -> u32 {
    for step in 0..rounds {
        value ^= step.wrapping_mul(0x9e3779b97f4a7c15);
        value = value.rotate_left((value >> 58) as u32);
        value = value.wrapping_mul(0xbf58476d1ce4e5b9);
        value ^= value >> 27;
    }
    (value ^ (value >> 32)) as u32
}
