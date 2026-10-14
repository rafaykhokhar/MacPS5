//! Integer-only COMISD/UCOMISD comparison and guest status handling.
//! Intel SDM vol.2 COMISD/UCOMISD: unordered sets ZF/PF/CF; OF/SF/AF clear.
//! https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf
//! NaN processing precedes denormal handling (SDM vol.1 section 4.9.2):
//! https://www.intel.com/content/dam/develop/external/us/en/documents-tps/253665-sdm-vol-11.pdf
use super::{FpUnsupported, MXCSR_VALID, arithmetic::finish};
const SIGN: u64 = 1 << 63;
const INF: u64 = 0x7ff0_0000_0000_0000;
const FRACTION: u64 = (1 << 52) - 1;
pub const COMPARE_FLAGS: u64 = 0x8d5;

/// Returns just the defined arithmetic flag bits, plus updated guest MXCSR.
pub fn compare_binary64(
    a: u64,
    b: u64,
    control: u32,
    quiet: bool,
) -> Result<(u64, u32), FpUnsupported> {
    if control & !MXCSR_VALID != 0 {
        return Err(FpUnsupported::ReservedControl);
    }
    let nan = |v: u64| v & INF == INF && v & FRACTION != 0;
    let (flags, status) = if nan(a) || nan(b) {
        let invalid = !quiet || (nan(a) && a & (1 << 51) == 0) || (nan(b) && b & (1 << 51) == 0);
        (0x45, u32::from(invalid))
    } else {
        let mut status = 0;
        let mut normalize = |v: u64| {
            if v & INF == 0 && v & FRACTION != 0 {
                if control & 0x40 != 0 {
                    return v & SIGN;
                }
                status |= 2;
            }
            v
        };
        let a = normalize(a);
        let b = normalize(b);
        let equal = a == b || (a & !SIGN == 0 && b & !SIGN == 0);
        let key = |v: u64| if v & SIGN != 0 { !v } else { v ^ SIGN };
        (
            if equal {
                0x40
            } else if key(a) < key(b) {
                1
            } else {
                0
            },
            status,
        )
    };
    let result = finish(flags, control, status)?;
    Ok((result.bits, result.mxcsr))
}

/// # Safety
/// `cpu` is null or points to a live, uniquely borrowed matching Cpu context.
#[allow(unsafe_code)]
pub unsafe extern "C" fn native_compare_binary64(
    cpu: *mut crate::legacy_cpu::Cpu,
    a: u64,
    b: u64,
    quiet: u64,
) {
    // Safety: the translator supplies its live, uniquely borrowed Cpu context.
    let Some(cpu) = (unsafe { cpu.as_mut() }) else {
        return;
    };
    if cpu.fault != 0 {
        return;
    }
    match compare_binary64(a, b, cpu.mxcsr, quiet != 0) {
        Ok((flags, mxcsr)) => {
            cpu.mxcsr = mxcsr;
            cpu.flags = (cpu.flags & !COMPARE_FLAGS) | flags;
        }
        Err(FpUnsupported::ReservedControl) => cpu.fault = super::CONTROL_FAULT,
        Err(FpUnsupported::Exception { mxcsr, .. }) => {
            cpu.mxcsr = mxcsr;
            cpu.fault = 15;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_ordering_matches_independent_host_comparisons() {
        let mut state = 0x9e3779b97f4a7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // Normal finite operands avoid host subnormal/NaN policy dependence.
            let exponent = 1 + ((state >> 52) % 2046);
            (state & (SIGN | FRACTION)) | (exponent << 52)
        };
        for _ in 0..20000 {
            let a = next();
            let b = next();
            let expected = match f64::from_bits(a).partial_cmp(&f64::from_bits(b)).unwrap() {
                std::cmp::Ordering::Less => 1,
                std::cmp::Ordering::Equal => 0x40,
                std::cmp::Ordering::Greater => 0,
            };
            assert_eq!(compare_binary64(a, b, 0x1f80, true), Ok((expected, 0x1f80)));
            assert_eq!(compare_binary64(a, a, 0x1f80, false), Ok((0x40, 0x1f80)));
        }
    }
    #[test]
    fn relations_nan_priority_and_denormal_controls() {
        for quiet in [false, true] {
            for (a, b, wanted) in [
                (0, SIGN, 0x40),
                (SIGN | 0x3ff0000000000000, 0, 1),
                (INF, 0x3ff0000000000000, 0),
                (SIGN | INF, SIGN | 0x3ff0000000000000, 1),
            ] {
                assert_eq!(compare_binary64(a, b, 0x1f80, quiet), Ok((wanted, 0x1f80)));
            }
            assert_eq!(compare_binary64(1, 0, 0x1f80, quiet), Ok((0, 0x1f82)));
            assert_eq!(compare_binary64(1, 0, 0x1fc0, quiet), Ok((0x40, 0x1fc0)));
            assert!(matches!(
                compare_binary64(1, 0, 0x1e80, quiet),
                Err(FpUnsupported::Exception { raised: 2, .. })
            ));
            assert_eq!(
                compare_binary64(INF | 1, 1, 0x1f80, quiet),
                Ok((0x45, 0x1f81))
            );
            assert_eq!(
                compare_binary64(INF | (1 << 51), 1, 0x1f80, quiet),
                Ok((0x45, if quiet { 0x1f80 } else { 0x1f81 }))
            );
        }
    }
}
