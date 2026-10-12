//! Arithmetic status flags, independent of host pointers and the legacy CPU layout.
//! Operation numbers 0..3 preserve the existing translator ABI; 4 is SBB, 5 is DEC,
//! 6 is truncated IMUL and 7 is ADC (Intel SDM Vol.2A, ADC).
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub struct FlagsResult {
    pub flags: u64,
    pub fault: u64,
}

pub fn flags(old: u64, a: u64, b: u64, result: u64, bits: u64, operation: u64) -> FlagsResult {
    if ![8, 16, 32, 64].contains(&bits) || operation > 7 {
        return FlagsResult {
            flags: old,
            fault: 3,
        };
    }
    let mask = u64::MAX >> (64 - bits);
    let sign = 1u64 << (bits - 1);
    let a = a & mask;
    let b = b & mask;
    if operation == 6 {
        let signed = |value: u64| {
            if value & sign == 0 {
                value as i128
            } else {
                value as i128 - (1i128 << bits)
            }
        };
        let product = signed(a) * signed(b);
        let overflow = product < -(sign as i128) || product >= sign as i128;
        // IMUL defines CF/OF only. Preserve the other, architecturally undefined
        // arithmetic flags rather than borrowing ARM64 condition flags.
        return FlagsResult {
            flags: (old & !0x801) | 2 | if overflow { 0x801 } else { 0 },
            fault: 0,
        };
    }
    let result = result & mask;
    let mut flags = (old & !0x8d5) | 2;
    if result == 0 {
        flags |= 0x40;
    }
    if result & sign != 0 {
        flags |= 0x80;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    if operation != 2 {
        if (a ^ b ^ result) & 0x10 != 0 {
            flags |= 0x10;
        }
        let subtract = matches!(operation, 0 | 4 | 5);
        let overflow = if subtract {
            (a ^ b) & (a ^ result) & sign
        } else {
            (!(a ^ b)) & (a ^ result) & sign
        };
        if overflow != 0 {
            flags |= 0x800;
        }
        let carry = match operation {
            0 => a < b,
            4 => (a as u128) < b as u128 + (old & 1) as u128,
            7 => (a as u128 + b as u128 + (old & 1) as u128) > mask as u128,
            3 | 5 => old & 1 != 0,
            _ => (a as u128 + b as u128) > mask as u128,
        };
        if carry {
            flags |= 1;
        }
    }
    FlagsResult { flags, fault: 0 }
}

// The narrowly allowed unsafe attribute exports a unique C ABI symbol. This
// function has only integer arguments/returns: it never dereferences host or
// guest pointers, allocates, or panics for any input.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn native_ps5_arithmetic_flags_v1(
    old: u64,
    a: u64,
    b: u64,
    result: u64,
    bits: u64,
    operation: u64,
) -> FlagsResult {
    flags(old, a, b, result, bits, operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Independent signed/unsigned widened reference; no overflow bit formulas.
    fn reference(old: u64, a: u64, b: u64, bits: u32, subtract: bool, borrow: u64) -> (u64, u64) {
        let modulus = 1i128 << bits;
        let mask = (modulus - 1) as u64;
        let a = a & mask;
        let b = b & mask;
        let signed = |v: u64| {
            if v as i128 >= modulus / 2 {
                v as i128 - modulus
            } else {
                v as i128
            }
        };
        let exact = if subtract {
            a as i128 - b as i128 - borrow as i128
        } else {
            a as i128 + b as i128 + borrow as i128
        };
        let signed_result = if subtract {
            signed(a) - signed(b) - borrow as i128
        } else {
            signed(a) + signed(b) + borrow as i128
        };
        let result = exact.rem_euclid(modulus) as u64;
        let mut f = (old & !0x8d5) | 2;
        if exact < 0 || exact >= modulus {
            f |= 1;
        }
        let mut parity = 0;
        for bit in 0..8 {
            parity += ((result >> bit) & 1) as u8;
        }
        if parity % 2 == 0 {
            f |= 4;
        }
        let low = if subtract {
            (a & 15) as i32 - (b & 15) as i32 - borrow as i32
        } else {
            (a & 15) as i32 + (b & 15) as i32 + borrow as i32
        };
        if !(0..16).contains(&low) {
            f |= 16;
        }
        if result == 0 {
            f |= 64;
        }
        if result as i128 >= modulus / 2 {
            f |= 128;
        }
        if signed_result < -modulus / 2 || signed_result >= modulus / 2 {
            f |= 0x800;
        }
        (result, f)
    }
    #[test]
    fn exhaustive_byte_add_sub_and_borrow() {
        for a in 0..256 {
            for b in 0..256 {
                for (op, subtract, borrow) in [
                    (0, true, 0),
                    (1, false, 0),
                    (4, true, 0),
                    (4, true, 1),
                    (7, false, 0),
                    (7, false, 1),
                ] {
                    let old = 0xfedcba9876543ed6 | borrow;
                    let (result, expected) = reference(old, a, b, 8, subtract, borrow);
                    assert_eq!(
                        flags(old, a, b, result, 8, op),
                        FlagsResult {
                            flags: expected,
                            fault: 0
                        },
                        "a={a} b={b} op={op} borrow={borrow}"
                    );
                }
            }
        }
    }
    #[test]
    fn wide_boundaries_and_random_borrows() {
        let mut state = 0x48297abdecf01023u64;
        for bits in [16, 32, 64] {
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            let mut values = vec![0, 1, 15, 16, sign - 1, sign, sign + 1, mask - 1, mask];
            for _ in 0..128 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                values.push(state & mask);
            }
            for &a in &values {
                for &b in &values {
                    for carry in [0, 1] {
                        let old = 0xa5a50000 | carry;
                        let (result, expected) = reference(old, a, b, bits, true, carry);
                        assert_eq!(
                            flags(old, a, b, result, bits as u64, 4).flags,
                            expected,
                            "bits={bits} a={a:x} b={b:x} c={carry}"
                        );
                        let (result, expected) = reference(old, a, b, bits, false, carry);
                        assert_eq!(
                            flags(old, a, b, result, bits as u64, 7).flags,
                            expected,
                            "ADC bits={bits} a={a:x} b={b:x} c={carry}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn decrement_preserves_carry_at_all_widths() {
        for bits in [8, 16, 32, 64] {
            let mask = u64::MAX >> (64 - bits);
            let sign = 1u64 << (bits - 1);
            for value in [0, 1, 15, 16, sign - 1, sign, sign + 1, mask] {
                for carry in [0, 1] {
                    let old = 0xfedcba9876543ed6 | carry;
                    let (result, expected) = reference(old, value, 1, bits, true, 0);
                    assert_eq!(
                        flags(old, value, 1, result, bits.into(), 5),
                        FlagsResult {
                            flags: (expected & !1) | carry,
                            fault: 0,
                        }
                    );
                }
            }
        }
    }
    #[test]
    fn division_rejects_invalid_modes_and_preserves_existing_faults() {
        for (bits, signed, prior, expected) in [
            (8, 0, 0, 3),
            (0, 0, 0, 3),
            (65, 0, 0, 3),
            (64, 2, 0, 3),
            (64, 0, 9, 9),
        ] {
            let mut cpu = crate::legacy_cpu::Cpu {
                registers: [u64::MAX; 16],
                flags: 0xed7,
                fault: prior,
                ..Default::default()
            };
            divide(&mut cpu, 1, bits, signed);
            assert_eq!(cpu.registers, [u64::MAX; 16]);
            assert_eq!(cpu.flags, 0xed7);
            assert_eq!(cpu.fault, expected);
        }
        let mut cpu = crate::legacy_cpu::Cpu::default();
        cpu.registers[2] = 1 << 63;
        let registers = cpu.registers;
        divide(&mut cpu, u64::MAX, 64, 1); // i128::MIN / -1 must not panic.
        assert_eq!(cpu.fault, DIVIDE_ERROR);
        assert_eq!(cpu.registers, registers);
    }
    #[test]
    fn negation_inc_logic_and_invalid_inputs() {
        for v in 0..256 {
            let (result, expected) = reference(0x202, 0, v, 8, true, 0);
            assert_eq!(flags(0x202, 0, v, result, 8, 0).flags, expected);
        }
        for carry in [0, 1] {
            assert_eq!(flags(carry, 255, 1, 0, 8, 3).flags & 1, carry);
        }
        assert_eq!(flags(0xffff, 0, 0, 0, 8, 2).flags & 0x8d5, 0x44);
        for bits in [0, 1, 7, 9, 65, u64::MAX] {
            assert_eq!(
                flags(0x202, 1, 2, 3, bits, 0),
                FlagsResult {
                    flags: 0x202,
                    fault: 3
                }
            );
        }
        assert_eq!(flags(0x202, 1, 2, 3, 64, 8).fault, 3);
    }
}

/// Modeled x86 divide error (#DE): zero divisor or quotient out of range.
pub const DIVIDE_ERROR: u64 = 13;

/// Divide the double-width DX:AX / EDX:EAX / RDX:RAX value atomically.
/// Arithmetic flags are undefined by x86 DIV/IDIV and are preserved here.
pub fn divide(cpu: &mut crate::legacy_cpu::Cpu, divisor: u64, bits: u64, signed: u64) {
    if cpu.fault != 0 {
        return;
    }
    if ![16, 32, 64].contains(&bits) || signed > 1 {
        cpu.fault = 3;
        return;
    }
    let mask = u64::MAX >> (64 - bits);
    let divisor = divisor & mask;
    let dividend = ((cpu.registers[2] & mask) as u128) << bits | (cpu.registers[0] & mask) as u128;
    let output = if divisor == 0 {
        None
    } else if signed == 0 {
        let quotient = dividend / divisor as u128;
        if quotient > mask as u128 {
            None
        } else {
            Some((quotient as u64, (dividend % divisor as u128) as u64))
        }
    } else {
        let sign = 1u64 << (bits - 1);
        let divisor = if divisor & sign == 0 {
            divisor as i128
        } else {
            divisor as i128 - (1i128 << bits)
        };
        let shift = 128 - bits * 2;
        let dividend = ((dividend << shift) as i128) >> shift;
        dividend.checked_div(divisor).and_then(|quotient| {
            if quotient < -(sign as i128) || quotient >= sign as i128 {
                None
            } else {
                Some((quotient as u64 & mask, (dividend % divisor) as u64 & mask))
            }
        })
    };
    if let Some((quotient, remainder)) = output {
        for (index, value) in [(0, quotient), (2, remainder)] {
            cpu.registers[index] = if bits == 16 {
                (cpu.registers[index] & !mask) | value
            } else {
                value
            };
        }
    } else {
        cpu.fault = DIVIDE_ERROR;
    }
}

#[allow(unsafe_code)]
pub mod division_ffi {
    /// # Safety
    /// CPU must be a live exclusive borrow for this synchronous call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_divide_v1(
        cpu: *mut crate::legacy_cpu::Cpu,
        divisor: u64,
        bits: u64,
        signed: u64,
    ) {
        if let Some(cpu) = unsafe { cpu.as_mut() } {
            super::divide(cpu, divisor, bits, signed);
        }
    }
}
