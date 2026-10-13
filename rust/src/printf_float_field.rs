//! Binary64 conversion-to-output bridge; no executor, argument fetch or heap.
//! Caller fetches the argument first, prepares once, then publishes `mxcsr`
//! BEFORE emit. In particular, a later sink allocation failure does not undo FP
//! flags or the consumed argument. gdtoa internal allocations remain host scratch
//! and are not a model of guest Balloc/rv_alloc allocation/failure behavior.
use crate::{
    printf_float::{self, Piece, SourceFormatted},
    printf_float_effects,
    printf_output::{self, Output, Vector},
    printf_spec::{Conversion, Length},
};
#[derive(Debug)]
pub enum PrepareError {
    Format,
    Float(printf_float::Error),
    Effects(printf_float_effects::Error),
    Pieces,
}
#[derive(Debug)]
pub enum EmitError<E> {
    Limit,
    Allocation,
    Output(printf_output::Error<E>),
}
#[derive(Debug)]
pub struct Prepared {
    pub mxcsr: u32,
    converted: SourceFormatted,
    sign: Option<u8>,
    width: usize,
    left: bool,
    zero: bool,
}
impl Prepared {
    pub fn prepare(bits: u64, spec: Conversion, mxcsr: u32) -> Result<Self, PrepareError> {
        if !b"fFeEgGaA".contains(&spec.conversion)
            || !matches!(spec.length, Length::Default | Length::Long)
            || spec.flags.grouping
        {
            return Err(PrepareError::Format);
        }
        let rounding = match (mxcsr >> 13) & 3 {
            0 => printf_float::Rounding::NearestEven,
            1 => printf_float::Rounding::Downward,
            2 => printf_float::Rounding::Upward,
            _ => printf_float::Rounding::TowardZero,
        };
        let kind = spec.conversion.to_ascii_lowercase();
        let mxcsr = if kind == b'a' {
            printf_float::hex_effects(bits, spec.precision, mxcsr).map_err(PrepareError::Float)?
        } else {
            let precision = spec.precision.unwrap_or(6);
            if precision > printf_float::MAX_PRECISION {
                return Err(PrepareError::Float(printf_float::Error::PrecisionLimit));
            }
            let digits = match kind {
                b'e' => precision + 1,
                b'g' => precision.max(1),
                _ => precision,
            };
            printf_float_effects::decimal_effects(
                bits,
                if kind == b'f' { 3 } else { 2 },
                digits,
                mxcsr,
            )
            .map_err(PrepareError::Effects)?
        };
        let converted = printf_float::source_format(
            bits,
            spec.conversion,
            spec.precision,
            spec.flags.alternate,
            rounding,
        )
        .map_err(PrepareError::Float)?;
        let value = &converted.value;
        if value.prefix_len > value.body.len() {
            return Err(PrepareError::Pieces);
        }
        let mut offset = value.prefix_len;
        for piece in &converted.pieces {
            let count = match piece {
                Piece::Print(range) => {
                    if range.start != offset
                        || range.end < range.start
                        || range.end > value.body.len()
                    {
                        return Err(PrepareError::Pieces);
                    }
                    range.len()
                }
                Piece::ZeroPad(count) => *count,
            };
            offset = offset.checked_add(count).ok_or(PrepareError::Pieces)?;
        }
        if offset != value.body.len() {
            return Err(PrepareError::Pieces);
        }
        let sign = if value.suppress_sign {
            None
        } else if value.negative {
            Some(b'-')
        } else if spec.flags.plus {
            Some(b'+')
        } else if spec.flags.space {
            Some(b' ')
        } else {
            None
        };
        let zero = spec.flags.zero && !value.special;
        Ok(Self {
            mxcsr,
            converted,
            sign,
            width: spec.width,
            left: spec.flags.left,
            zero,
        })
    }
    /// Requires caller to publish self.mxcsr first. Reject the entire field's
    /// INT_MAX overflow before adding any field vectors, matching vfprintf.
    pub fn emit<E>(
        &self,
        output: &mut Output,
        mut sink: impl FnMut(&Vector) -> Result<(), E>,
    ) -> Result<(), EmitError<E>> {
        self.emit_batch(output, |batch| batch.submit(&mut sink))
    }
    /// Submit each source __sprint batch without flattening its vectors.
    pub fn emit_batch<E>(
        &self,
        output: &mut Output,
        mut sink: impl FnMut(printf_output::Batch<'_>) -> Result<(), E>,
    ) -> Result<(), EmitError<E>> {
        let value = &self.converted.value;
        let size = value
            .body
            .len()
            .checked_add(usize::from(self.sign.is_some()))
            .ok_or(EmitError::Limit)?;
        let field = size.max(self.width);
        if output
            .total()
            .checked_add(field)
            .is_none_or(|n| n > i32::MAX as usize)
        {
            return Err(EmitError::Limit);
        }
        let padding = field - size;
        if !self.left && !self.zero {
            output
                .pad_batch(b' ', padding, &mut sink)
                .map_err(EmitError::Output)?;
        }
        if let Some(sign) = self.sign {
            emit_print(output, &[sign], &mut sink)?;
        }
        if value.prefix_len != 0 {
            emit_print(output, &value.body[..value.prefix_len], &mut sink)?;
        }
        if !self.left && self.zero {
            output
                .pad_batch(b'0', padding, &mut sink)
                .map_err(EmitError::Output)?;
        }
        for piece in &self.converted.pieces {
            match piece {
                Piece::Print(range) => emit_print(output, &value.body[range.clone()], &mut sink)?,
                Piece::ZeroPad(count) => output
                    .pad_batch(b'0', *count, &mut sink)
                    .map_err(EmitError::Output)?,
            }
        }
        if self.left {
            output
                .pad_batch(b' ', padding, &mut sink)
                .map_err(EmitError::Output)?;
        }
        output.flush_batch(&mut sink).map_err(EmitError::Output)
    }
}
fn emit_print<E>(
    output: &mut Output,
    bytes: &[u8],
    sink: impl FnMut(printf_output::Batch<'_>) -> Result<(), E>,
) -> Result<(), EmitError<E>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(|_| EmitError::Allocation)?;
    copy.extend_from_slice(bytes);
    output
        .print_batch(Vector::Owned(copy), sink)
        .map_err(EmitError::Output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printf_spec::Flags;
    fn spec(conversion: u8, precision: usize) -> Conversion {
        Conversion {
            flags: Flags::default(),
            width: 0,
            precision: Some(precision),
            length: Length::Default,
            conversion,
        }
    }
    #[test]
    fn hexadecimal_prefix_sign_and_zero_padding_order() {
        let mut s = spec(b'a', 1);
        s.width = 12;
        s.flags.plus = true;
        s.flags.zero = true;
        let p = Prepared::prepare(1.5f64.to_bits(), s, 0x1f80).unwrap();
        let mut out = Output::new(100);
        let mut chunks = Vec::new();
        p.emit(&mut out, |v| {
            match v {
                Vector::Owned(v) => chunks.push(v.clone()),
                Vector::Pad { byte, length } => chunks.push(vec![*byte; *length]),
                _ => unreachable!(),
            };
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(chunks.concat(), b"+0x0001.8p+0");
        assert_eq!(chunks[0], b"+");
        assert_eq!(chunks[1], b"0x");
        assert_eq!(chunks[2], b"000");
    }
    #[test]
    fn effects_precede_failing_sink_and_tiny_source_output() {
        let p = Prepared::prepare(1, spec(b'f', 2), 0x5f80).unwrap();
        assert_eq!(p.mxcsr, 0x5fa2);
        let mut control = 0x5f80;
        assert_eq!(control, 0x5f80);
        control = p.mxcsr;
        let mut out = Output::new(100);
        assert!(p.emit(&mut out, |_| Err::<(), _>(7)).is_err());
        assert_eq!(control, 0x5fa2);
        let mut out = Output::new(100);
        let mut bytes = Vec::new();
        p.emit(&mut out, |v| {
            match v {
                Vector::Owned(v) => bytes.extend(v),
                Vector::Pad { byte, length } => bytes.extend(std::iter::repeat_n(*byte, *length)),
                _ => unreachable!(),
            };
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(bytes, b"0.00");
    }
    #[test]
    fn int_max_overflow_does_not_emit_or_flush_prior_literal() {
        let p = Prepared::prepare(1.0f64.to_bits(), spec(b'f', 2), 0x1f80).unwrap();
        let mut output = Output::new(usize::MAX);
        let previous = i32::MAX as usize - 2;
        output
            .print(
                Vector::Guest {
                    address: 0x1234,
                    length: previous,
                },
                |_| {
                    panic!("single queued literal must remain deferred");
                    #[allow(unreachable_code)]
                    Ok::<_, ()>(())
                },
            )
            .unwrap();
        let mut calls = 0;
        assert!(matches!(
            p.emit(&mut output, |_| {
                calls += 1;
                Ok::<_, ()>(())
            }),
            Err(EmitError::Limit)
        ));
        assert_eq!(calls, 0);
        assert_eq!(output.total(), previous);
        let mut retained = Vec::new();
        output
            .flush(|v| {
                retained.push(v.len());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(retained, [previous]);
    }
    #[test]
    fn nan_suppresses_plus_and_zero_padding() {
        let mut s = spec(b'G', 3);
        s.width = 6;
        s.flags.plus = true;
        s.flags.zero = true;
        let p = Prepared::prepare(0xfff0000000000001, s, 0x1f80).unwrap();
        assert_eq!(p.mxcsr, 0x1f80);
        let mut out = Output::new(100);
        let mut bytes = Vec::new();
        p.emit(&mut out, |v| {
            match v {
                Vector::Owned(v) => bytes.extend(v),
                Vector::Pad { byte, length } => bytes.extend(std::iter::repeat_n(*byte, *length)),
                _ => unreachable!(),
            };
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(bytes, b"   NAN");
    }
}
