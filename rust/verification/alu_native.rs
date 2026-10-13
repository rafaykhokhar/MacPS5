//! Native instruction execution oracle, compiled by the Rust verification runner.
//! The ABI copy is deliberately checked against the translator's fixed offsets.
use native_ps5::legacy_cpu::Cpu;
use std::mem::{offset_of, size_of};
struct Case {
    block: unsafe extern "C" fn(*mut Cpu),
    bits: u32,
    dst: usize,
    dst_shift: u32,
    src: usize,
    src_shift: u32,
    immediate: Option<u64>,
    negate: bool,
    decrement: bool,
    end: u64,
}
struct MultiplyCase {
    block: unsafe extern "C" fn(*mut Cpu),
    bits: u32,
    dst: usize,
    src: usize,
    memory: bool,
    immediate: Option<u64>,
    end: u64,
}
struct DivideCase {
    block: unsafe extern "C" fn(*mut Cpu),
    bits: u32,
    source: usize,
    signed: bool,
    memory: bool,
    end: u64,
}
// Bit-serial unsigned division and signed magnitudes form an independent oracle;
// no host division or remainder instruction appears in this reference.
fn divide_reference(
    high: u64,
    low: u64,
    divisor: u64,
    bits: u32,
    signed: bool,
) -> Option<(u64, u64)> {
    let mask = u64::MAX >> (64 - bits);
    let wide_mask = u128::MAX >> (128 - 2 * bits);
    let numerator = ((high & mask) as u128) << bits | (low & mask) as u128;
    let divisor = divisor & mask;
    if divisor == 0 {
        return None;
    }
    let negative_numerator = signed && high & (1 << (bits - 1)) != 0;
    let negative_divisor = signed && divisor & (1 << (bits - 1)) != 0;
    let numerator = if negative_numerator {
        (!numerator).wrapping_add(1) & wide_mask
    } else {
        numerator
    };
    let divisor = if negative_divisor {
        (!divisor).wrapping_add(1) & mask
    } else {
        divisor
    } as u128;
    let (mut quotient, mut remainder) = (0u128, 0u128);
    for bit in (0..bits * 2).rev() {
        remainder = (remainder << 1) | ((numerator >> bit) & 1);
        if remainder >= divisor {
            remainder -= divisor;
            quotient |= 1u128 << bit;
        }
    }
    let negative_quotient = negative_numerator != negative_divisor;
    let limit = if !signed {
        mask as u128
    } else if negative_quotient {
        1u128 << (bits - 1)
    } else {
        (1u128 << (bits - 1)) - 1
    };
    if quotient > limit {
        return None;
    }
    Some((
        if negative_quotient {
            (0u64.wrapping_sub(quotient as u64)) & mask
        } else {
            quotient as u64
        },
        if negative_numerator {
            (0u64.wrapping_sub(remainder as u64)) & mask
        } else {
            remainder as u64
        },
    ))
}
struct WordCase {
    block: unsafe extern "C" fn(*mut Cpu),
    op: u8,
    location: u8,
    src: usize,
    shift: u32,
    immediate: Option<u64>,
    end: u64,
}
fn word_flags(old: u64, a: u16, b: u16, result: u16, op: u8) -> u64 {
    if matches!(op, 0 | 8 | 10 | 11 | 12) {
        return old;
    }
    let mut flags = (old & !0x8d5) | 2;
    if result == 0 {
        flags |= 64;
    }
    if result & 0x8000 != 0 {
        flags |= 128;
    }
    if (result as u8).count_ones() % 2 == 0 {
        flags |= 4;
    }
    if matches!(op, 1 | 2 | 3 | 9) {
        let subtract = matches!(op, 2 | 3);
        let signed_result = if subtract {
            (a as i16) as i32 - (b as i16) as i32
        } else {
            (a as i16) as i32 + (b as i16) as i32
        };
        if !(-32768..=32767).contains(&signed_result) {
            flags |= 0x800;
        }
        let carry = if op == 9 {
            old & 1 != 0
        } else if subtract {
            a < b
        } else {
            a as u32 + b as u32 > 65535
        };
        if carry {
            flags |= 1;
        }
        if if subtract {
            a & 15 < b & 15
        } else {
            (a & 15) + (b & 15) > 15
        } {
            flags |= 16;
        }
    }
    flags
}
include!(env!("NATIVE_ALU_FIXTURES"));
fn reference(old: u64, a: u64, b: u64, bits: u32, borrow: u64) -> (u64, u64) {
    let modulus = 1i128 << bits;
    let mask = (modulus - 1) as u64;
    let a = a & mask;
    let b = b & mask;
    let signed = |x: u64| {
        if x as i128 >= modulus / 2 {
            x as i128 - modulus
        } else {
            x as i128
        }
    };
    let exact = a as i128 - b as i128 - borrow as i128;
    let result = exact.rem_euclid(modulus) as u64;
    let signed_result = signed(a) - signed(b) - borrow as i128;
    let mut flags = (old & !0x8d5) | 2;
    if exact < 0 {
        flags |= 1;
    }
    let ones = (0..8).map(|bit| (result >> bit) & 1).sum::<u64>();
    if ones % 2 == 0 {
        flags |= 4;
    }
    if (a & 15) < (b & 15) + borrow {
        flags |= 16;
    }
    if result == 0 {
        flags |= 64;
    }
    if result as i128 >= modulus / 2 {
        flags |= 128;
    }
    if signed_result < -modulus / 2 || signed_result >= modulus / 2 {
        flags |= 0x800;
    }
    (result, flags)
}
fn main() {
    assert_eq!(size_of::<Cpu>(), 816);
    assert_eq!(offset_of!(Cpu, flags), 136);
    assert_eq!(offset_of!(Cpu, fault), 152);
    assert_eq!(offset_of!(Cpu, vectors), 208);
    // Forces the runtime crate into the link; assembly calls its exported ABI.
    assert_eq!(
        native_ps5::arithmetic::native_ps5_arithmetic_flags_v1(2, 0, 0, 0, 8, 0).flags,
        0x46
    );
    let mut checks = 0;
    let mut rng = 0x2598acd72645bb13u64;
    for case in CASES {
        let mask = u64::MAX >> (64 - case.bits);
        let sign = 1u64 << (case.bits - 1);
        let values: Vec<_> = if case.bits == 8 {
            (0..256).collect()
        } else {
            let mut v = vec![0, 1, 15, 16, sign - 1, sign, sign + 1, mask - 1, mask];
            for _ in 0..128 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                v.push(rng & mask);
            }
            v
        };
        for &a in &values {
            for &b in &values {
                for carry in [0, 1] {
                    let mut cpu = Cpu {
                        registers: std::array::from_fn(|i| 0xdeadbeefbead0000 | i as u64),
                        rip: 0,
                        flags: 0x602 | carry,
                        bias: 0,
                        fault: 0,
                        fuel: 100,
                        stop: 0,
                        target: 0,
                        memory: 0,
                        next_rip: 0,
                        mxcsr: 0,
                        x87_control: 0x037f,
                        x87_status: 0,
                        vectors: [[0; 32]; 16],
                        x87: Default::default(),
                    };
                    // Install operands before deriving expected results, so aliases and
                    // AH/AL overlap have their architectural meaning.
                    cpu.registers[case.dst] = (cpu.registers[case.dst] & !(mask << case.dst_shift))
                        | (a << case.dst_shift);
                    if case.immediate.is_none() && !case.negate {
                        cpu.registers[case.src] = (cpu.registers[case.src]
                            & !(mask << case.src_shift))
                            | (b << case.src_shift);
                    }
                    let lhs = (cpu.registers[case.dst] >> case.dst_shift) & mask;
                    let rhs = case
                        .immediate
                        .unwrap_or(cpu.registers[case.src] >> case.src_shift)
                        & mask;
                    let (result, flags) = if case.negate {
                        reference(cpu.flags, 0, lhs, case.bits, 0)
                    } else if case.decrement {
                        let (result, flags) = reference(cpu.flags, lhs, 1, case.bits, 0);
                        (result, (flags & !1) | carry)
                    } else {
                        reference(cpu.flags, lhs, rhs, case.bits, carry)
                    };
                    let mut expected = cpu.registers;
                    expected[case.dst] = if case.bits == 32 {
                        result
                    } else {
                        (expected[case.dst] & !(mask << case.dst_shift))
                            | (result << case.dst_shift)
                    };
                    // SAFETY: each function is emitted for this checked repr(C) layout,
                    // has no memory operands, and receives a live exclusive Cpu pointer.
                    unsafe { (case.block)(&mut cpu) };
                    assert_eq!(
                        cpu.registers, expected,
                        "bits={} a={a:x} b={b:x} carry={carry}",
                        case.bits
                    );
                    assert_eq!(cpu.flags, flags);
                    assert_eq!(cpu.fault, 0);
                    assert_eq!(cpu.fuel, 98);
                    assert_eq!(cpu.stop, 3);
                    assert_eq!(cpu.rip, case.end);
                    checks += 1;
                }
            }
        }
    }
    for &(block, bits, end) in MEMORY_CASES {
        let mask = u64::MAX >> (64 - bits);
        let sign = 1u64 << (bits - 1);
        for value in [0, 1, 15, 16, sign - 1, sign, sign + 1, mask] {
            for carry in [0, 1] {
                // Valid, read-only, wholly absent, and boundary-straddling operands.
                for mode in 0..4 {
                    let mut data = [0xa5; 16];
                    data[4..12].copy_from_slice(&value.to_le_bytes());
                    let before = data;
                    let mut memory = native_ps5::guest_memory::Memory::default();
                    let length = if mode == 3 {
                        4 + bits as usize / 8 - 1
                    } else {
                        16
                    };
                    memory
                        .add(0x1000, &mut data[..length], mode != 1, false)
                        .unwrap();
                    let mut cpu = Cpu {
                        fuel: 100,
                        flags: 0x602 | carry,
                        ..Cpu::default()
                    };
                    let address = if mode == 2 { 0x2000 } else { 0x1004 };
                    cpu.registers[7] = address - 0x448;
                    let registers = cpu.registers;
                    cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                    // SAFETY: generated code uses the checked Cpu ABI; memory and its
                    // exclusive backing remain live through this synchronous invocation.
                    unsafe { block(&mut cpu) };
                    drop(memory);
                    assert_eq!(cpu.registers, registers);
                    if mode == 0 {
                        let (result, flags) = reference(0x602 | carry, value, 1, bits, 0);
                        let mut expected = before;
                        expected[4..4 + bits as usize / 8]
                            .copy_from_slice(&result.to_le_bytes()[..bits as usize / 8]);
                        assert_eq!(data, expected);
                        assert_eq!(cpu.flags, (flags & !1) | carry);
                        assert_eq!((cpu.fault, cpu.stop, cpu.rip, cpu.fuel), (0, 3, end, 98));
                    } else {
                        assert_eq!(data, before);
                        assert_eq!(cpu.flags, 0x602 | carry);
                        assert_eq!(cpu.fault, if mode == 1 { 2 } else { 1 });
                        assert_eq!((cpu.rip, cpu.fuel), (0, 99));
                    }
                    checks += 1;
                }
            }
        }
    }
    let mut jump_checks = 0;
    for &(block, source, indirect, relative) in JUMP_CASES {
        for target in [0, 1, 0x1000, 0x9000, 0x10006da0, u64::MAX] {
            for fuel in [0, 1, 100] {
                for mode in 0..if indirect { 3 } else { 1 } {
                    let mut data = target.to_le_bytes();
                    let mut memory = native_ps5::guest_memory::Memory::default();
                    let pointer = if relative { 0x9000 } else { 0x9001 };
                    memory
                        .add(
                            if mode == 2 { 0xa000 } else { pointer },
                            &mut data[..if mode == 1 { 7 } else { 8 }],
                            false,
                            false,
                        )
                        .unwrap();
                    let mut cpu = Cpu {
                        registers: std::array::from_fn(|i| 0xaabbccdd00001000 | i as u64),
                        vectors: [[0x5a; 32]; 16],
                        flags: 0xed7,
                        bias: 0x8000,
                        fuel,
                        target: 0xdead,
                        next_rip: 0xbeef,
                        ..Cpu::default()
                    };
                    cpu.registers[source] = if indirect { pointer } else { target };
                    let registers = cpu.registers;
                    cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                    // SAFETY: generated block uses checked Cpu/Memory ABI; live
                    // exclusive storage remains valid throughout this synchronous call.
                    unsafe { block(&mut cpu) };
                    assert_eq!(cpu.registers, registers); // Includes RSP: tail jumps never push.
                    assert_eq!(cpu.vectors, [[0x5a; 32]; 16]);
                    assert_eq!(
                        (cpu.flags, cpu.bias, cpu.rip, cpu.next_rip),
                        (0xed7, 0x8000, 0, 0xbeef)
                    );
                    if fuel == 0 {
                        assert_eq!(
                            (cpu.stop, cpu.fault, cpu.target, cpu.fuel),
                            (4, 0, 0xdead, 0)
                        );
                    } else if mode != 0 {
                        assert_eq!(
                            (cpu.stop, cpu.fault, cpu.target, cpu.fuel),
                            (0, 1, 0xdead, fuel - 1)
                        );
                    } else {
                        assert_eq!(
                            (cpu.stop, cpu.fault, cpu.target, cpu.fuel),
                            (5, 0, target, fuel - 1)
                        );
                    }
                    drop(memory);
                    assert_eq!(data, target.to_le_bytes());
                    jump_checks += 1;
                }
            }
        }
    }
    println!(
        "PASS: native indirect JMP register/RSP/R15, actual FF27 and RIP-relative memory; absolute tail targets, no guest-frame writes, precise memory/fuel stops; {jump_checks} checks"
    );
    let mut word_checks = 0;
    let mut values = vec![
        0u16, 1, 15, 16, 127, 128, 255, 256, 32767, 32768, 32769, 65534, 65535,
    ];
    for _ in 0..8 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        values.push(rng as u16);
    }
    for case in WORD_CASES {
        let size = if matches!(case.op, 10 | 11) { 1 } else { 2 };
        let writes = !matches!(case.op, 3 | 4);
        for &a in &values {
            for &b in &values {
                for old_flags in [0x202, 0xed7] {
                    for mode in 0..if case.location == 0 { 1 } else { 3 } {
                        let mut bytes = if case.location == 1 {
                            b.to_le_bytes()
                        } else {
                            a.to_le_bytes()
                        };
                        let before = bytes;
                        let mut memory = native_ps5::guest_memory::Memory::default();
                        memory
                            .add(0x1001, &mut bytes[..size], mode != 1, false)
                            .unwrap();
                        let mut cpu = Cpu {
                            registers: [0xaabbccddeeff0000; 16],
                            flags: old_flags,
                            fuel: 100,
                            ..Cpu::default()
                        };
                        cpu.registers[0] |= a as u64;
                        let src_mask = if matches!(case.op, 10 | 11) {
                            255
                        } else {
                            65535
                        };
                        cpu.registers[case.src] = (cpu.registers[case.src]
                            & !(src_mask << case.shift))
                            | ((b as u64 & src_mask) << case.shift);
                        cpu.registers[7] = if mode == 2 { 0x1002 } else { 0x1001 };
                        let lhs = if case.location == 2 {
                            a
                        } else {
                            cpu.registers[0] as u16
                        };
                        let rhs = case.immediate.unwrap_or(if case.location == 1 {
                            b as u64
                        } else {
                            cpu.registers[case.src] >> case.shift
                        }) as u16;
                        let rhs = if case.op == 9 { 1 } else { rhs };
                        let result = match case.op {
                            0 => rhs,
                            1 | 9 => lhs.wrapping_add(rhs),
                            2 | 3 => lhs.wrapping_sub(rhs),
                            4 | 5 => lhs & rhs,
                            6 => lhs | rhs,
                            7 => lhs ^ rhs,
                            8 => !lhs,
                            10 => rhs as u8 as u16,
                            11 => rhs as u8 as i8 as i16 as u16,
                            12 => cpu.registers[7]
                                .wrapping_add(cpu.registers[6].wrapping_mul(2))
                                .wrapping_add(0x34) as u16,
                            _ => unreachable!(),
                        };
                        let fault = if case.location != 0 && mode == 2 {
                            if case.location == 2 && case.op == 0 {
                                2
                            } else {
                                1
                            }
                        } else if case.location == 2 && mode == 1 && writes {
                            2
                        } else {
                            0
                        };
                        let mut expected_regs = cpu.registers;
                        let mut expected_bytes = before;
                        if fault == 0 && writes {
                            if case.location == 2 {
                                expected_bytes = result.to_le_bytes();
                            } else {
                                expected_regs[0] = (expected_regs[0] & !65535) | result as u64;
                            }
                        }
                        let flags = if fault == 0 {
                            word_flags(old_flags, lhs, rhs, result, case.op)
                        } else {
                            old_flags
                        };
                        cpu.memory =
                            (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                        // SAFETY: checked Cpu and exclusive Memory remain live for
                        // this synchronous generated ARM64 instruction sequence.
                        unsafe { (case.block)(&mut cpu) };
                        assert_eq!(
                            cpu.registers, expected_regs,
                            "op={} loc={} a={a:x} b={b:x} mode={mode}",
                            case.op, case.location
                        );
                        assert_eq!(cpu.flags, flags, "op={} a={a:x} b={b:x}", case.op);
                        assert_eq!(cpu.fault, fault);
                        assert_eq!(cpu.rip, if fault == 0 { case.end } else { 0 });
                        assert_eq!(cpu.fuel, if fault == 0 { 98 } else { 99 });
                        drop(memory);
                        assert_eq!(bytes, expected_bytes);
                        word_checks += 1;
                    }
                }
            }
        }
    }
    println!(
        "PASS: native 16-bit scalar/arithmetic operations, partial registers, extension, LEA, flags, immediates and guarded memory; {word_checks} checks"
    );
    let mut divide_checks = 0;
    for case in DIVIDE_CASES {
        let mask = u64::MAX >> (64 - case.bits);
        let sign = 1u64 << (case.bits - 1);
        let size = case.bits as usize / 8;
        let mut values = vec![0, 1, 2, 3, 7, sign - 1, sign, sign + 1, mask - 1, mask];
        for _ in 0..8 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            values.push(rng & mask);
        }
        for &high in &values {
            for &low in &values {
                for &divisor in &values {
                    let mut bytes = divisor.to_le_bytes();
                    let mut memory = native_ps5::guest_memory::Memory::default();
                    memory
                        .add(0x1001, &mut bytes[..size], false, false)
                        .unwrap();
                    let mut cpu = Cpu {
                        registers: [0xaabbccddeeff0000; 16],
                        flags: 0xed7,
                        fuel: 100,
                        ..Cpu::default()
                    };
                    cpu.registers[0] = (cpu.registers[0] & !mask) | low;
                    cpu.registers[2] = (cpu.registers[2] & !mask) | high;
                    if case.memory {
                        cpu.registers[7] = 0x1001;
                    } else {
                        cpu.registers[case.source] = (cpu.registers[case.source] & !mask) | divisor;
                    }
                    let expected_division = divide_reference(
                        cpu.registers[2],
                        cpu.registers[0],
                        divisor,
                        case.bits,
                        case.signed,
                    );
                    let mut expected = cpu.registers;
                    if let Some((quotient, remainder)) = expected_division {
                        for (reg, value) in [(0, quotient), (2, remainder)] {
                            expected[reg] = if case.bits == 16 {
                                (expected[reg] & !mask) | value
                            } else {
                                value
                            };
                        }
                    }
                    cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                    // SAFETY: checked Cpu and Memory layouts stay exclusive and live
                    // for this synchronous generated native block invocation.
                    unsafe { (case.block)(&mut cpu) };
                    assert_eq!(
                        cpu.registers, expected,
                        "bits={} signed={} source={} h={high:x} l={low:x} d={divisor:x}",
                        case.bits, case.signed, case.source
                    );
                    assert_eq!(cpu.flags, 0xed7);
                    assert_eq!(
                        cpu.fault,
                        if expected_division.is_some() {
                            0
                        } else {
                            native_ps5::arithmetic::DIVIDE_ERROR
                        }
                    );
                    assert_eq!(
                        cpu.rip,
                        if expected_division.is_some() {
                            case.end
                        } else {
                            0
                        }
                    );
                    assert_eq!(cpu.fuel, if expected_division.is_some() { 98 } else { 99 });
                    drop(memory);
                    assert_eq!(bytes, divisor.to_le_bytes());
                    divide_checks += 1;
                }
            }
        }
        if case.memory {
            for address in [0x1000, 0x1002, 0x2000] {
                let mut bytes = [0u8; 8]; // Memory fault takes priority over #DE.
                let mut memory = native_ps5::guest_memory::Memory::default();
                memory
                    .add(0x1001, &mut bytes[..size], false, false)
                    .unwrap();
                let mut cpu = Cpu {
                    registers: [0xdeadbeef; 16],
                    flags: 0xed7,
                    fuel: 100,
                    ..Cpu::default()
                };
                cpu.registers[7] = address;
                let expected = cpu.registers;
                cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                // SAFETY: same exclusive synchronous ABI with deliberately invalid guest operands.
                unsafe { (case.block)(&mut cpu) };
                assert_eq!(cpu.registers, expected);
                assert_eq!(cpu.flags, 0xed7);
                assert_eq!((cpu.fault, cpu.rip, cpu.fuel), (1, 0, 99));
                divide_checks += 1;
            }
        }
        let mut cpu = Cpu {
            flags: 0xed7,
            ..Cpu::default()
        };
        // SAFETY: checked Cpu layout; zero fuel must stop before any memory/helper call.
        unsafe { (case.block)(&mut cpu) };
        assert_eq!(cpu.registers, [0; 16]);
        assert_eq!(
            (cpu.fault, cpu.flags, cpu.stop, cpu.fuel, cpu.rip),
            (0, 0xed7, 4, 0, 0)
        );
        divide_checks += 1;
    }
    println!(
        "PASS: native DIV/IDIV16/32/64 against bit-serial oracle; quotient/remainder, signed limits, RAX/RDX aliases, guarded faults and no commit on divide error; {divide_checks} checks"
    );
    let mut multiply_checks = 0;
    for case in MULTIPLY_CASES {
        let mask = u64::MAX >> (64 - case.bits);
        let sign = 1u64 << (case.bits - 1);
        let size = case.bits as usize / 8;
        let mut values = vec![0, 1, 2, 15, 16, sign - 1, sign, sign + 1, mask - 1, mask];
        for _ in 0..16 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            values.push(rng & mask);
        }
        for &a in &values {
            for &b in &values {
                for old_flags in [2, 0xed7] {
                    let mut bytes = b.to_le_bytes();
                    let mut memory = native_ps5::guest_memory::Memory::default();
                    memory
                        .add(0x1001, &mut bytes[..size], false, false)
                        .unwrap();
                    let mut cpu = Cpu {
                        registers: std::array::from_fn(|i| 0xaabbccddeeff0000 | i as u64),
                        fuel: 100,
                        flags: old_flags,
                        ..Cpu::default()
                    };
                    cpu.registers[case.dst] = (cpu.registers[case.dst] & !mask) | a;
                    if !case.memory {
                        cpu.registers[case.src] = (cpu.registers[case.src] & !mask) | b;
                    }
                    cpu.registers[7] = 0x1001;
                    cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                    let source = if case.memory {
                        b
                    } else {
                        cpu.registers[case.src] & mask
                    };
                    let lhs = if case.immediate.is_some() {
                        source
                    } else {
                        cpu.registers[case.dst] & mask
                    };
                    let rhs = case.immediate.unwrap_or(source) & mask;
                    // Independent unsigned-magnitude oracle handles signed overflow.
                    let magnitude = |v: u64| {
                        if v & sign != 0 {
                            ((!v).wrapping_add(1) & mask) as u128
                        } else {
                            v as u128
                        }
                    };
                    let negative = (lhs ^ rhs) & sign != 0;
                    let product = magnitude(lhs) * magnitude(rhs);
                    let result = (if negative {
                        0u128.wrapping_sub(product)
                    } else {
                        product
                    }) as u64
                        & mask;
                    let overflow = product
                        > if negative {
                            sign as u128
                        } else {
                            (sign - 1) as u128
                        };
                    let mut expected = cpu.registers;
                    expected[case.dst] = if case.bits == 16 {
                        (expected[case.dst] & !mask) | result
                    } else {
                        result
                    };
                    // SAFETY: checked Cpu layout and exclusive borrowed Memory remain
                    // live through the generated ARM64 block's synchronous invocation.
                    unsafe { (case.block)(&mut cpu) };
                    assert_eq!(cpu.registers, expected);
                    assert_eq!(
                        cpu.flags,
                        (old_flags & !0x801) | if overflow { 0x801 } else { 0 }
                    );
                    assert_eq!(
                        (cpu.fault, cpu.rip, cpu.stop, cpu.fuel),
                        (0, case.end, 3, 98)
                    );
                    drop(memory);
                    assert_eq!(bytes, b.to_le_bytes());
                    multiply_checks += 1;
                }
            }
        }
        if case.memory {
            for address in [0x1000, 0x1002, 0x2000] {
                let mut bytes = [0x5a; 8];
                let mut memory = native_ps5::guest_memory::Memory::default();
                memory
                    .add(0x1001, &mut bytes[..size], false, false)
                    .unwrap();
                let mut cpu = Cpu {
                    registers: [0xdeadbeef; 16],
                    flags: 0xed7,
                    fuel: 100,
                    ..Cpu::default()
                };
                cpu.registers[7] = address;
                let before = cpu.registers;
                cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                // SAFETY: same synchronous checked ABI; guest addresses are deliberately invalid.
                unsafe { (case.block)(&mut cpu) };
                assert_eq!(cpu.registers, before);
                assert_eq!(cpu.flags, 0xed7);
                assert_eq!((cpu.fault, cpu.rip, cpu.fuel), (1, 0, 99));
                multiply_checks += 1;
            }
        }
    }
    println!(
        "PASS: native two/three-operand IMUL16/32/64 signed results and overflow, immediate extension, aliases, partial registers and guarded loads; {multiply_checks} checks"
    );
    let mut vector_checks = 0;
    for &(block, all) in ZERO_CASES {
        for fuel in [0, 1, 2, 100] {
            for flags in [2, 0x202, 0xed7] {
                let mut cpu = Cpu {
                    registers: std::array::from_fn(|i| 0xaabbccdd00110000 | i as u64),
                    vectors: std::array::from_fn(|i| {
                        std::array::from_fn(|b| (i * 32 + b + 1) as u8)
                    }),
                    flags,
                    fuel,
                    ..Cpu::default()
                };
                let registers = cpu.registers;
                let mut expected = cpu.vectors;
                if fuel > 0 {
                    for vector in &mut expected {
                        vector[if all { 0 } else { 16 }..].fill(0);
                    }
                }
                // SAFETY: generated register-only code receives its checked, live Cpu ABI.
                unsafe { block(&mut cpu) };
                assert_eq!(cpu.registers, registers);
                assert_eq!(cpu.vectors, expected);
                assert_eq!(cpu.flags, flags);
                assert_eq!(cpu.fault, 0);
                assert_eq!(cpu.fuel, fuel.saturating_sub(2));
                assert_eq!(cpu.stop, if fuel < 2 { 4 } else { 3 });
                assert_eq!(cpu.rip, if fuel == 0 { 0 } else { 3 });
                vector_checks += 1;
            }
        }
    }
    for &(block, size, aligned, kind, dst, src) in VECTOR_CASES {
        for mode in 0..if kind == 0 { 1 } else { 5 } {
            for flags in [2, 0x202, 0xed7] {
                let mut data: [u8; 96] = std::array::from_fn(|i| (i * 3 + 11) as u8);
                let original = data;
                let mut memory = native_ps5::guest_memory::Memory::default();
                memory.add(0x1000, &mut data, mode != 2, false).unwrap();
                let offset = match mode {
                    1 => 33,
                    4 => 96 - size + 1,
                    _ => 32,
                };
                let mut cpu = Cpu {
                    registers: std::array::from_fn(|i| 0xaabbccdd00110000 | i as u64),
                    vectors: std::array::from_fn(|i| {
                        std::array::from_fn(|b| (i * 7 + b + 1) as u8)
                    }),
                    flags,
                    fuel: 100,
                    ..Cpu::default()
                };
                cpu.registers[7] = if mode == 3 {
                    0x2000
                } else {
                    0x1000 + offset as u64
                };
                cpu.memory = (&mut memory as *mut native_ps5::guest_memory::Memory<'_>) as u64;
                let registers = cpu.registers;
                let mut expected_vectors = cpu.vectors;
                let mut expected_data = original;
                let fail = kind != 0
                    && (mode == 3
                        || mode == 4
                        || (aligned && mode == 1)
                        || (kind == 2 && mode == 2));
                if !fail {
                    if kind == 0 {
                        expected_vectors[dst][..size].copy_from_slice(&cpu.vectors[src][..size]);
                        expected_vectors[dst][size..].fill(0);
                    } else if kind == 1 {
                        expected_vectors[dst][..size]
                            .copy_from_slice(&original[offset..offset + size]);
                        expected_vectors[dst][size..].fill(0);
                    } else {
                        expected_data[offset..offset + size]
                            .copy_from_slice(&cpu.vectors[dst][..size]);
                    }
                }
                // SAFETY: generated code uses the Rust guarded Memory ABI, and both
                // exclusive Cpu/Memory and backing storage remain live through the call.
                unsafe { block(&mut cpu) };
                drop(memory);
                assert_eq!(cpu.registers, registers);
                assert_eq!(
                    cpu.vectors, expected_vectors,
                    "size={size} aligned={aligned} kind={kind} mode={mode}"
                );
                assert_eq!(data, expected_data);
                assert_eq!(cpu.flags, flags);
                assert_eq!(
                    cpu.fault,
                    if fail {
                        if kind == 2 { 2 } else { 1 }
                    } else {
                        0
                    }
                );
                assert_eq!(cpu.rip, if fail { 0 } else { 5 });
                assert_eq!(cpu.fuel, if fail { 99 } else { 98 });
                vector_checks += 1;
            }
        }
    }
    println!(
        "PASS: native VZEROUPPER/VZEROALL, VEX128/256 VMOVDQA/VMOVDQU aliases, alignment, guarded memory and precise fuel; {vector_checks} checks"
    );
    let mut extension_checks = 0;
    for &(block, source_bits, destination_bits, destination, end) in EXTEND_CASES {
        let source_mask = u64::MAX >> (64 - source_bits);
        let destination_mask = u64::MAX >> (64 - destination_bits);
        let sign = 1u64 << (source_bits - 1);
        let mut values: Vec<u64> = if source_bits <= 16 {
            (0..=source_mask).collect()
        } else {
            vec![0, 1, sign - 1, sign, sign + 1, source_mask]
        };
        for _ in 0..512 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            values.push(rng & source_mask);
        }
        for value in values {
            for flags in [2, 0x202, 0x8d7, 0xed7] {
                let mut cpu = Cpu {
                    registers: std::array::from_fn(|i| 0xcafebabe12340000 | i as u64),
                    fuel: 100,
                    flags,
                    vectors: [[0xa5; 32]; 16],
                    ..Cpu::default()
                };
                cpu.registers[0] = (cpu.registers[0] & !source_mask) | value;
                let mut expected = cpu.registers;
                let signed = if value >= sign {
                    value as i128 - (1i128 << source_bits)
                } else {
                    value as i128
                };
                let result = if destination == 0 {
                    signed as u64
                } else if signed < 0 {
                    u64::MAX
                } else {
                    0
                } & destination_mask;
                expected[destination] = if destination_bits == 16 {
                    (expected[destination] & !destination_mask) | result
                } else {
                    result
                };
                // SAFETY: generated accumulator-only code uses the checked Cpu layout
                // and receives a live exclusive pointer for this synchronous invocation.
                unsafe { block(&mut cpu) };
                assert_eq!(
                    cpu.registers, expected,
                    "source={source_bits} destination={destination_bits}/{destination} value={value:x}"
                );
                assert_eq!(cpu.flags, flags);
                assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
                assert_eq!((cpu.fault, cpu.stop, cpu.rip, cpu.fuel), (0, 3, end, 98));
                extension_checks += 1;
            }
        }
    }
    println!(
        "PASS: native CBW/CWDE/CDQE/CWD/CDQ/CQO sign extension, partial-register preservation and unchanged flags; {extension_checks} checks"
    );
    println!(
        "PASS: Rust-native NEG/SBB/DEC register aliases, DEC guarded memory, widths, flags and preserved state; {checks} checks"
    );
}
