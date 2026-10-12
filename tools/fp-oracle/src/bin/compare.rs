//! Compare the independent saved LLVM corpus to the experimental CPU engine.
//! Unsupported cases are counted separately, never counted as successes.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Provide corpus TSV")?;
    let mut accepted = 0usize;
    let mut mismatch = 0usize;
    let mut bit_mismatch = 0usize;
    let mut status_mismatch = 0usize;
    let mut raw_status_mismatch = 0usize;
    let mut normalized = 0usize;
    let mut tiny_normalized = 0usize;
    let mut status_pairs = BTreeMap::<(u32, u32), usize>::new();
    let mut unsupported = BTreeMap::<String, usize>::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 5 {
            return Err("Invalid corpus row".into());
        }
        let a = u64::from_str_radix(fields[0], 16)?;
        let b = u64::from_str_radix(fields[1], 16)?;
        let rc = fields[2].parse::<u32>()?;
        let expected = u64::from_str_radix(fields[3], 16)?;
        let status = u32::from_str_radix(fields[4], 16)?;
        let subnormal = |v: u64| v & 0x7ff0_0000_0000_0000 == 0 && v & 0x000f_ffff_ffff_ffff != 0;
        let expected_flags = (status & 1)
            | ((status & 0x1e) << 1)
            | if subnormal(a) || subnormal(b) { 2 } else { 0 };
        let raw_flags = expected_flags;
        let expected_flags = x86_overflow_flags(a, b, rc, raw_flags);
        let overflow_flags = expected_flags;
        let expected_flags = x86_tiny_flags(a, b, rc, overflow_flags);
        match native_ps5::scalar_fp::multiply_binary64(a, b, 0x1f80 | rc << 13) {
            Ok(result) => {
                accepted += 1;
                if result.mxcsr & 0x3f != raw_flags {
                    raw_status_mismatch += 1;
                }
                if overflow_flags != raw_flags {
                    normalized += 1;
                }
                if expected_flags != overflow_flags {
                    tiny_normalized += 1;
                }
                if result.bits != expected {
                    bit_mismatch += 1;
                }
                if result.mxcsr & 0x3f != expected_flags {
                    status_mismatch += 1;
                    *status_pairs
                        .entry((result.mxcsr & 0x3f, expected_flags))
                        .or_default() += 1;
                }
                if result.bits != expected || result.mxcsr & 0x3f != expected_flags {
                    mismatch += 1;
                    if mismatch <= 12 {
                        eprintln!(
                            "Mismatch a={a:016x} b={b:016x} rc={rc} got={:016x}/{:02x} expected={expected:016x}/{expected_flags:02x}",
                            result.bits,
                            result.mxcsr & 0x3f
                        );
                    }
                }
            }
            Err(reason) => {
                *unsupported.entry(format!("{reason:?}")).or_default() += 1;
            }
        }
    }
    println!(
        "Finite LLVM corpus: {accepted} accepted; {mismatch} mismatches; unsupported={unsupported:?}"
    );
    println!(
        "Result-bit mismatches: {bit_mismatch}; status mismatches: {status_mismatch}; (actual, expected) status counts: {status_pairs:?}"
    );
    println!(
        "Raw LLVM status mismatches: {raw_status_mismatch}; independently normalized x86 overflow cases: {normalized}; tiny cases: {tiny_normalized}"
    );
    if mismatch != 0 {
        return Err("Arithmetic/status comparison failed".into());
    }
    if accepted == 0 {
        return Err("No cases executed".into());
    }
    Ok(())
}

/// LLVM APFloat's overflow_result reports only INEXACT when directed rounding
/// saturates. x86 sets OE as well (Intel SDM vol.1 §4.9.1.4, table4-10):
/// https://cdrdv2-public.intel.com/843820/325462-sdm-vol-1-2abcd-3abcd-4-1.pdf
/// A saturated result alone does not prove overflow. For the modes directed
/// toward finite magnitude, rounding with unbounded exponent overflows iff the
/// exact magnitude reaches 2^1024. Quad's113-bit precision represents the full
/// 106-bit product exactly, including its complete binary64 exponent range.
fn x86_overflow_flags(a: u64, b: u64, rc: u32, flags: u32) -> u32 {
    use rustc_apfloat::{
        Float, FloatConvert, Round, Status,
        ieee::{Double, Quad},
    };
    let negative = (a ^ b) >> 63 != 0;
    let saturating = rc == 3 || (rc == 1 && !negative) || (rc == 2 && negative);
    if !saturating || flags & 8 != 0 {
        return flags;
    }
    let mut loses = false;
    let qa: Quad = Double::from_bits(a as u128).convert(&mut loses).value;
    assert!(!loses);
    let qb: Quad = Double::from_bits(b as u128).convert(&mut loses).value;
    assert!(!loses);
    let product = qa.mul_r(qb, Round::NearestTiesToEven);
    assert_eq!(
        product.status,
        Status::OK,
        "finite double product must fit Quad exactly"
    );
    let limit = Quad::from_bits((16383u128 + 1024) << 112);
    if product.value.abs() >= limit {
        flags | 8
    } else {
        flags
    }
}

#[cfg(test)]
mod tests {
    use super::x86_overflow_flags;
    #[test]
    fn saturation_is_not_enough_to_infer_overflow() {
        let max = 0x7fef_ffff_ffff_ffff;
        for rc in [1, 3] {
            assert_eq!(x86_overflow_flags(max, 0x4000_0000_0000_0000, rc, 32), 40);
            assert_eq!(x86_overflow_flags(max, 0x3ff0_0000_0000_0000, rc, 0), 0);
            // (2 - 2^-51) * (2^1023 + 2^971) = 2^1024 - 2^920.
            // Saturates to maxfinite with precision only, below true overflow.
            assert_eq!(
                x86_overflow_flags(0x3fff_ffff_ffff_fffe, 0x7fe0_0000_0000_0001, rc, 32),
                32
            );
            // Exactly2^1024 must overflow, not just values strictly above it.
            assert_eq!(
                x86_overflow_flags(0x4000_0000_0000_0000, 0x7fe0_0000_0000_0000, rc, 32),
                40
            );
        }
        assert_eq!(
            x86_overflow_flags(max | (1 << 63), 0x4000_0000_0000_0000, 2, 32),
            40
        );
        assert_eq!(x86_overflow_flags(max, 0x4000_0000_0000_0000, 0, 40), 40);
    }
}

/// Intel SDM vol.1 §4.9.1.5 defines tiny after destination-precision rounding
/// with unbounded exponent, before subnormal quantization. LLVM instead tests
/// its final encoding at the normal/subnormal boundary. Compute x86 tininess
/// independently: exact Quad product scaled by2^1022, then binary64 precision
/// rounding near1. Compare to1, preserving sign while applying directed modes.
/// https://cdrdv2-public.intel.com/843820/325462-sdm-vol-1-2abcd-3abcd-4-1.pdf
fn x86_tiny_flags(a: u64, b: u64, rc: u32, flags: u32) -> u32 {
    use rustc_apfloat::{
        Float, FloatConvert, Round, Status,
        ieee::{Double, Quad},
    };
    if flags & 32 == 0 {
        return flags;
    }
    let mut loses = false;
    let qa: Quad = Double::from_bits(a as u128).convert(&mut loses).value;
    assert!(!loses);
    let qb: Quad = Double::from_bits(b as u128).convert(&mut loses).value;
    assert!(!loses);
    let product = qa.mul_r(qb, Round::NearestTiesToEven);
    assert_eq!(product.status, Status::OK);
    if quad_is_tiny(product.value, rc) {
        flags | 16
    } else {
        flags & !16
    }
}

fn quad_is_tiny(product: rustc_apfloat::ieee::Quad, rc: u32) -> bool {
    use rustc_apfloat::{
        Float, FloatConvert, Round, Status,
        ieee::{Double, Quad},
    };
    if product.is_zero() {
        return false;
    }
    let scale = Quad::from_bits((16383u128 + 1022) << 112);
    let scaled = product.mul_r(scale, Round::NearestTiesToEven);
    assert_eq!(scaled.status, Status::OK);
    let round = [
        Round::NearestTiesToEven,
        Round::TowardNegative,
        Round::TowardPositive,
        Round::TowardZero,
    ][rc as usize];
    let rounded: Double = scaled.value.convert_r(round, &mut false).value;
    // Products far below the threshold can underflow even after scaling; zero
    // still means tiny. Overflow after scaling means not tiny.
    rounded.abs() < Double::from_bits(0x3ff0_0000_0000_0000)
}

#[cfg(test)]
mod tiny_tests {
    use super::{quad_is_tiny, x86_tiny_flags};
    use rustc_apfloat::{Float, Round, ieee::Quad};
    #[test]
    fn unbounded_precision_tininess_differs_from_final_encoding() {
        let minimum = 0x0010_0000_0000_0000;
        // Exactly midway below minimum normal: still tiny at53bit precision,
        // even when final nearest rounding encodes a normal.
        assert_eq!(x86_tiny_flags(minimum, 0x3fef_ffff_ffff_ffff, 0, 32), 48);
        assert_eq!(x86_tiny_flags(minimum, 0x3ff0_0000_0000_0000, 0, 0), 0);
        let normal = Quad::from_bits((16383u128 - 1022) << 112);
        let quarter = Quad::from_bits((16383u128 - 1076) << 112);
        let below = normal.sub_r(quarter, Round::NearestTiesToEven);
        assert_eq!(below.status, rustc_apfloat::Status::OK);
        // Quarterulp rounds to normal already at unbounded53bit precision.
        assert!(!quad_is_tiny(below.value, 0));
        assert!(quad_is_tiny(below.value, 1));
        assert!(!quad_is_tiny(below.value, 2));
        assert!(quad_is_tiny(below.value, 3));
        assert!(!quad_is_tiny(-below.value, 1));
        assert!(quad_is_tiny(-below.value, 2));
    }
}
