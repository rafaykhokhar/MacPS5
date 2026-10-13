//! Bounded, integer-only binary64 printf conversion, C decimal point.
//!
//! Policy references (FreeBSD releng/11.4, inspected 2026-10-13):
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vfprintf.c
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/contrib/gdtoa/dtoa.c
//! This is an independent exact-decimal algorithm, not copied dtoa code. Rounding
//! is supplied explicitly; this module neither reads nor changes host/guest FP
//! status flags. Target dtoa build flags/exception effects are not established.
//! Hex policy: https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gdtoa/_hdtoa.c
//! No extended80, locale grouping, sign padding or field width.

pub const MAX_PRECISION: usize = 4096;
const DIGITS: usize = 1100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rounding {
    NearestEven,
    TowardZero,
    Upward,
    Downward,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    PrecisionLimit,
    Allocation,
    InternalBound,
    Control,
}
#[derive(Debug, PartialEq, Eq)]
pub struct Formatted {
    pub negative: bool,
    pub body: Vec<u8>,
    /// Prefix bytes in body, after which outer zero padding must be inserted.
    pub prefix_len: usize,
    /// Special values disable zero padding in vfprintf.
    pub special: bool,
    /// FreeBSD vfprintf clears even a requested plus/space sign for NaN.
    pub suppress_sign: bool,
}

// Significant decimal digits, most significant first. Exact binary64 values
// require at most 767 coefficient digits (53 bits multiplied by 5^1074).
// 1100 also comfortably bounds decimal carries and all intermediate products.
struct Decimal {
    digits: [u8; DIGITS],
    len: usize,
    exponent: i32,
}
impl Decimal {
    fn exact(bits: u64) -> Result<Self, Error> {
        let encoded = ((bits >> 52) & 0x7ff) as i32;
        let mut coefficient = bits & ((1u64 << 52) - 1);
        let power = if encoded == 0 {
            -1074
        } else {
            coefficient |= 1u64 << 52;
            encoded - 1023 - 52
        };
        let mut value = Self {
            digits: [0; DIGITS],
            len: 0,
            exponent: 0,
        };
        if coefficient == 0 {
            value.len = 1;
            return Ok(value);
        }
        while coefficient != 0 {
            value.digits[value.len] = (coefficient % 10) as u8;
            value.len += 1;
            coefficient /= 10;
        }
        for _ in 0..power.unsigned_abs() {
            let mut carry = 0u16;
            for digit in &mut value.digits[..value.len] {
                let product = u16::from(*digit) * if power < 0 { 5 } else { 2 } + carry;
                *digit = (product % 10) as u8;
                carry = product / 10;
            }
            while carry != 0 {
                if value.len == DIGITS {
                    return Err(Error::InternalBound);
                }
                value.digits[value.len] = (carry % 10) as u8;
                value.len += 1;
                carry /= 10;
            }
        }
        value.exponent = value.len as i32 - 1 + power.min(0);
        value.digits[..value.len].reverse();
        value.trim();
        Ok(value)
    }
    fn trim(&mut self) {
        while self.len > 1 && self.digits[self.len - 1] == 0 {
            self.len -= 1;
        }
    }
    fn digit(&self, index: i32) -> u8 {
        if index < 0 || index as usize >= self.len {
            b'0'
        } else {
            b'0' + self.digits[index as usize]
        }
    }
    fn round(&mut self, keep: i32, negative: bool, mode: Rounding) {
        if keep >= self.len as i32 || (self.len == 1 && self.digits[0] == 0) {
            return;
        }
        let cut = keep.max(0) as usize;
        let nonzero = self.digits[cut..self.len].iter().any(|&d| d != 0);
        let above_half = keep >= 0
            && (self.digits[cut] > 5
                || (self.digits[cut] == 5
                    && self.digits[cut + 1..self.len].iter().any(|&d| d != 0)));
        let tie = keep >= 0
            && self.digits[cut] == 5
            && self.digits[cut + 1..self.len].iter().all(|&d| d == 0);
        let increment = match mode {
            Rounding::NearestEven => {
                above_half || (tie && cut > 0 && self.digits[cut - 1] % 2 == 1)
            }
            Rounding::TowardZero => false,
            Rounding::Upward => !negative && nonzero,
            Rounding::Downward => negative && nonzero,
        };
        if keep <= 0 {
            self.len = 1;
            self.digits[0] = u8::from(increment);
            self.exponent = if increment {
                self.exponent - keep + 1
            } else {
                0
            };
            return;
        }
        self.len = cut;
        if increment {
            let mut position = cut;
            loop {
                if position == 0 {
                    self.digits[0] = 1;
                    self.len = 1;
                    self.exponent += 1;
                    break;
                }
                position -= 1;
                if self.digits[position] < 9 {
                    self.digits[position] += 1;
                    break;
                }
                self.digits[position] = 0;
            }
        }
        self.trim();
    }
    fn fixed(&self, out: &mut Vec<u8>, precision: usize, alternate: bool) {
        for position in 0..self.exponent.max(0) + 1 {
            out.push(self.digit(position));
        }
        if self.exponent < 0 {
            out[0] = b'0';
        }
        if precision != 0 || alternate {
            out.push(b'.');
        }
        for fraction in 1..=precision {
            out.push(self.digit(self.exponent + fraction as i32));
        }
    }
    fn scientific(&self, out: &mut Vec<u8>, precision: usize, alternate: bool, upper: bool) {
        out.push(self.digit(0));
        if precision != 0 || alternate {
            out.push(b'.');
        }
        for position in 1..=precision {
            out.push(self.digit(position as i32));
        }
        out.push(if upper { b'E' } else { b'e' });
        out.push(if self.exponent < 0 { b'-' } else { b'+' });
        let exponent = self.exponent.unsigned_abs();
        if exponent >= 100 {
            out.push(b'0' + (exponent / 100) as u8);
        }
        out.push(b'0' + ((exponent / 10) % 10) as u8);
        out.push(b'0' + (exponent % 10) as u8);
    }
}

// Exact bit extraction and integer quantization implement the normalized
// leading-one spelling selected by FreeBSD hdtoa, including subnormals.
fn hexadecimal(
    bits: u64,
    precision: Option<usize>,
    alternate: bool,
    rounding: Rounding,
    out: &mut Vec<u8>,
    upper: bool,
) {
    let encoded = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1u64 << 52) - 1);
    let mut significand;
    let mut exponent;
    if encoded != 0 {
        significand = fraction | (1u64 << 52);
        exponent = encoded - 1023;
    } else if fraction != 0 {
        let top = 63 - fraction.leading_zeros();
        significand = fraction << (52 - top);
        exponent = top as i32 - 1074;
    } else {
        significand = 0;
        exponent = 0;
    }
    let places = precision.unwrap_or_else(|| {
        if significand == 0 {
            0
        } else {
            (52 - significand.trailing_zeros().min(52)).div_ceil(4) as usize
        }
    });
    if places < 13 {
        let shift = 52 - places * 4;
        let remainder = significand & ((1u64 << shift) - 1);
        let half = 1u64 << (shift - 1);
        let quotient = significand >> shift;
        let negative = bits >> 63 != 0;
        let increment = match rounding {
            Rounding::NearestEven => remainder > half || (remainder == half && quotient & 1 != 0),
            Rounding::TowardZero => false,
            Rounding::Upward => !negative && remainder != 0,
            Rounding::Downward => negative && remainder != 0,
        };
        significand = (quotient + u64::from(increment)) << shift;
        if significand == 1u64 << 53 {
            significand >>= 1;
            exponent += 1;
        }
    }
    out.extend_from_slice(if upper { b"0X" } else { b"0x" });
    out.push(if significand == 0 { b'0' } else { b'1' });
    if places != 0 || alternate {
        out.push(b'.');
    }
    let alphabet = if upper {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    for position in 0..places {
        let nibble = if position < 13 {
            (significand >> (48 - 4 * position)) & 15
        } else {
            0
        };
        out.push(alphabet[nibble as usize]);
    }
    out.push(if upper { b'P' } else { b'p' });
    out.push(if exponent < 0 { b'-' } else { b'+' });
    let mut magnitude = exponent.unsigned_abs();
    let mut digits = [0u8; 4];
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[start..]);
}

pub fn format(
    bits: u64,
    conversion: u8,
    precision: Option<usize>,
    alternate: bool,
    rounding: Rounding,
) -> Result<Formatted, Error> {
    let kind = conversion.to_ascii_lowercase();
    if !matches!(kind, b'f' | b'e' | b'g' | b'a') {
        return Err(Error::Unsupported);
    }
    let requested_precision = precision;
    let mut precision = precision.unwrap_or(6);
    if precision > MAX_PRECISION {
        return Err(Error::PrecisionLimit);
    }
    if kind == b'g' {
        precision = precision.max(1);
    }
    let negative = bits >> 63 != 0;
    let upper = conversion.is_ascii_uppercase();
    let special = (bits >> 52) & 0x7ff == 0x7ff;
    let nan = special && bits & ((1u64 << 52) - 1) != 0;
    let mut body = Vec::new();
    // Maximum finite fixed integral field is 309 digits. General fixed can
    // include four leading fractional zeros; scientific exponent is <= 324.
    body.try_reserve_exact(precision + 320)
        .map_err(|_| Error::Allocation)?;
    if special {
        body.extend_from_slice(match (nan, upper) {
            (true, true) => b"NAN",
            (true, false) => b"nan",
            (false, true) => b"INF",
            (false, false) => b"inf",
        });
    } else if kind == b'a' {
        hexadecimal(
            bits,
            requested_precision,
            alternate,
            rounding,
            &mut body,
            upper,
        );
    } else {
        let mut decimal = Decimal::exact(bits)?;
        let keep = match kind {
            b'f' => decimal.exponent + 1 + precision as i32,
            b'e' => precision as i32 + 1,
            _ => precision as i32,
        };
        decimal.round(keep, negative, rounding);
        match kind {
            b'f' => decimal.fixed(&mut body, precision, alternate),
            b'e' => decimal.scientific(&mut body, precision, alternate, upper),
            _ => {
                let significant = if alternate {
                    precision
                } else {
                    decimal.len.min(precision)
                };
                if decimal.exponent < -4 || decimal.exponent >= precision as i32 {
                    decimal.scientific(&mut body, significant - 1, alternate, upper);
                } else {
                    let fraction = (significant as i32 - decimal.exponent - 1).max(0) as usize;
                    decimal.fixed(&mut body, fraction, alternate);
                }
            }
        }
    }
    Ok(Formatted {
        negative: negative && !nan,
        body,
        prefix_len: if kind == b'a' && !special { 2 } else { 0 },
        special,
        suppress_sign: nan,
    })
}

/// Source hdtoa effects under masked, gradual-underflow SSE binary64 controls.
/// The upstream integer-bit fpclassifyd leaves signaling NaNs untouched:
/// https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/fpclassify.c
/// Subnormal normalization multiplies by 2^514 exactly (denormal-operand flag).
/// Explicit short precision uses a scaled add/subtract of signed one: all
/// operands/results are normal, and only discarded significand bits produce
/// precision loss. The subtraction is exact by Sterbenz's lemma. Default or
/// Thirteen or more fractional hexadecimal digits perform no rounding arithmetic.
/// Returns complete MXCSR preserving incoming sticky flags. This is the source
/// operation model; native console compiler/code-generation parity is separate.
pub fn hex_effects(bits: u64, precision: Option<usize>, mxcsr: u32) -> Result<u32, Error> {
    if precision.is_some_and(|p| p > MAX_PRECISION) {
        return Err(Error::PrecisionLimit);
    }
    if mxcsr & !0xffff != 0 || mxcsr & 0x1f80 != 0x1f80 || mxcsr & 0x8040 != 0 {
        return Err(Error::Control);
    }
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1u64 << 52) - 1);
    if exponent == 0x7ff || (exponent == 0 && fraction == 0) {
        return Ok(mxcsr);
    }
    let (significand, flags) = if exponent == 0 {
        let top = 63 - fraction.leading_zeros();
        (fraction << (52 - top), 2)
    } else {
        (fraction | (1u64 << 52), 0)
    };
    let inexact =
        precision.is_some_and(|p| p < 13 && significand & ((1u64 << (52 - p * 4)) - 1) != 0);
    Ok(mxcsr | flags | if inexact { 32 } else { 0 })
}

/// A source PRINT vector or PAD request. Empty PRINTs still count toward NIOV.
/// Ranges refer to `SourceFormatted.value.body`; the outer formatter emits the
/// prefix separately before these pieces and splits ZeroPad into 16-byte chunks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Print(std::ops::Range<usize>),
    ZeroPad(usize),
}
#[derive(Debug)]
pub struct SourceFormatted {
    pub value: Formatted,
    pub pieces: Vec<Piece>,
}

/// FreeBSD 11.4 output-vector policy, separate from mathematical rounding.
/// This does not establish source FP-status effects; callers must resolve that
/// boundary before registering a runtime service. In dtoa mode3, ilim<=0 uses
/// the half comparison even for directed rounding (dtoa.c:594-605).
pub fn source_format(
    bits: u64,
    conversion: u8,
    precision: Option<usize>,
    alternate: bool,
    rounding: Rounding,
) -> Result<SourceFormatted, Error> {
    let kind = conversion.to_ascii_lowercase();
    let mut value = format(bits, conversion, precision, alternate, rounding)?;
    let mut pieces = Vec::new();
    pieces.try_reserve_exact(8).map_err(|_| Error::Allocation)?;
    if value.special {
        pieces.push(Piece::Print(0..value.body.len()));
        return Ok(SourceFormatted { value, pieces });
    }
    let mut p = precision.unwrap_or(6);
    let mut at = value.prefix_len;
    fn print(pieces: &mut Vec<Piece>, at: &mut usize, count: usize) {
        pieces.push(Piece::Print(*at..*at + count));
        *at += count;
    }
    fn pad(pieces: &mut Vec<Piece>, at: &mut usize, count: usize) {
        if count != 0 {
            pieces.push(Piece::ZeroPad(count));
            *at += count;
        }
    }
    fn print_and_pad(pieces: &mut Vec<Piece>, at: &mut usize, available: usize, total: usize) {
        let count = available.min(total);
        if count != 0 {
            print(pieces, at, count);
        }
        pad(pieces, at, total - count);
    }
    let (expt, ndig, scientific) = if kind == b'a' {
        let exponent_at = value
            .body
            .iter()
            .position(|&b| b == b'p' || b == b'P')
            .ok_or(Error::InternalBound)?;
        p = exponent_at - value.prefix_len - 1 - usize::from(value.body.contains(&b'.'));
        let zero = bits & !(1u64 << 63) == 0;
        (0, if zero { 1 } else { p + 1 }, true)
    } else {
        if kind == b'g' {
            p = p.max(1);
        }
        let mut decimal = Decimal::exact(bits)?;
        let keep = match kind {
            b'f' => decimal.exponent + 1 + p as i32,
            b'e' => p as i32 + 1,
            _ => p as i32,
        };
        let source_rounding = if kind == b'f' && keep <= 0 {
            Rounding::NearestEven
        } else {
            rounding
        };
        if source_rounding != rounding {
            value = format(bits, conversion, precision, alternate, source_rounding)?;
        }
        decimal.round(keep, bits >> 63 != 0, source_rounding);
        let nonzero_input = bits & !(1u64 << 63) != 0;
        let no_digits = kind == b'f' && nonzero_input && decimal.digits[0] == 0;
        let expt = if no_digits {
            -(p as i32)
        } else {
            decimal.exponent + 1
        };
        let ndig = if no_digits { 0 } else { decimal.len };
        let scientific = kind == b'e' || (kind == b'g' && (expt <= -4 || expt > p as i32));
        if kind == b'g' {
            let significant = if alternate { p } else { ndig };
            p = if scientific {
                significant - 1
            } else {
                (significant as i32 - expt).max(0) as usize
            };
        }
        (expt, ndig, scientific)
    };
    if scientific {
        print(&mut pieces, &mut at, 1);
        if p != 0 || alternate {
            print(&mut pieces, &mut at, 1);
            print(&mut pieces, &mut at, ndig - 1);
            pad(&mut pieces, &mut at, p + 1 - ndig);
        }
        let remaining = value
            .body
            .len()
            .checked_sub(at)
            .ok_or(Error::InternalBound)?;
        print(&mut pieces, &mut at, remaining);
    } else if expt <= 0 {
        print(&mut pieces, &mut at, 1);
        if p != 0 || alternate {
            print(&mut pieces, &mut at, 1);
        }
        pad(&mut pieces, &mut at, (-expt) as usize);
        let remaining = (p as i32 + expt).max(0) as usize;
        print_and_pad(&mut pieces, &mut at, ndig, remaining);
    } else {
        print_and_pad(&mut pieces, &mut at, ndig, expt as usize);
        if p != 0 || alternate {
            print(&mut pieces, &mut at, 1);
        }
        print_and_pad(&mut pieces, &mut at, ndig.saturating_sub(expt as usize), p);
    }
    if at != value.body.len() || pieces.len() > 8 {
        return Err(Error::InternalBound);
    }
    Ok(SourceFormatted { value, pieces })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(value: f64, conversion: u8, precision: usize, alternate: bool, expected: &str) {
        let result = format(
            value.to_bits(),
            conversion,
            Some(precision),
            alternate,
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(result.body, expected.as_bytes());
    }
    #[test]
    fn exact_ties_and_carry() {
        check(0.5, b'f', 0, false, "0");
        check(1.5, b'f', 0, false, "2");
        check(2.5, b'f', 0, false, "2");
        check(1.25, b'f', 1, false, "1.2");
        check(1.75, b'f', 1, false, "1.8");
        check(9.5, b'e', 0, false, "1e+01");
        check(1.0625, b'f', 3, false, "1.062");
        check(0.0625, b'f', 3, false, "0.062");
    }
    #[test]
    fn directed_rounding_sign_and_tiny_values() {
        for (value, mode, expected) in [
            (1.25f64, Rounding::Upward, "1.3"),
            (-1.25, Rounding::Upward, "1.2"),
            (-1.25, Rounding::Downward, "1.3"),
            (1.25, Rounding::Downward, "1.2"),
            (1.75, Rounding::TowardZero, "1.7"),
        ] {
            let result = format(value.to_bits(), b'f', Some(1), false, mode).unwrap();
            assert_eq!(result.body, expected.as_bytes());
            assert_eq!(result.negative, value < 0.0);
        }
        assert_eq!(
            format(1, b'f', Some(2), false, Rounding::Upward)
                .unwrap()
                .body,
            b"0.01"
        );
        assert_eq!(
            format(1, b'f', Some(2), false, Rounding::NearestEven)
                .unwrap()
                .body,
            b"0.00"
        );
    }
    #[test]
    fn general_rounded_exponent_and_alternate() {
        check(9.5, b'g', 1, false, "1e+01");
        check(9.5, b'G', 1, true, "1.E+01");
        check(0.000099999, b'g', 1, false, "0.0001");
        check(100.0, b'g', 6, false, "100");
        check(100.0, b'g', 6, true, "100.000");
        check(0.0, b'g', 6, true, "0.00000");
        check(12.0, b'g', 0, false, "1e+01");
        check(1.0, b'f', 0, true, "1.");
    }
    #[test]
    fn extremes_known_decimal_values() {
        check(f64::from_bits(1), b'e', 6, false, "4.940656e-324");
        check(f64::MIN_POSITIVE, b'e', 6, false, "2.225074e-308");
        check(f64::MAX, b'e', 6, false, "1.797693e+308");
        check(
            f64::from_bits(0x3fb999999999999a),
            b'f',
            55,
            false,
            "0.1000000000000000055511151231257827021181583404541015625",
        );
    }
    #[test]
    fn special_sign_and_defaults() {
        let zero = format(
            (-0.0f64).to_bits(),
            b'f',
            None,
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert!(zero.negative);
        assert_eq!(zero.body, b"0.000000");
        let nan = format(0xfff0000000000001, b'G', None, false, Rounding::NearestEven).unwrap();
        assert!(nan.special && nan.suppress_sign && !nan.negative);
        assert_eq!(nan.body, b"NAN");
        let inf = format(
            f64::NEG_INFINITY.to_bits(),
            b'e',
            None,
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert!(inf.special && inf.negative && !inf.suppress_sign);
        assert_eq!(inf.body, b"inf");
    }
    // Independent u128 rational oracle: inputs i/256 are exact dyadics, and
    // decimal quantization uses quotient/remainder without decimal digit code.
    #[test]
    fn rational_fixed_oracle() {
        for numerator in 0u128..=1024 {
            for precision in 0usize..=4 {
                let scale = 10u128.pow(precision as u32);
                let quotient = numerator * scale / 256;
                let remainder = numerator * scale % 256;
                for negative in [false, true] {
                    for mode in [
                        Rounding::NearestEven,
                        Rounding::TowardZero,
                        Rounding::Upward,
                        Rounding::Downward,
                    ] {
                        let increment = match mode {
                            Rounding::NearestEven => {
                                remainder > 128 || (remainder == 128 && quotient % 2 == 1)
                            }
                            Rounding::TowardZero => false,
                            Rounding::Upward => !negative && remainder != 0,
                            Rounding::Downward => negative && remainder != 0,
                        };
                        let rounded = quotient + u128::from(increment);
                        let mut expected = (rounded / scale).to_string();
                        if precision != 0 {
                            expected.push('.');
                            let fraction = (rounded % scale).to_string();
                            expected.extend(std::iter::repeat_n('0', precision - fraction.len()));
                            expected.push_str(&fraction);
                        }
                        let value = numerator as f64 / 256.0;
                        let bits = value.to_bits() | (u64::from(negative) << 63);
                        let result = format(bits, b'f', Some(precision), false, mode).unwrap();
                        assert_eq!(
                            result.body,
                            expected.as_bytes(),
                            "{numerator}/256 p={precision} {mode:?}"
                        );
                        assert_eq!(result.negative, negative);
                    }
                }
            }
        }
    }

    #[test]
    fn hexadecimal_defaults_subnormals_and_extrema() {
        for (bits, expected) in [
            (0u64, "0x0p+0"),
            (1, "0x1p-1074"),
            (3, "0x1.8p-1073"),
            (0x000fffffffffffff, "0x1.ffffffffffffep-1023"),
            (0x0010000000000000, "0x1p-1022"),
            (0x7fefffffffffffff, "0x1.fffffffffffffp+1023"),
            (0x3ff8000000000000, "0x1.8p+0"),
        ] {
            let result = format(bits, b'a', None, false, Rounding::NearestEven).unwrap();
            assert_eq!(result.body, expected.as_bytes());
            assert_eq!(result.prefix_len, 2);
        }
        check(1.5, b'A', 3, true, "0X1.800P+0");
        check(1.0, b'a', 0, true, "0x1.p+0");
        check(1.0, b'a', 15, false, "0x1.000000000000000p+0");
        assert_eq!(
            format(
                f64::INFINITY.to_bits(),
                b'a',
                None,
                false,
                Rounding::NearestEven
            )
            .unwrap()
            .prefix_len,
            0
        );
    }

    #[test]
    fn hexadecimal_rounding_ties_even_carries_and_signs() {
        check(1.5, b'a', 0, false, "0x1p+1");
        check(1.03125, b'a', 1, false, "0x1.0p+0");
        check(1.09375, b'a', 1, false, "0x1.2p+0");
        check(1.96875, b'a', 1, false, "0x1.0p+1");
        for negative in [false, true] {
            for mode in [
                Rounding::NearestEven,
                Rounding::TowardZero,
                Rounding::Upward,
                Rounding::Downward,
            ] {
                let bits = 1.96875f64.to_bits() | (u64::from(negative) << 63);
                let up = mode == Rounding::NearestEven
                    || mode
                        == if negative {
                            Rounding::Downward
                        } else {
                            Rounding::Upward
                        };
                let result = format(bits, b'a', Some(1), false, mode).unwrap();
                assert_eq!(result.body, if up { b"0x1.0p+1" } else { b"0x1.fp+0" });
                assert_eq!(result.negative, negative);
            }
        }
        check(f64::MAX, b'a', 0, false, "0x1p+1024");
        let big = format(1, b'a', Some(MAX_PRECISION), true, Rounding::NearestEven).unwrap();
        assert_eq!(big.body.len(), MAX_PRECISION + 10);
        assert!(big.body.starts_with(b"0x1."));
        assert!(big.body.ends_with(b"p-1074"));
    }

    #[test]
    fn directed_scientific_and_general_carry_switches() {
        for negative in [false, true] {
            for mode in [
                Rounding::NearestEven,
                Rounding::TowardZero,
                Rounding::Upward,
                Rounding::Downward,
            ] {
                let up = mode == Rounding::NearestEven
                    || mode
                        == if negative {
                            Rounding::Downward
                        } else {
                            Rounding::Upward
                        };
                let bits = 9.5f64.to_bits() | (u64::from(negative) << 63);
                let scientific = format(bits, b'e', Some(0), false, mode).unwrap();
                let general = format(bits, b'g', Some(1), false, mode).unwrap();
                assert_eq!(scientific.body, if up { b"1e+01" } else { b"9e+00" });
                assert_eq!(general.body, if up { b"1e+01".as_slice() } else { b"9" });
                assert_eq!(general.negative, negative);
            }
        }
    }

    #[test]
    fn source_vectors_keep_prints_distinct_from_padding() {
        let v = source_format(
            1.0f64.to_bits(),
            b'e',
            Some(3),
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(v.value.body, b"1.000e+00");
        assert_eq!(
            v.pieces,
            [
                Piece::Print(0..1),
                Piece::Print(1..2),
                Piece::Print(2..2),
                Piece::ZeroPad(3),
                Piece::Print(5..9)
            ]
        );
        let h = source_format(
            1.0f64.to_bits(),
            b'a',
            Some(3),
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(
            h.pieces,
            [
                Piece::Print(2..3),
                Piece::Print(3..4),
                Piece::Print(4..7),
                Piece::Print(7..10)
            ]
        );
        let f = source_format(
            100.0f64.to_bits(),
            b'f',
            Some(2),
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(
            f.pieces,
            [
                Piece::Print(0..1),
                Piece::ZeroPad(2),
                Piece::Print(3..4),
                Piece::ZeroPad(2)
            ]
        );
        let tiny = source_format(1, b'f', Some(2), false, Rounding::Upward).unwrap();
        assert_eq!(tiny.value.body, b"0.00");
        assert_eq!(
            tiny.pieces,
            [Piece::Print(0..1), Piece::Print(1..2), Piece::ZeroPad(2)]
        );
        assert_eq!(
            format(1, b'f', Some(2), false, Rounding::Upward)
                .unwrap()
                .body,
            b"0.01"
        );
    }

    #[test]
    fn source_piece_ranges_cover_output() {
        for bits in [
            0,
            1,
            3,
            0x3fd0000000000000,
            0x3ff0000000000000,
            0x4023000000000000,
            0x7fefffffffffffff,
        ] {
            for conversion in *b"fega" {
                for p in [None, Some(0), Some(1), Some(3), Some(20), Some(4096)] {
                    for alternate in [false, true] {
                        for rounding in [
                            Rounding::NearestEven,
                            Rounding::Upward,
                            Rounding::Downward,
                            Rounding::TowardZero,
                        ] {
                            let output =
                                source_format(bits, conversion, p, alternate, rounding).unwrap();
                            let mut rebuilt = output.value.body[..output.value.prefix_len].to_vec();
                            for piece in output.pieces {
                                match piece {
                                    Piece::Print(range) => {
                                        rebuilt.extend_from_slice(&output.value.body[range])
                                    }
                                    Piece::ZeroPad(count) => {
                                        rebuilt.extend(std::iter::repeat_n(b'0', count))
                                    }
                                }
                            }
                            assert_eq!(rebuilt, output.value.body);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn source_hex_effects_preserve_flags_and_reject_unmodeled_controls() {
        for mode in 0..4 {
            let control = 0x1f80 | (mode << 13) | 1;
            assert_eq!(
                hex_effects(1.5f64.to_bits(), Some(0), control),
                Ok(control | 32)
            );
            assert_eq!(hex_effects(1.5f64.to_bits(), Some(1), control), Ok(control));
            assert_eq!(hex_effects(1, None, control), Ok(control | 2));
            assert_eq!(hex_effects(3, Some(0), control), Ok(control | 34));
            assert_eq!(
                hex_effects(0x7ff0000000000001, Some(0), control),
                Ok(control)
            );
        }
        for control in [0x9f80, 0x1fc0, 0, 0x11f80] {
            assert_eq!(
                hex_effects(1.0f64.to_bits(), None, control),
                Err(Error::Control)
            );
        }
    }

    #[test]
    fn explicit_bounds_and_unsupported() {
        assert_eq!(
            format(0, b'q', None, false, Rounding::NearestEven),
            Err(Error::Unsupported)
        );
        assert_eq!(
            format(0, b'f', Some(usize::MAX), false, Rounding::NearestEven),
            Err(Error::PrecisionLimit)
        );
        let output = format(
            f64::MAX.to_bits(),
            b'f',
            Some(MAX_PRECISION),
            false,
            Rounding::NearestEven,
        )
        .unwrap();
        assert_eq!(output.body.len(), 309 + 1 + MAX_PRECISION);
    }
}
