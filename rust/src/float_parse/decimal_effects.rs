//! Bridge from validated decimal tokens to source-ordered gdtoa effects.
use super::decimal_refinement::{self, RefinementError};
use super::{FreeBsdParsed, Rounding};
/// Token has already been consumed and bounded by the pure parser; whitespace
/// and the optional sign have been removed. Keep spelling, not just value:
/// equivalent spellings can execute different gdtoa arithmetic paths.
pub fn refine(
    text: &[u8],
    mut parsed: FreeBsdParsed,
    negative: bool,
    rounding: Rounding,
) -> Result<(FreeBsdParsed, u32), RefinementError> {
    let mut digits = Vec::new();
    let mut at = 0;
    let mut fractional = 0;
    while text.get(at).is_some_and(u8::is_ascii_digit) {
        digits.push(text[at]);
        at += 1;
    }
    if text.get(at) == Some(&b'.') {
        at += 1;
        while text.get(at).is_some_and(u8::is_ascii_digit) {
            digits.push(text[at]);
            fractional += 1;
            at += 1;
        }
        while fractional > 0 && digits.last() == Some(&b'0') {
            digits.pop();
            fractional -= 1;
        }
    }
    let skip = digits
        .iter()
        .position(|b| *b != b'0')
        .expect("nonzero validated decimal");
    let digits = &digits[skip..];
    let mut exponent = 0i32;
    if matches!(text.get(at), Some(b'e' | b'E')) {
        at += 1;
        let minus = text.get(at) == Some(&b'-');
        if matches!(text.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while text.get(at).is_some_and(u8::is_ascii_digit) {
            exponent = (exponent.saturating_mul(10) + i32::from(text[at] - b'0')).min(19999);
            at += 1;
        }
        if minus {
            exponent = -exponent;
        }
    }
    exponent -= fractional;
    let mode = match rounding {
        Rounding::NearestEven => 0,
        Rounding::Down => 1,
        Rounding::Up => 2,
        Rounding::TowardZero => 3,
    };
    let control = 0x1f80 | (mode << 13);
    let result = if mode == 0 {
        decimal_refinement::nearest(digits, exponent, negative, control)
    } else {
        decimal_refinement::directed(digits, exponent, negative, control)
    }?;
    parsed.bits = result.bits;
    Ok((parsed, result.mxcsr & 63))
}
