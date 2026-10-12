//! Source-ordered effects for FreeBSD 11.4 gdtoa strtod.
use super::{parse_freebsd, Error, FreeBsdParsed, Rounding};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects {
    pub parsed: FreeBsdParsed,
    /// New sticky SSE flags only. Caller preserves controls and existing flags.
    pub mxcsr_flags: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectsError {
    Parse(Error),
    ArithmeticModel,
    IterationBudget,
}

/// Execute the covered gdtoa paths without using host floating point.
///
/// Hex conversion, specials and zero are integer-only. For decimal nd<=15,
/// strtod.c's fast paths construct an exact integer coefficient, then perform
/// one multiply/divide by an exactly represented 10^k (k<=22). The optional
/// first multiplication has nd+i<=15 decimal digits and is therefore exact:
/// its integer result is <10^15<2^50. Only the final operation can set PE.
/// These paths cannot overflow, underflow or consume a denormal operand.
/// The caller must enforce matching RC, masked exceptions and no DAZ/FTZ.
pub fn parse_effects(input: &[u8], rounding: Rounding) -> Result<Effects, EffectsError> {
    let parsed = parse_freebsd(input, rounding).map_err(EffectsError::Parse)?;
    let mut text = &input[..parsed.end];
    while text.first().is_some_and(|b| matches!(*b, 9..=13 | 32)) {
        text = &text[1..];
    }
    let negative = text.first() == Some(&b'-');
    if matches!(text.first(), Some(b'+' | b'-')) {
        text = &text[1..];
    }
    if text.is_empty()
        || text
            .first()
            .is_some_and(|b| b.eq_ignore_ascii_case(&b'i') || b.eq_ignore_ascii_case(&b'n'))
        || (text.first() == Some(&b'0') && matches!(text.get(1), Some(b'x' | b'X')))
    {
        return Ok(Effects {
            parsed,
            mxcsr_flags: 0,
        });
    }
    let (nd, exponent) = decimal_shape(text);
    if nd == 0 {
        return Ok(Effects {
            parsed,
            mxcsr_flags: 0,
        });
    }
    if nd > 15 || exponent < -22 || exponent > 22 + (15 - nd) as i32 {
        let (parsed, mxcsr_flags) =
            super::decimal_effects::refine(text, parsed, negative, rounding).map_err(|error| {
                match error {
                    super::decimal_refinement::RefinementError::IterationBudget => {
                        EffectsError::IterationBudget
                    }
                    _ => EffectsError::ArithmeticModel,
                }
            })?;
        return Ok(Effects {
            parsed,
            mxcsr_flags,
        });
    }
    Ok(Effects {
        parsed,
        mxcsr_flags: if parsed.inexact { 0x20 } else { 0 },
    })
}

// Mirror gdtoa's digit accounting: discard leading integer zeros and trailing
// fractional zeros, but retain integer trailing zeros and fractional leading
// zeros in the scale. Input is already the parser's consumed decimal token.
fn decimal_shape(text: &[u8]) -> (usize, i32) {
    let mut at = 0;
    while text.get(at) == Some(&b'0') {
        at += 1;
    }
    let begin = at;
    while text.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    let mut nd = at - begin;
    let mut nf = 0;
    if text.get(at) == Some(&b'.') {
        at += 1;
        let frac_begin = at;
        let mut first_nonzero = None;
        while let Some(b) = text.get(at).filter(|b| b.is_ascii_digit()) {
            if *b != b'0' {
                first_nonzero.get_or_insert(at);
                nf = at - frac_begin + 1;
            }
            at += 1;
        }
        nd += if nd == 0 {
            first_nonzero.map_or(0, |first| nf - (first - frac_begin))
        } else {
            nf
        };
    }
    let mut e: i32 = 0;
    if matches!(text.get(at), Some(b'e' | b'E')) {
        at += 1;
        let negative = text.get(at) == Some(&b'-');
        if matches!(text.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while let Some(b) = text.get(at).filter(|b| b.is_ascii_digit()) {
            e = (e.saturating_mul(10) + i32::from(*b - b'0')).min(19999);
            at += 1;
        }
        if negative {
            e = -e;
        }
    }
    (nd, e - nf as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fast_paths_and_general_refinement() {
        for s in [
            "0.1",
            "123456789012345e22",
            "1e36",
            "000.00100",
            "0x1p-1075",
            "nan",
            "1e+",
            "no",
            "0e9999",
        ] {
            assert!(
                parse_effects(s.as_bytes(), Rounding::NearestEven).is_ok(),
                "{s}"
            );
        }
        for s in [
            "1234567890123456",
            "1e37",
            "1e-23",
            "1e-400",
            "0.00000000000000000000001",
        ] {
            assert!(
                parse_effects(s.as_bytes(), Rounding::NearestEven).is_ok(),
                "{s}"
            );
        }
        assert_eq!(
            parse_effects(b"\x0b0.1", Rounding::NearestEven)
                .unwrap()
                .mxcsr_flags,
            0x20
        );
        assert_eq!(
            parse_effects(b"0x1p-1075", Rounding::NearestEven)
                .unwrap()
                .mxcsr_flags,
            0
        );
        assert_eq!(
            parse_effects(b"0.5", Rounding::NearestEven)
                .unwrap()
                .mxcsr_flags,
            0
        );
    }
    #[test]
    fn source_digit_accounting() {
        assert_eq!(decimal_shape(b"0001200.003000e5"), (7, 2));
        assert_eq!(decimal_shape(b"000.00100"), (1, -3));
        assert_eq!(decimal_shape(b"12.000"), (2, 0));
        assert_eq!(decimal_shape(b"0.000e999"), (0, 999));
    }
    #[test]
    fn general_flags_are_intermediate_effects_not_result_inexactness() {
        // Refinement can re-enter with a zero approximation on this input.
        let tiny = parse_effects(b"5961995109394176e-374", Rounding::NearestEven).unwrap();
        assert_eq!(
            (tiny.parsed.bits, tiny.parsed.errno, tiny.mxcsr_flags),
            (0, Some(34), 0x30)
        );
        let half = b"1.00000000000000011102230246251565404236316680908203125";
        let p = parse_effects(half, Rounding::NearestEven).unwrap();
        assert!(p.parsed.inexact);
        assert_eq!(p.mxcsr_flags, 0);
        assert_eq!(
            parse_effects(b"1e-400", Rounding::Down)
                .unwrap()
                .parsed
                .bits,
            1 << 63
        );
        assert_eq!(
            parse_effects(b"1e-400", Rounding::NearestEven)
                .unwrap()
                .mxcsr_flags,
            0x30
        );
        assert_eq!(
            parse_effects(b"1e400", Rounding::NearestEven)
                .unwrap()
                .mxcsr_flags,
            0
        );
        for (rounding, bits, errno) in [
            (Rounding::NearestEven, 1 << 52, None),
            (Rounding::Up, 1 << 52, None),
            (Rounding::Down, (1 << 52) - 1, Some(34)),
            (Rounding::TowardZero, (1 << 52) - 1, Some(34)),
        ] {
            let p = parse_effects(b"2.2250738585072012e-308", rounding).unwrap();
            assert_eq!(
                (p.parsed.bits, p.parsed.errno, p.mxcsr_flags),
                (bits, errno, 0x20)
            );
        }
    }
    #[test]
    fn exact_decimal_subnormal_and_bounded_long_coefficient() {
        let mut digits = vec![1u8];
        for _ in 0..1074 {
            let mut carry = 0u32;
            for digit in &mut digits {
                let v = u32::from(*digit) * 5 + carry;
                *digit = (v % 10) as u8;
                carry = v / 10;
            }
            while carry != 0 {
                digits.push((carry % 10) as u8);
                carry /= 10;
            }
        }
        let text = format!(
            "{}e-1074",
            digits
                .iter()
                .rev()
                .map(|d| char::from(b'0' + d))
                .collect::<String>()
        );
        for rounding in [
            Rounding::NearestEven,
            Rounding::Up,
            Rounding::Down,
            Rounding::TowardZero,
        ] {
            let p = parse_effects(text.as_bytes(), rounding).unwrap();
            assert_eq!(
                (
                    p.parsed.bits,
                    p.parsed.errno,
                    p.mxcsr_flags,
                    p.parsed.inexact
                ),
                (1, Some(34), 0x20, false)
            );
        }
        let text = format!("1{}e-4095", "0".repeat(4095));
        let p = parse_effects(text.as_bytes(), Rounding::NearestEven).unwrap();
        assert_eq!((p.parsed.bits, p.mxcsr_flags), (0x3ff0000000000000, 0));
        assert_eq!(
            parse_effects("1".repeat(4097).as_bytes(), Rounding::NearestEven),
            Err(EffectsError::Parse(Error::SignificandLimit))
        );
    }
}
