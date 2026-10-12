//! FreeBSD 11.4 scanf floating tokenization in the C locale.
//! Derived from vfscanf.c parsefloat; see scan_float/FREEBSD-LICENSE.
use crate::float_parse::{
    effects::{parse_effects, Effects, EffectsError},
    Rounding,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Start,
    GotSign,
    Inf,
    Nan,
    Done,
    MaybeHex,
    Digits,
    Point,
    Fraction,
    Exponent,
    ExponentDigits,
}
/// Input begins after scanner whitespace skipping. Width0 means the512-byte
/// source buffer cap. Return the committed prefix; speculative suffixes such
/// as an incomplete exponent or NaN payload remain unread by the scanner.
pub fn token_len(input: &[u8], width: usize) -> usize {
    let end = input
        .len()
        .min(if width == 0 { 512 } else { width.min(512) });
    let (mut state, mut at, mut commit) = (State::Start, 0, 0);
    let (mut special, mut got_digit, mut hex) = (0, false, false);
    'scan: while at < end {
        let c = input[at];
        loop {
            match state {
                State::Start => {
                    state = State::GotSign;
                    if c == b'+' || c == b'-' {
                        break;
                    }
                }
                State::GotSign => {
                    match c {
                        b'0' => {
                            state = State::MaybeHex;
                            commit = at + 1;
                        }
                        b'i' | b'I' => state = State::Inf,
                        b'n' | b'N' => state = State::Nan,
                        _ => {
                            state = State::Digits;
                            continue;
                        }
                    }
                    break;
                }
                State::Inf => {
                    if special > 6 || !c.eq_ignore_ascii_case(&b"nfinity"[special]) {
                        break 'scan;
                    }
                    if special == 1 || special == 6 {
                        commit = at + 1;
                    }
                    special += 1;
                    break;
                }
                State::Nan => {
                    match special {
                        0 => {
                            if !c.eq_ignore_ascii_case(&b'a') {
                                break 'scan;
                            }
                        }
                        1 => {
                            if !c.eq_ignore_ascii_case(&b'n') {
                                break 'scan;
                            }
                            commit = at + 1;
                        }
                        2 => {
                            if c != b'(' {
                                break 'scan;
                            }
                        }
                        _ => {
                            if c == b')' {
                                commit = at + 1;
                                state = State::Done;
                            } else if !c.is_ascii_alphanumeric() && c != b'_' {
                                break 'scan;
                            }
                        }
                    }
                    special += 1;
                    break;
                }
                State::Done => break 'scan,
                State::MaybeHex => {
                    state = State::Digits;
                    if c == b'x' || c == b'X' {
                        hex = true;
                        break;
                    }
                    got_digit = true;
                }
                State::Digits => {
                    if c.is_ascii_digit() || (hex && c.is_ascii_hexdigit()) {
                        got_digit = true;
                        commit = at + 1;
                        break;
                    }
                    state = State::Point;
                }
                State::Point => {
                    state = State::Fraction;
                    if c == b'.' {
                        if got_digit {
                            commit = at + 1;
                        }
                        break;
                    }
                }
                State::Fraction => {
                    if (!hex && (c == b'e' || c == b'E')) || (hex && (c == b'p' || c == b'P')) {
                        if !got_digit {
                            break 'scan;
                        }
                        state = State::Exponent;
                    } else if c.is_ascii_digit() || (hex && c.is_ascii_hexdigit()) {
                        commit = at + 1;
                        got_digit = true;
                    } else {
                        break 'scan;
                    }
                    break;
                }
                State::Exponent => {
                    state = State::ExponentDigits;
                    if c == b'+' || c == b'-' {
                        break;
                    }
                }
                State::ExponentDigits => {
                    if !c.is_ascii_digit() {
                        break 'scan;
                    }
                    commit = at + 1;
                    break;
                }
            }
        }
        at += 1;
    }
    commit
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binary64 {
    pub consumed: usize,
    pub effects: Effects,
}
/// For unsuppressed %lf/%le/%lg/%la. Suppression uses token_len directly and
/// never invokes conversion or changes floating status/errno.
pub fn binary64(
    input: &[u8],
    width: usize,
    rounding: Rounding,
) -> Result<Option<Binary64>, EffectsError> {
    let consumed = token_len(input, width);
    if consumed == 0 {
        return Ok(None);
    }
    let effects = parse_effects(&input[..consumed], rounding)?;
    Ok(Some(Binary64 { consumed, effects }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn speculative_suffix_is_not_consumed() {
        for (s, n) in [
            ("1e+", 1),
            ("0x1p-", 3),
            ("+.", 0),
            ("0x.", 1),
            ("nan(12", 3),
            ("nan(a b)", 3),
            ("nan(a_b)", 8),
            ("infinite", 3),
            ("INFINITY!", 8),
            (".25x", 3),
            ("-0.e+3!", 6),
        ] {
            assert_eq!(token_len(s.as_bytes(), 0), n, "{s}");
        }
    }
    #[test]
    fn width_cap_and_double_conversion() {
        assert_eq!(token_len(b"1e2", 2), 1);
        assert_eq!(token_len(b"nan(12)", 6), 3);
        assert_eq!(token_len("1".repeat(600).as_bytes(), 0), 512);
        let v = binary64(b"1e+tail", 0, Rounding::NearestEven)
            .unwrap()
            .unwrap();
        assert_eq!((v.consumed, v.effects.parsed.bits), (1, 0x3ff0000000000000));
        let v = binary64(b"nan(123", 0, Rounding::NearestEven)
            .unwrap()
            .unwrap();
        assert_eq!((v.consumed, v.effects.parsed.bits), (3, 0x7ff8000000000000));
    }
}
