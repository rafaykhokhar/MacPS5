//! Finite masked multiplication with independent DAZ/FTZ and sticky-state checks.
use finite_fp_oracle::{quad_is_tiny, x86_overflow_flags, x86_tiny_flags};
use rustc_apfloat::{
    Float, FloatConvert, Round,
    ieee::{Double, Quad},
};

fn main() {
    let mut seed = 0x475e_a198_d7c3_250bu64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut count = 0usize;
    for index in 0..20_000 {
        let mut a = next();
        let mut b = next();
        if a >> 52 & 0x7ff == 0x7ff {
            a ^= 1 << 52;
        }
        if b >> 52 & 0x7ff == 0x7ff {
            b ^= 1 << 52;
        }
        if index % 3 == 0 {
            a &= !(0x7ff << 52);
        }
        if index % 5 == 0 {
            b &= !(0x7ff << 52);
        }
        for daz in [false, true] {
            for ftz in [false, true] {
                for rc in 0..4 {
                    let subnormal =
                        |x: u64| x & 0x7ff0_0000_0000_0000 == 0 && x & 0x000f_ffff_ffff_ffff != 0;
                    let input = |x: u64| {
                        if daz && subnormal(x) {
                            x & (1 << 63)
                        } else {
                            x
                        }
                    };
                    let (left, right) = (input(a), input(b));
                    let round = [
                        Round::NearestTiesToEven,
                        Round::TowardNegative,
                        Round::TowardPositive,
                        Round::TowardZero,
                    ][rc as usize];
                    let reference = Double::from_bits(left as u128)
                        .mul_r(Double::from_bits(right as u128), round);
                    let status = reference.status.bits() as u32;
                    let mut flags = (status & 1)
                        | ((status & 0x1e) << 1)
                        | if !daz && (subnormal(a) || subnormal(b)) {
                            2
                        } else {
                            0
                        };
                    flags =
                        x86_tiny_flags(left, right, rc, x86_overflow_flags(left, right, rc, flags));
                    let mut expected = reference.value.to_bits() as u64;
                    if ftz {
                        let qa: Quad = Double::from_bits(left as u128).convert(&mut false).value;
                        let qb: Quad = Double::from_bits(right as u128).convert(&mut false).value;
                        let product = qa.mul_r(qb, Round::NearestTiesToEven);
                        assert_eq!(product.status, rustc_apfloat::Status::OK);
                        if quad_is_tiny(product.value, rc) {
                            expected = (a ^ b) & (1 << 63);
                            flags |= 48;
                        }
                    }
                    // Keep an unrelated preexisting exception sticky bit.
                    let control = 0x1f84
                        | (rc << 13)
                        | if daz { 0x40 } else { 0 }
                        | if ftz { 0x8000 } else { 0 };
                    let actual = native_ps5::scalar_fp::multiply_binary64(a, b, control)
                        .expect("masked finite multiplication");
                    assert_eq!(
                        actual.bits, expected,
                        "a={a:016x} b={b:016x} rc={rc} daz={daz} ftz={ftz}"
                    );
                    assert_eq!(
                        actual.mxcsr,
                        control | flags,
                        "flags a={a:016x} b={b:016x} rc={rc} daz={daz} ftz={ftz}"
                    );
                    count += 1;
                }
            }
        }
    }
    println!(
        "PASS: {count} finite multiplication control combinations against independent LLVM/Quad policy"
    );
}
