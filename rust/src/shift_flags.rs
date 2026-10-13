//! SHL/SHR/SAR/ROL/ROR flags for 8-, 16-, 32- and 64-bit operands.
//! Undefined AF and multi-bit OF retain their incoming values. AMD APM Vol.3
//! Rev.3.31 pp.315/318/334 specifies CF as the last shifted-out bit, including
//! narrow counts at/beyond width. Rotate counts are masked before width reduction.
use crate::arithmetic::FlagsResult;
/// Rotate a width+carry ring. Undefined multi-bit OF retains its old value.
pub fn rotate_carry(old: u64, value: u64, raw: u64, bits: u32, right: bool) -> Option<(u64, u64)> {
    if ![8, 16, 32, 64].contains(&bits) {
        return None;
    }
    let masked = raw & if bits == 64 { 63 } else { 31 };
    let count = masked as u32 % (bits + 1);
    let mask = (1u128 << bits) - 1;
    let value = u128::from(value) & mask;
    if count == 0 {
        return Some((value as u64, old));
    }
    let width = bits + 1;
    let ring_mask = (1u128 << width) - 1;
    let ring = value | (u128::from(old & 1) << bits);
    let rotated = (if right {
        (ring >> count) | (ring << (width - count))
    } else {
        (ring << count) | (ring >> (width - count))
    }) & ring_mask;
    let result = (rotated & mask) as u64;
    let carry = (rotated >> bits) as u64;
    let mut flags = (old & !1) | carry;
    if masked == 1 {
        let overflow = if right {
            ((value as u64 >> (bits - 1)) ^ old) & 1
        } else {
            (result >> (bits - 1)) ^ carry
        };
        flags = (flags & !0x800) | (overflow << 11);
    }
    Some((result, flags))
}
pub fn flags(
    old: u64,
    original: u64,
    count: u64,
    result: u64,
    bits: u64,
    operation: u64,
) -> FlagsResult {
    if ![8, 16, 32, 64].contains(&bits) || operation > 4 {
        return FlagsResult {
            flags: old,
            fault: 3,
        };
    }
    let count = count & if bits == 64 { 63 } else { 31 };
    if count == 0 {
        return FlagsResult {
            flags: old,
            fault: 0,
        };
    }
    let mask = u64::MAX >> (64 - bits);
    let original = original & mask;
    let result = result & mask;
    let carry = match operation {
        0 | 1 if count > bits => 0,
        2 if count >= bits => original >> (bits - 1),
        0 => (original >> (bits - count)) & 1,
        1 | 2 => (original >> (count - 1)) & 1,
        3 => result & 1,
        _ => result >> (bits - 1),
    };
    let mut flags = (old & !1) | carry;
    if operation < 3 {
        flags &= !0xc4;
        if result == 0 {
            flags |= 0x40;
        }
        flags |= ((result >> (bits - 1)) & 1) << 7;
        if (result as u8).count_ones() % 2 == 0 {
            flags |= 4;
        }
    }
    if count == 1 {
        let overflow = match operation {
            0 | 3 => (result >> (bits - 1)) ^ carry,
            1 => original >> (bits - 1),
            2 => 0,
            _ => (result >> (bits - 1)) ^ (result >> (bits - 2)),
        } & 1;
        flags = (flags & !0x800) | (overflow << 11);
    }
    FlagsResult { flags, fault: 0 }
}
// Integer-only ABI: no pointers, allocation or host CPU flag dependencies.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn native_ps5_shift_flags_v1(
    old: u64,
    original: u64,
    count: u64,
    result: u64,
    bits: u64,
    operation: u64,
) -> FlagsResult {
    flags(old, original, count, result, bits, operation)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_bit_reference_covers_counts_widths_rotates_and_preserved_flags() {
        for bits in [8, 16, 32, 64] {
            let mask = u64::MAX >> (64 - bits);
            let sign = 1 << (bits - 1);
            for input in [
                0,
                1,
                2,
                mask,
                mask - 1,
                sign,
                sign - 1,
                0x55aa55aa55aa55aa,
                0xaa55aa55aa55aa55,
            ] {
                for operation in 0..5 {
                    for raw_count in 0..256 {
                        let count = raw_count & if bits == 64 { 63 } else { 31 };
                        let original = input & mask;
                        let mut value = original;
                        let mut carry = false;
                        for _ in 0..count {
                            match operation {
                                0 | 3 => {
                                    carry = value & sign != 0;
                                    value = (value << 1) & mask;
                                    if operation == 3 {
                                        value |= u64::from(carry);
                                    }
                                }
                                _ => {
                                    carry = value & 1 != 0;
                                    let top = value & sign;
                                    value >>= 1;
                                    if operation == 2 {
                                        value |= top;
                                    }
                                    if operation == 4 && carry {
                                        value |= sign;
                                    }
                                }
                            }
                        }
                        for old in [0, 2, 0x8d5, u64::MAX] {
                            let actual = flags(old, input, raw_count, value, bits, operation);
                            assert_eq!(actual.fault, 0);
                            if count == 0 {
                                assert_eq!(actual.flags, old);
                                continue;
                            }
                            let mut expected = (old & !1) | u64::from(carry);
                            if operation < 3 {
                                expected &= !0xc4;
                                if value == 0 {
                                    expected |= 0x40;
                                }
                                if value & sign != 0 {
                                    expected |= 0x80;
                                }
                                let parity = (0..8).filter(|bit| value & (1 << bit) != 0).count();
                                if parity % 2 == 0 {
                                    expected |= 4;
                                }
                            }
                            if count == 1 {
                                let overflow = match operation {
                                    0 | 3 => (original & sign != 0) != (value & sign != 0),
                                    1 => original & sign != 0,
                                    2 => false,
                                    _ => (original & 1 != 0) != (original & sign != 0),
                                };
                                expected = (expected & !0x800) | if overflow { 0x800 } else { 0 };
                            }
                            assert_eq!(
                                actual.flags, expected,
                                "bits={bits} op={operation} count={raw_count}"
                            );
                        }
                    }
                }
            }
        }
        for (bits, operation) in [(0, 0), (7, 0), (17, 0), (65, 0), (32, 5), (64, u64::MAX)] {
            assert_eq!(
                flags(0x123, 0, 0, 0, bits, operation),
                FlagsResult {
                    flags: 0x123,
                    fault: 3
                }
            );
        }
    }
}
