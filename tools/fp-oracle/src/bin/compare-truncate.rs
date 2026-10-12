//! Independent masked CVTTSD2SI comparison against LLVM integer conversion.
//! Normalizes LLVM invalid-result values to x86 integer indefinite. No hardware
//! exception delivery or native lowering is established by this model check.
use native_ps5::scalar_fp::truncate_binary64_i32;
use rustc_apfloat::{Float, Round, Status, ieee::Double};

fn main() {
    let mut seed = 0x8324_d869_9a2c_451fu64;
    let mut count = 0u64;
    let edges = [
        0,
        1,
        0x000f_ffff_ffff_ffff,
        0x0010_0000_0000_0000,
        0x3ff8_0000_0000_0000,
        0x41df_ffff_ffff_ffff,
        0x41e0_0000_0000_0000,
        0xc1e0_0000_001f_ffff,
        0xc1e0_0000_0020_0000,
        0x7ff0_0000_0000_0000,
        0x7ff0_0000_0000_0001,
        0x7ff8_ffff_ffff_ffff,
        u64::MAX,
    ];
    for index in 0..50_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let bits = edges.get(index).copied().unwrap_or(seed);
        for daz in [false, true] {
            let subnormal = bits & 0x7ff0_0000_0000_0000 == 0;
            let input = if daz && subnormal {
                bits & (1 << 63)
            } else {
                bits
            };
            let mut exact = false;
            let expected =
                Double::from_bits(input as u128).to_i128_r(32, Round::TowardZero, &mut exact);
            let invalid = expected.status.contains(Status::INVALID_OP);
            let expected_bits = if invalid {
                0x8000_0000
            } else {
                expected.value as u32
            };
            let flags = if invalid {
                1
            } else if expected.status.contains(Status::INEXACT) {
                32
            } else {
                0
            };
            for rc in 0..4 {
                // Retain an unrelated sticky flag while changing RC and DAZ.
                let control = 0x1f84 | (rc << 13) | if daz { 0x40 } else { 0 };
                let actual =
                    truncate_binary64_i32(bits, control).expect("masked conversion must finish");
                assert_eq!(
                    actual.bits, expected_bits,
                    "input={bits:016x} rc={rc} daz={daz}"
                );
                assert_eq!(
                    actual.mxcsr,
                    control | flags,
                    "status input={bits:016x} rc={rc} daz={daz}"
                );
                count += 1;
            }
        }
    }
    println!(
        "PASS: {count} independent LLVM masked truncation comparisons including RC independence, DAZ, special values and sticky flags"
    );
}
