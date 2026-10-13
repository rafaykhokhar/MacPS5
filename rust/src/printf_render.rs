//! Single-pass sequential printf rendering into the private allocating sink.
//! Literal vectors stay deferred across %n; ordinary conversions flush before
//! scanning the next field. Errors do not flush queued source vectors.
//! Positional, extended80, locale grouping and %m remain explicit faults.
//! NumericOnly FP opts into byte rendering, not audited fenv/allocation timing.
use crate::{
    guest_memory::Memory,
    printf_args::{Class, Cursor, Value},
    printf_buffer::{Buffer, Error, Kind},
    printf_float,
    printf_output::{Batch, Output, Vector},
    printf_spec::{self, Conversion, Host, Item, Length, Parser},
};
#[derive(Clone, Copy, Debug)]
pub enum FloatPolicy {
    Reject,
    NumericOnly(printf_float::Rounding),
}
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub floats: FloatPolicy,
    pub output_limit: usize,
    pub argument_limit: usize,
    pub string_limit: usize,
    pub parser: printf_spec::Limits,
    /// Used only by source-required successful wide sizing side effects.
    pub errno_address: Option<u64>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            floats: FloatPolicy::Reject,
            output_limit: 65536,
            argument_limit: 4096,
            string_limit: 65536,
            parser: printf_spec::Limits::default(),
            errno_address: None,
        }
    }
}
fn fault(message: impl Into<String>) -> Error {
    Kind::Runtime(message.into()).into()
}
fn owned(bytes: &[u8]) -> Result<Vector, Error> {
    let mut v = Vec::new();
    v.try_reserve_exact(bytes.len())
        .map_err(|_| fault("Printf renderer host allocation"))?;
    v.extend_from_slice(bytes);
    Ok(Vector::Owned(v))
}
/// Rendering storage and submission boundary. Snapshots must cover the full
/// current owned allocation, including allocator padding, without borrowing memory.
pub(crate) trait Target<'a> {
    type Ranges: AsRef<[(u64, usize)]>;
    fn memory(&mut self) -> &mut Memory<'a>;
    fn private_ranges(&self) -> Result<Self::Ranges, Error>;
    fn validate_external_write(&self, address: u64, length: usize) -> Result<(), Error>;
    fn submit_batch(&mut self, batch: Batch<'_>) -> Result<(), Error>;
}
impl<'a> Target<'a> for Buffer<'_, 'a> {
    type Ranges = [(u64, usize); 1];
    fn memory(&mut self) -> &mut Memory<'a> {
        Buffer::memory(self)
    }
    fn private_ranges(&self) -> Result<Self::Ranges, Error> {
        Ok([self.private_span()?])
    }
    fn validate_external_write(&self, address: u64, length: usize) -> Result<(), Error> {
        Buffer::validate_external_write(self, address, length)
    }
    fn submit_batch(&mut self, batch: Batch<'_>) -> Result<(), Error> {
        batch.submit(|vector| match vector {
            Vector::Guest { address, length } => self.append_guest(*address, *length),
            Vector::Owned(bytes) => self.append(bytes),
            Vector::Pad { byte, length } => self.append(&[*byte; 16][..*length]),
        })
    }
}
fn output_error(error: crate::printf_output::Error<Error>) -> Error {
    match error {
        crate::printf_output::Error::Sink(e) => e,
        crate::printf_output::Error::Limit => Kind::OutputLimit.into(),
        crate::printf_output::Error::Poisoned => Kind::Poisoned.into(),
    }
}
fn print<'a>(
    output: &mut Output,
    buffer: &mut impl Target<'a>,
    vector: Vector,
) -> Result<(), Error> {
    output
        .print_batch(vector, |batch| buffer.submit_batch(batch))
        .map_err(output_error)
}
fn pad<'a>(
    output: &mut Output,
    buffer: &mut impl Target<'a>,
    byte: u8,
    length: usize,
) -> Result<(), Error> {
    output
        .pad_batch(byte, length, |batch| buffer.submit_batch(batch))
        .map_err(output_error)
}
fn flush<'a>(output: &mut Output, buffer: &mut impl Target<'a>) -> Result<(), Error> {
    output
        .flush_batch(|batch| buffer.submit_batch(batch))
        .map_err(output_error)
}
/// Explicit source-backed hexadecimal context; the default render entry remains
/// unchanged. State and pool mappings must belong to this executor.
pub struct HexContext<'a> {
    pub state: &'a mut crate::guest_gdtoa::State,
    pub retained: &'a mut crate::guest_gdtoa::Retained,
    pub mxcsr: &'a mut u32,
}
fn disjoint(address: u64, length: usize, ranges: &[(u64, usize)]) -> crate::Result<()> {
    let end = address
        .checked_add(length as u64)
        .ok_or("Printf private range overflow")?;
    if length != 0
        && ranges
            .iter()
            .any(|&(base, count)| address < base.saturating_add(count as u64) && base < end)
    {
        return Err("Printf source or destination aliases owned gdtoa storage".into());
    }
    Ok(())
}
fn ranges(state: &crate::guest_gdtoa::State) -> Result<Vec<(u64, usize)>, Error> {
    let mut result = Vec::new();
    for range in state.owned_ranges() {
        result
            .try_reserve(1)
            .map_err(|_| fault("Printf ownership snapshot allocation"))?;
        result.push(range);
    }
    Ok(result)
}
struct ArgumentMemory<'m, 'a, 'r> {
    memory: &'m mut Memory<'a>,
    ranges: &'r [(u64, usize)],
    output: &'r [(u64, usize)],
    wide: Option<(u64, usize)>,
}
impl crate::printf_args::Memory for ArgumentMemory<'_, '_, '_> {
    type Error = String;
    fn read(&self, address: u64, out: &mut [u8]) -> crate::Result<()> {
        disjoint(address, out.len(), self.ranges)?;
        disjoint(address, out.len(), self.output)?;
        if let Some(wide) = self.wide {
            disjoint(address, out.len(), &[wide])?;
        }
        out.copy_from_slice(self.memory.read(address, out.len())?);
        Ok(())
    }
    fn validate_write(&self, address: u64, length: usize) -> crate::Result<()> {
        disjoint(address, length, self.ranges)?;
        disjoint(address, length, self.output)?;
        if let Some(wide) = self.wide {
            disjoint(address, length, &[wide])?;
        }
        self.memory.validate_write(address, length)
    }
    fn write(&mut self, address: u64, bytes: &[u8]) -> crate::Result<()> {
        self.validate_write(address, bytes.len())?;
        self.memory.write(address, bytes)
    }
}
struct Context<'b, T, G> {
    buffer: &'b mut T,
    cursor: Cursor,
    guard: &'b mut G,
    wide: Option<u64>,
    gdtoa_ranges: Vec<(u64, usize)>,
}
impl<'a, T: Target<'a>, G: FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>> Context<'_, T, G> {
    fn fetch(&mut self, class: Class) -> Result<Value, Error> {
        let output_ranges = self.buffer.private_ranges()?;
        let wide = self
            .wide
            .map(|base| {
                self.buffer
                    .memory()
                    .heap_allocation_size(base)
                    .map(|length| (base, length))
            })
            .transpose()?;
        if !self.gdtoa_ranges.is_empty() {
            let guard = &mut self.guard;
            let mut memory = ArgumentMemory {
                memory: self.buffer.memory(),
                ranges: &self.gdtoa_ranges,
                output: output_ranges.as_ref(),
                wide,
            };
            return self
                .cursor
                .fetch(&mut memory, class, |m, a, n| guard(m.memory, a, n))
                .map_err(|e| fault(format!("Printf argument fault: {e:?}")));
        }
        let guard = &mut self.guard;
        self.cursor
            .fetch(self.buffer.memory(), class, |memory, address, count| {
                let end = address
                    .checked_add(count as u64)
                    .ok_or("Printf cursor write overflow")?;
                if output_ranges
                    .as_ref()
                    .iter()
                    .any(|&(base, length)| address < base + length as u64 && base < end)
                {
                    return Err("Printf cursor aliases private output storage".into());
                }
                if wide.is_some_and(|(base, length)| address < base + length as u64 && base < end) {
                    return Err("Printf cursor aliases private wide conversion storage".into());
                }
                guard(memory, address, count)
            })
            .map_err(|e| fault(format!("Printf argument fault: {e:?}")))
    }

    fn external_write(&mut self, address: u64, length: usize) -> Result<(), Error> {
        disjoint(address, length, &self.gdtoa_ranges)?;
        self.buffer.validate_external_write(address, length)?;
        if let Some(base) = self.wide {
            let size = self.buffer.memory().heap_allocation_size(base)?;
            let end = address
                .checked_add(length as u64)
                .ok_or_else(|| fault("Printf wide guard overflow"))?;
            if length != 0 && address < base + size as u64 && base < end {
                return Err(Kind::PrivateStorageAlias.into());
            }
        }
        (self.guard)(self.buffer.memory(), address, length)?;
        Ok(())
    }
    fn release_wide(&mut self) -> Result<(), Error> {
        if let Some(address) = self.wide {
            if let Err(message) = self.buffer.memory().free(address) {
                let mut error = fault("Printf wide temporary cleanup failed");
                error.cleanup = Some(crate::printf_buffer::CleanupFault { address, message });
                return Err(error);
            }
            self.wide = None;
        }
        Ok(())
    }
    fn wide_unit(&mut self, source: u64, index: usize, budget: usize) -> Result<u16, Error> {
        if index >= budget {
            return Err(fault("Printf wide read budget"));
        }
        let address = index
            .checked_mul(2)
            .and_then(|offset| source.checked_add(offset as u64))
            .ok_or_else(|| fault("Printf wide source overflow"))?;
        if let Some(base) = self.wide {
            let size = self.buffer.memory().heap_allocation_size(base)?;
            let end = address
                .checked_add(2)
                .ok_or_else(|| fault("Printf wide source overflow"))?;
            if address < base + size as u64 && base < end {
                return Err(Kind::PrivateStorageAlias.into());
            }
        }
        disjoint(address, 2, &self.gdtoa_ranges)?;
        if !self.gdtoa_ranges.is_empty() {
            disjoint(address, 2, self.buffer.private_ranges()?.as_ref())?;
        }
        Ok(self.buffer.memory().load(address, 2)? as u16)
    }
    fn wide_string(
        &mut self,
        source: u64,
        precision: Option<usize>,
        options: Options,
    ) -> Result<Vector, Error> {
        if source == 0 {
            return owned(&b"(null)"[..precision.unwrap_or(6).min(6)]);
        }
        let capacity = match precision {
            Some(n) if n < 128 => n,
            None => {
                let mut n = 0;
                loop {
                    let unit = self.wide_unit(source, n, options.string_limit)?;
                    if unit > 255 {
                        return Err(Kind::Libc(86).into());
                    }
                    if unit == 0 {
                        break;
                    }
                    n += 1;
                }
                n
            }
            Some(precision) => {
                let mut n = 0;
                loop {
                    // Source calls wcrtomb before the capacity check. NUL is a
                    // one-byte conversion here, so this prepass does not stop
                    // at NUL. The subsequent wcsrtombs copy does stop there.
                    let unit = self.wide_unit(source, n, options.string_limit)?;
                    if unit > 255 {
                        let errno = options
                            .errno_address
                            .ok_or_else(|| fault("Printf wide sizing requires errno context"))?;
                        self.external_write(errno, 4)?;
                        self.buffer.memory().store(errno, 86, 4)?;
                        break;
                    }
                    if n == precision {
                        break;
                    }
                    n += 1;
                }
                n
            }
        };
        let count = capacity.checked_add(1).ok_or(Kind::OutputLimit)?;
        let address = self
            .buffer
            .memory()
            .malloc(count as u64)
            .map_err(Kind::Allocation)?;
        self.wide = Some(address);
        let mut length = 0;
        while length < capacity {
            let unit = self.wide_unit(source, length, options.string_limit)?;
            if unit > 255 {
                return Err(Kind::Libc(86).into());
            }
            self.buffer
                .memory()
                .store(address + length as u64, unit.into(), 1)?;
            if unit == 0 {
                break;
            }
            length += 1;
        }
        self.buffer.memory().store(address + length as u64, 0, 1)?;
        Ok(Vector::Guest { address, length })
    }
    fn gp(&mut self) -> Result<u64, Error> {
        match self.fetch(Class::Gp)? {
            Value::Gp(v) => Ok(v),
            _ => unreachable!("requested GP class"),
        }
    }
}
impl<'a, T: Target<'a>, G: FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>> Host
    for Context<'_, T, G>
{
    type Fault = Error;
    fn read_byte(&mut self, address: u64) -> Result<u8, Error> {
        disjoint(address, 1, &self.gdtoa_ranges)?;
        if !self.gdtoa_ranges.is_empty() {
            disjoint(address, 1, self.buffer.private_ranges()?.as_ref())?;
        }
        Ok(self.buffer.memory().load(address, 1)? as u8)
    }
    fn fetch_gp(&mut self, positional: Option<usize>) -> Result<u64, Error> {
        if positional.is_some() {
            return Err(fault("Positional printf arguments unsupported"));
        }
        self.gp()
    }
}
fn bits(length: Length) -> Result<u32, Error> {
    Ok(match length {
        Length::Default => 32,
        Length::Char => 8,
        Length::Short => 16,
        Length::Long | Length::LongLong | Length::IntMax | Length::Size | Length::PtrDiff => 64,
        Length::LongDouble => return Err(fault("Extended80 integer printf length unsupported")),
    })
}
struct Field {
    body: Vector,
    sign: Option<u8>,
    prefix: Vector,
    precision_zeros: usize,
    zero: bool,
}
fn integer(spec: Conversion, raw: u64) -> Result<Field, Error> {
    let pointer = spec.conversion == b'p';
    let width = if pointer { 64 } else { bits(spec.length)? };
    let mask = u64::MAX >> (64 - width);
    let original = raw & mask;
    let mut value = original;
    let signed = matches!(spec.conversion, b'd' | b'i');
    let sign = if signed {
        if value & (1u64 << (width - 1)) != 0 {
            value = value.wrapping_neg() & mask;
            Some(b'-')
        } else if spec.flags.plus {
            Some(b'+')
        } else if spec.flags.space {
            Some(b' ')
        } else {
            None
        }
    } else {
        None
    };
    let base = match spec.conversion {
        b'o' => 8,
        b'x' | b'X' | b'p' => 16,
        _ => 10,
    };
    let alphabet = if spec.conversion == b'X' {
        b"0123456789ABCDEF"
    } else {
        b"0123456789abcdef"
    };
    let mut digits = [0u8; 64];
    let mut at = 64;
    if value != 0 || spec.precision != Some(0) || (base == 8 && spec.flags.alternate) {
        loop {
            at -= 1;
            digits[at] = alphabet[(value % base) as usize];
            value /= base;
            if value == 0 {
                break;
            }
        }
    }
    if base == 8 && spec.flags.alternate && (at == 64 || digits[at] != b'0') {
        at -= 1;
        digits[at] = b'0';
    }
    let prefix = if pointer || (base == 16 && spec.flags.alternate && original != 0) {
        if spec.conversion == b'X' {
            b"0X".as_slice()
        } else {
            b"0x".as_slice()
        }
    } else {
        b"".as_slice()
    };
    Ok(Field {
        body: owned(&digits[at..])?,
        sign,
        prefix: owned(prefix)?,
        precision_zeros: spec.precision.unwrap_or(0).saturating_sub(64 - at),
        zero: spec.flags.zero && spec.precision.is_none(),
    })
}
fn emit<'a>(
    output: &mut Output,
    buffer: &mut impl Target<'a>,
    spec: Conversion,
    field: Field,
) -> Result<(), Error> {
    let length = field
        .body
        .len()
        .checked_add(field.prefix.len())
        .and_then(|n| n.checked_add(usize::from(field.sign.is_some())))
        .and_then(|n| n.checked_add(field.precision_zeros))
        .ok_or(Kind::OutputLimit)?;
    let padding = spec.width.saturating_sub(length);
    if !spec.flags.left && !field.zero {
        pad(output, buffer, b' ', padding)?;
    }
    if let Some(sign) = field.sign {
        print(output, buffer, owned(&[sign])?)?;
    }
    if !field.prefix.is_empty() {
        print(output, buffer, field.prefix)?;
    }
    if !spec.flags.left && field.zero {
        pad(output, buffer, b'0', padding)?;
    }
    pad(output, buffer, b'0', field.precision_zeros)?;
    print(output, buffer, field.body)?;
    if spec.flags.left {
        pad(output, buffer, b' ', padding)?;
    }
    flush(output, buffer)
}
/// Render once inside printf_buffer::run. Caller combines guard with its live
/// return-frame rules; this renderer also excludes private allocation storage.
pub fn render(
    buffer: &mut Buffer<'_, '_>,
    format: u64,
    va: u64,
    options: Options,
    guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
) -> Result<(), Error> {
    render_target(buffer, format, va, options, None, guard).map(|_| ())
}
/// Source-backed a/A rendering, with all gdtoa ranges excluded from guest
/// format/argument/count accesses. Decimal/extended floats stay unsupported.
/// The caller must also guard final result-slot publication against gdtoa ranges.
pub fn render_with_hdtoa(
    buffer: &mut Buffer<'_, '_>,
    format: u64,
    va: u64,
    options: Options,
    hex: HexContext<'_>,
    guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
) -> Result<(), Error> {
    render_target(buffer, format, va, options, Some(hex), guard).map(|_| ())
}
/// Render through an owned target, returning the logical count after cleanup.
pub(crate) fn render_target<'a>(
    buffer: &mut impl Target<'a>,
    format: u64,
    va: u64,
    options: Options,
    mut hex: Option<HexContext<'_>>,
    mut guard: impl FnMut(&Memory<'_>, u64, usize) -> crate::Result<()>,
) -> Result<usize, Error> {
    let initial_ranges = hex
        .as_ref()
        .map(|h| ranges(h.state))
        .transpose()?
        .unwrap_or_default();
    let mut parser = Parser::new(format, options.parser)
        .map_err(|e| fault(format!("Printf parser bound: {e:?}")))?;
    let mut output = Output::new(options.output_limit.min(i32::MAX as usize));
    let mut context = Context {
        buffer,
        cursor: Cursor::new(va, options.argument_limit),
        guard: &mut guard,
        wide: None,
        gdtoa_ranges: initial_ranges,
    };
    let result = (|| {
        while let Some(item) = parser.next(&mut context).map_err(|e| match e {
            printf_spec::Error::Guest(e) => e,
            other => fault(format!("Printf parser fault: {other:?}")),
        })? {
            let spec = match item {
                Item::Literal(span) => {
                    print(
                        &mut output,
                        context.buffer,
                        Vector::Guest {
                            address: span.address,
                            length: span.len,
                        },
                    )?;
                    continue;
                }
                Item::Conversion(spec) => spec,
            };
            if spec.flags.grouping {
                return Err(fault("Printf locale grouping unsupported"));
            }
            if spec.length == Length::LongDouble {
                return Err(fault("Printf extended80 rendering unsupported"));
            }
            if spec.conversion == b'n' {
                let width = (bits(spec.length)? / 8) as usize;
                let address = context.gp()?;
                context.external_write(address, width)?;
                context
                    .buffer
                    .memory()
                    .store(address, output.total() as u64, width as u64)?;
                continue; // Source %n bypasses ordinary conversion flush.
            }
            if let Some(h) = hex.as_mut()
                && b"fFeEgGaA".contains(&spec.conversion)
            {
                if !b"aA".contains(&spec.conversion) {
                    return Err(fault("Source decimal gdtoa allocation sequencing pending"));
                }
                let output_ranges = context.buffer.private_ranges()?;
                let wide = context
                    .wide
                    .map(|base| {
                        context
                            .buffer
                            .memory()
                            .heap_allocation_size(base)
                            .map(|n| (base, n))
                    })
                    .transpose()?;
                let cursor = &mut context.cursor;
                let guard = &mut context.guard;
                let private = &context.gdtoa_ranges;
                let converted = crate::guest_hdtoa::convert(
                    h.state,
                    h.retained,
                    context.buffer.memory(),
                    spec,
                    h.mxcsr,
                    |memory| {
                        let mut memory = ArgumentMemory {
                            memory,
                            ranges: private,
                            output: output_ranges.as_ref(),
                            wide,
                        };
                        match cursor
                            .fetch(&mut memory, Class::Binary64, |m, a, n| {
                                guard(m.memory, a, n)
                            })
                            .map_err(|e| fault(format!("Printf argument fault: {e:?}")))?
                        {
                            Value::Binary64(bits) => Ok(bits),
                            _ => Err(fault("Printf binary64 argument invariant")),
                        }
                    },
                )
                .map_err(|e| match e {
                    crate::guest_hdtoa::Error::Fetch(e) => e,
                    other => fault(format!("Printf hdtoa fault: {other:?}")),
                })?;
                context.gdtoa_ranges = ranges(h.state)?;
                converted
                    .field
                    .emit_batch(&mut output, |batch| context.buffer.submit_batch(batch))
                    .map_err(|e| match e {
                        crate::printf_float_field::EmitError::Output(e) => output_error(e),
                        crate::printf_float_field::EmitError::Limit => Kind::OutputLimit.into(),
                        crate::printf_float_field::EmitError::Allocation => {
                            fault("Printf float vector host allocation")
                        }
                    })?;
                continue;
            }
            let field = if b"diuoxXp".contains(&spec.conversion) {
                integer(spec, context.gp()?)?
            } else if b"fFeEgGaA".contains(&spec.conversion) {
                let FloatPolicy::NumericOnly(rounding) = options.floats else {
                    return Err(fault(
                        "Printf floating effects require explicit numeric-only policy; target fenv pending",
                    ));
                };
                if !matches!(spec.length, Length::Default | Length::Long) {
                    return Err(fault("Printf floating length unsupported"));
                }
                let Value::Binary64(bits) = context.fetch(Class::Binary64)? else {
                    unreachable!("requested binary64")
                };
                let f = printf_float::format(
                    bits,
                    spec.conversion,
                    spec.precision,
                    spec.flags.alternate,
                    rounding,
                )
                .map_err(|e| fault(format!("Printf floating output fault: {e:?}")))?;
                let sign = if f.suppress_sign {
                    None
                } else if f.negative {
                    Some(b'-')
                } else if spec.flags.plus {
                    Some(b'+')
                } else if spec.flags.space {
                    Some(b' ')
                } else {
                    None
                };
                // Numerical-only path: full dtoa vector segmentation remains an
                // explicit policy boundary until source pieces are available.
                Field {
                    prefix: owned(&f.body[..f.prefix_len])?,
                    body: owned(&f.body[f.prefix_len..])?,
                    sign,
                    precision_zeros: 0,
                    zero: spec.flags.zero && !f.special,
                }
            } else if matches!(spec.conversion, b'c' | b's' | b'%') {
                if !matches!(spec.length, Length::Default | Length::Long)
                    || (spec.conversion == b'%' && spec.length != Length::Default)
                {
                    return Err(fault("Printf modified text conversion unsupported"));
                }
                let body = match spec.conversion {
                    b'c' if spec.length == Length::Long => {
                        let unit = context.gp()? as u16;
                        if unit > 255 {
                            return Err(Kind::Libc(86).into());
                        }
                        owned(&[unit as u8])?
                    }
                    b's' if spec.length == Length::Long => {
                        // Source releases the preceding wide buffer before fetching
                        // even a NULL new argument; other conversions retain it.
                        context.release_wide()?;
                        let source = context.gp()?;
                        context.wide_string(source, spec.precision, options)?
                    }
                    b'%' => owned(b"%")?,
                    b'c' => owned(&[context.gp()? as u8])?,
                    _ => {
                        let address = context.gp()?;
                        if address == 0 {
                            owned(&b"(null)"[..spec.precision.unwrap_or(6).min(6)])?
                        } else {
                            let maximum = spec
                                .precision
                                .unwrap_or(options.string_limit)
                                .min(options.string_limit);
                            let mut length = 0;
                            while length < maximum {
                                let at = address
                                    .checked_add(length as u64)
                                    .ok_or_else(|| fault("Printf string address overflow"))?;
                                if context.read_byte(at)? == 0 {
                                    break;
                                }
                                length += 1;
                            }
                            if length == maximum && spec.precision.is_none_or(|p| p > maximum) {
                                return Err(fault("Printf string read budget"));
                            }
                            Vector::Guest { address, length }
                        }
                    }
                };
                Field {
                    body,
                    sign: None,
                    prefix: owned(b"")?,
                    precision_zeros: 0,
                    zero: spec.flags.zero,
                }
            } else {
                return Err(fault("Printf conversion unsupported in renderer"));
            };
            emit(&mut output, context.buffer, spec, field)?;
        }
        flush(&mut output, context.buffer)
    })();
    // vfprintf cleanup releases dtoaresult before convbuf. Attempt both even
    // after a runtime cleanup fault, preserving every retained address in diagnostics.
    let mut result = result;
    if let Some(h) = hex.as_mut() {
        let address = h.retained.current().map(|s| s.address - 4);
        if let Err(cleanup) = h.retained.release(h.state, context.buffer.memory()) {
            let message = format!("Printf gdtoa cleanup failed at {address:?}: {cleanup:?}");
            let mut error = result.err().unwrap_or_else(|| fault(message.clone()));
            if let Some(existing) = error.cleanup.as_mut() {
                existing.message.push_str("; ");
                existing.message.push_str(&message);
            } else {
                error.cleanup = Some(crate::printf_buffer::CleanupFault {
                    address: address.unwrap_or(0),
                    message,
                });
            }
            result = Err(error);
        }
    }
    if let Err(cleanup) = context.release_wide() {
        result = match result {
            Ok(()) => Err(cleanup),
            Err(mut error) => {
                if let Some(extra) = cleanup.cleanup {
                    if let Some(existing) = error.cleanup.as_mut() {
                        existing.message.push_str(&format!(
                            "; wide cleanup retained {}: {}",
                            extra.address, extra.message
                        ));
                    } else {
                        error.cleanup = Some(extra);
                    }
                }
                Err(error)
            }
        };
    }
    result.map(|()| output.total())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{guest_memory::OwnedMappings, printf_buffer};
    const SLOT: u64 = 0x1000;
    const FMT: u64 = 0x1100;
    const VA: u64 = 0x1200;
    const REGS: u64 = 0x1300;
    const DATA: u64 = 0x1500;
    fn setup(format: &[u8], gp: &[u64], fp: &[u64]) -> Vec<u8> {
        let mut bytes = vec![0; 4096];
        bytes[0x100..0x100 + format.len()].copy_from_slice(format);
        bytes[0x204..0x208].copy_from_slice(&48u32.to_le_bytes());
        bytes[0x208..0x210].copy_from_slice(&(REGS + 176).to_le_bytes());
        bytes[0x210..0x218].copy_from_slice(&REGS.to_le_bytes());
        for (i, v) in gp.iter().enumerate() {
            let p = if i < 6 {
                0x300 + i * 8
            } else {
                0x300 + 176 + (i - 6) * 8
            };
            bytes[p..p + 8].copy_from_slice(&v.to_le_bytes());
        }
        for (i, v) in fp.iter().enumerate() {
            let p = 0x300 + 48 + i * 16;
            bytes[p..p + 8].copy_from_slice(&v.to_le_bytes());
        }
        bytes
    }
    fn guard(_: &Memory<'_>, _: u64, _: usize) -> crate::Result<()> {
        Ok(())
    }
    struct Trace<'b, 'm, 'a> {
        buffer: &'b mut Buffer<'m, 'a>,
        batches: Vec<(usize, Vec<usize>)>,
    }
    impl<'a> Target<'a> for Trace<'_, '_, 'a> {
        type Ranges = [(u64, usize); 1];
        fn memory(&mut self) -> &mut Memory<'a> {
            self.buffer.memory()
        }
        fn private_ranges(&self) -> Result<Self::Ranges, Error> {
            self.buffer.private_ranges()
        }
        fn validate_external_write(&self, a: u64, n: usize) -> Result<(), Error> {
            self.buffer.validate_external_write(a, n)
        }
        fn submit_batch(&mut self, batch: Batch<'_>) -> Result<(), Error> {
            self.batches
                .push((batch.residual(), batch.vectors().map(Vector::len).collect()));
            self.buffer.submit_batch(batch)
        }
    }
    #[test]
    fn target_batches_keep_count_write_before_deferred_literal_read() {
        let mut data = setup(b"abc%hhn%040uZ\0", &[FMT, 7], &[]);
        let mut owner = OwnedMappings::default();
        let mut memory = Memory::default();
        memory.add(SLOT, &mut data, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut memory, SLOT, 65536, guard, |buffer| {
            let mut target = Trace {
                buffer,
                batches: Vec::new(),
            };
            assert_eq!(
                render_target(&mut target, FMT, VA, Options::default(), None, guard)?,
                44
            );
            assert_eq!(target.batches, [(43, vec![3, 16, 16, 7, 1]), (1, vec![1])]);
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 44);
        let address = memory.load(SLOT, 8).unwrap();
        assert_eq!(memory.read(address, 3).unwrap(), &[3, b'b', b'c']);
        assert_eq!(memory.read(address + 42, 3).unwrap(), b"7Z\0");
    }
    #[test]
    fn source_hex_uses_target_batches_and_releases_retained_storage() {
        let mut data = setup(b"L%300.140aR\0", &[], &[1.0f64.to_bits()]);
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut memory = Memory::default();
        memory.add(SLOT, &mut data, true, false).unwrap();
        memory
            .add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        memory.attach_owned(&mut owner).unwrap();
        let count = printf_buffer::run(&mut memory, SLOT, 65536, guard, |buffer| {
            let mut target = Trace {
                buffer,
                batches: Vec::new(),
            };
            let count = render_target(
                &mut target,
                FMT,
                VA,
                Options::default(),
                Some(HexContext {
                    state,
                    retained: &mut retained,
                    mxcsr: &mut control,
                }),
                guard,
            )?;
            assert_eq!(count, 302);
            assert_eq!(target.batches.len(), 3);
            assert_eq!(target.batches[0].1.len(), 8);
            assert_eq!(target.batches.last(), Some(&(1, vec![1])));
            assert_eq!(target.batches.iter().map(|b| b.0).sum::<usize>(), count);
            assert!(target.batches.iter().flat_map(|b| &b.1).any(|&n| n == 16));
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 302);
        let address = memory.load(SLOT, 8).unwrap();
        assert_eq!(memory.read(address + 154, 4).unwrap(), b"0x1.");
        assert_eq!(memory.read(address + 298, 5).unwrap(), b"p+0R\0");
        assert!(retained.current().is_none());
        assert_eq!(control, 0x1f80);
        assert_eq!(state.owned_ranges().count(), 1);
    }
    #[test]
    fn source_hex_cursor_flags_output_and_retained_cleanup() {
        let mut data = setup(b"%+012.1a|%.0a%n\0", &[DATA], &[1.5f64.to_bits(), 3]);
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        m.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut m, SLOT, 65536, guard, |b| {
            render_with_hdtoa(
                b,
                FMT,
                VA,
                Options::default(),
                HexContext {
                    state,
                    retained: &mut retained,
                    mxcsr: &mut control,
                },
                guard,
            )
        })
        .unwrap();
        let expected = b"+0x0001.8p+0|0x1p-1072";
        assert_eq!(n as usize, expected.len());
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, expected.len()).unwrap(), expected);
        assert_eq!(m.load(DATA, 4).unwrap(), expected.len() as u64);
        assert_eq!(m.load(VA + 4, 4).unwrap(), 80);
        assert_eq!(control, 0x1fa2);
        assert!(retained.current().is_none());
        assert_eq!(state.pool_used(), 32);
        assert!(m.store(0x9000, 0, 8).is_err()); // static pool stays runtime-owned after cleanup.
    }
    #[test]
    fn source_hex_large_field_preserves_output_growth_and_frees_digit_heap() {
        let mut data = setup(b"%.4096a\0", &[], &[1.0f64.to_bits()]);
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        m.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut m, SLOT, 65536, guard, |b| {
            render_with_hdtoa(
                b,
                FMT,
                VA,
                Options::default(),
                HexContext {
                    state,
                    retained: &mut retained,
                    mxcsr: &mut control,
                },
                guard,
            )?;
            assert_eq!(b.logical_capacity(), 4229);
            Ok(())
        })
        .unwrap();
        assert_eq!(n, 4103);
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, 4).unwrap(), b"0x1.");
        assert_eq!(m.read(p + 4100, 4).unwrap(), b"p+0\0");
        assert_eq!(state.owned_ranges().count(), 1);
        assert_eq!(state.pool_used(), 0);
        assert!(retained.current().is_none());
    }
    #[test]
    fn source_hex_count_alias_fault_keeps_flags_and_cleans_output() {
        let mut data = setup(b"%.0a%n\0", &[0x9004], &[3]);
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        m.attach_owned(&mut owner).unwrap();
        let result = printf_buffer::run(&mut m, SLOT, 65536, guard, |b| {
            render_with_hdtoa(
                b,
                FMT,
                VA,
                Options::default(),
                HexContext {
                    state,
                    retained: &mut retained,
                    mxcsr: &mut control,
                },
                guard,
            )
        });
        assert!(result.is_err());
        assert_eq!(control, 0x1fa2);
        assert_eq!(m.load(VA + 4, 4).unwrap(), 64);
        assert_eq!(m.load(VA, 4).unwrap(), 8);
        assert_eq!(m.load(SLOT, 8).unwrap(), 0);
        assert!(m.heap_allocation_size(0x400000000).is_err());
        assert!(retained.current().is_none());
        assert_eq!(m.load(0x900c, 4).unwrap(), 1); // freedtoa restored the cache header.
    }
    #[test]
    fn source_hex_payload_alias_fails_before_cursor_advance() {
        let mut data = setup(b"%a\0", &[], &[]);
        data[0x210..0x218].copy_from_slice(&(0x9000u64 - 48).to_le_bytes());
        let mut owner = OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(0x9000).unwrap();
        let (pool, state) = store.split_mut();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut control = 0x1f80;
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.add_runtime_data(0x9000, pool, state.runtime_owner())
            .unwrap();
        m.attach_owned(&mut owner).unwrap();
        assert!(
            printf_buffer::run(&mut m, SLOT, 65536, guard, |b| render_with_hdtoa(
                b,
                FMT,
                VA,
                Options::default(),
                HexContext {
                    state,
                    retained: &mut retained,
                    mxcsr: &mut control
                },
                guard
            ))
            .is_err()
        );
        assert_eq!(m.load(VA + 4, 4).unwrap(), 48);
        assert_eq!(control, 0x1f80);
        assert_eq!(state.pool_used(), 0);
    }

    #[test]
    fn mixed_gp_fp_stars_and_narrowing_update_cursor_once() {
        let mut data = setup(
            b"%+08d|%*.*f|%hhd|%#x\0",
            &[(-12i64) as u64, 8, 2, 255, 42],
            &[2.5f64.to_bits()],
        );
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let options = Options {
            floats: FloatPolicy::NumericOnly(printf_float::Rounding::NearestEven),
            ..Options::default()
        };
        let n = printf_buffer::run(&mut m, SLOT, 65536, guard, |b| {
            render(b, FMT, VA, options, guard)
        })
        .unwrap();
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, n as usize).unwrap(), b"-0000012|    2.50|-1|0x2a");
        assert_eq!(m.load(VA, 4).unwrap(), 40);
        assert_eq!(m.load(VA + 4, 4).unwrap(), 64);
    }
    #[test]
    fn n_changes_deferred_literal_before_flush() {
        let mut data = setup(b"ABC%hhn\0", &[FMT], &[]);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap();
        assert_eq!(n, 3);
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, 4).unwrap(), b"\x03BC\0");
    }
    #[test]
    fn ordinary_conversion_flushes_before_n_rewrites_format() {
        let mut data = setup(b"ABC%c%hhn\0", &[b'!' as u64, FMT], &[]);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap();
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, 5).unwrap(), b"ABC!\0");
        assert_eq!(m.load(FMT, 1).unwrap(), 4);
    }
    #[test]
    fn string_precision_null_pointer_and_growth() {
        let mut data = setup(
            b"%300s|%.0s|%.3s|%.0p|%#.0o|%c\0",
            &[DATA, u64::MAX, 0, 0, 0, 0],
            &[],
        );
        data[0x500..0x502].copy_from_slice(b"a\0");
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut m, SLOT, 1000, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap();
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, 299).unwrap(), [b' '; 299]);
        assert_eq!(
            m.read(p + 299, n as usize - 299 + 1).unwrap(),
            b"a||(nu|0x|0|\0\0"
        );
    }
    #[test]
    fn guard_failures_do_not_publish_or_flush_queued_literals() {
        let mut data = setup(b"ABC%n\0", &[DATA], &[]);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), |_, address, _| {
                if address == DATA {
                    Err("live frame".into())
                } else {
                    Ok(())
                }
            })
        })
        .unwrap_err();
        assert!(matches!(e.kind, Kind::Runtime(_)));
        assert_eq!(m.load(SLOT, 8).unwrap(), 0);
        assert_eq!(m.load(VA, 4).unwrap(), 8);
        assert_eq!(m.load(DATA, 4).unwrap(), 0);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn float_effects_and_other_unfinished_formats_are_explicit_boundaries() {
        for format in [b"%f\0".as_slice(), b"%Lf\0", b"%1$d\0", b"%'d\0"] {
            let mut data = setup(format, &[DATA], &[1.0f64.to_bits()]);
            let mut owner = OwnedMappings::default();
            let mut m = Memory::default();
            m.add(SLOT, &mut data, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
                render(b, FMT, VA, Options::default(), guard)
            })
            .unwrap_err();
            assert!(matches!(e.kind, Kind::Runtime(_)));
            assert_eq!(m.load(VA, 4).unwrap(), 0);
            assert_eq!(m.load(VA + 4, 4).unwrap(), 48);
        }
    }
    #[test]
    fn count_widths_and_register_overflow_share_the_cursor() {
        let pointers = [
            DATA,
            DATA + 8,
            DATA + 16,
            DATA + 24,
            DATA + 32,
            DATA + 40,
            DATA + 48,
        ];
        let mut data = setup(b"ABCDE%hhn%hn%n%ln%lln%zn%tn\0", &pointers, &[]);
        data[0x500..0x540].fill(0xa5);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        assert_eq!(
            printf_buffer::run(&mut m, SLOT, 100, guard, |b| render(
                b,
                FMT,
                VA,
                Options::default(),
                guard
            ))
            .unwrap(),
            5
        );
        for (index, width) in [1, 2, 4, 8, 8, 8, 8].into_iter().enumerate() {
            assert_eq!(m.load(pointers[index], width).unwrap(), 5);
            if width < 8 {
                assert_eq!(m.load(pointers[index] + width, 1).unwrap(), 0xa5);
            }
        }
        assert_eq!(m.load(VA, 4).unwrap(), 48);
        assert_eq!(m.load(VA + 8, 8).unwrap(), REGS + 184);
    }
    #[test]
    fn cursor_and_count_guards_reject_private_output_storage() {
        for cursor_alias in [false, true] {
            let mut data = setup(b"%n\0", &[DATA], &[]);
            let mut owner = OwnedMappings::default();
            let mut m = Memory::default();
            m.add(SLOT, &mut data, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
                let (private, _) = b.private_span()?;
                let va = if cursor_alias {
                    b.memory().store(private + 16, REGS, 8)?;
                    private
                } else {
                    b.memory().store(REGS, private + 127, 8)?;
                    VA
                };
                render(b, FMT, va, Options::default(), guard)
            })
            .unwrap_err();
            assert!(matches!(
                e.kind,
                Kind::Runtime(_) | Kind::PrivateStorageAlias
            ));
            assert!(e.cleanup.is_none());
            assert_eq!(m.load(SLOT, 8).unwrap(), 0);
            drop(m);
            assert_eq!(owner.bytes_allocated(), 0);
        }
    }
    #[test]
    fn wide_character_low16_policy_covers_all_c_bytes() {
        for unit in 0..=255u64 {
            let mut data = setup(b"%lc\0", &[0xdead_beef_0000 | unit], &[]);
            let mut owner = OwnedMappings::default();
            let mut m = Memory::default();
            m.add(SLOT, &mut data, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            assert_eq!(
                printf_buffer::run(&mut m, SLOT, 100, guard, |b| render(
                    b,
                    FMT,
                    VA,
                    Options::default(),
                    guard
                ))
                .unwrap(),
                1
            );
            let p = m.load(SLOT, 8).unwrap();
            assert_eq!(m.read(p, 2).unwrap(), [unit as u8, 0]);
            m.free(p).unwrap();
            drop(m);
            assert_eq!(owner.bytes_allocated(), 0);
        }
        let mut data = setup(b"%lc\0", &[256], &[]);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap_err();
        assert_eq!(e.kind, Kind::Libc(86));
        assert_eq!(m.load(VA, 4).unwrap(), 8);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn wide_small_precision_zero_work_null_and_repeated_temporary_cleanup() {
        let mut data = setup(
            b"%.0ls|%.127ls|%.3ls|%ls\0",
            &[u64::MAX, DATA, 0, DATA],
            &[],
        );
        data[0x500..0x508].copy_from_slice(&[65, 0, 255, 0, 0, 0, 0, 1]);
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let n = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap();
        let p = m.load(SLOT, 8).unwrap();
        assert_eq!(m.read(p, n as usize + 1).unwrap(), b"|A\xff|(nu|A\xff\0");
        m.free(p).unwrap();
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn wide_large_sizing_can_set_errno_then_succeed_on_prefix() {
        for units in [[0u16, 256], [65u16, 256]] {
            let mut data = setup(b"%.128ls\0", &[DATA], &[]);
            for (i, unit) in units.into_iter().enumerate() {
                data[0x500 + i * 2..0x502 + i * 2].copy_from_slice(&unit.to_le_bytes());
            }
            let mut owner = OwnedMappings::default();
            let mut m = Memory::default();
            m.add(SLOT, &mut data, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            m.store(SLOT + 8, 99, 4).unwrap();
            let options = Options {
                errno_address: Some(SLOT + 8),
                ..Options::default()
            };
            let n = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
                render(b, FMT, VA, options, guard)
            })
            .unwrap();
            assert_eq!(n, i32::from(units[0] != 0));
            assert_eq!(m.load(SLOT + 8, 4).unwrap(), 86);
            let p = m.load(SLOT, 8).unwrap();
            if n != 0 {
                assert_eq!(m.load(p, 1).unwrap(), 65);
            }
            assert_eq!(m.load(p + n as u64, 1).unwrap(), 0);
            m.free(p).unwrap();
            drop(m);
            assert_eq!(owner.bytes_allocated(), 0);
        }
    }
    #[test]
    fn wide_sizing_beyond_nul_and_capacity_check_order_are_observable() {
        let mut data = setup(b"%.128ls\0", &[DATA], &[]);
        data[0x500..0x600].fill(0);
        data[0x600..0x602].copy_from_slice(&256u16.to_le_bytes());
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let options = Options {
            errno_address: Some(SLOT + 8),
            ..Options::default()
        };
        assert_eq!(
            printf_buffer::run(&mut m, SLOT, 100, guard, |b| render(
                b, FMT, VA, options, guard
            ))
            .unwrap(),
            0
        );
        assert_eq!(m.load(SLOT + 8, 4).unwrap(), 86);
        let p = m.load(SLOT, 8).unwrap();
        m.free(p).unwrap();
        // Source accesses unit129 even with precision128. A bounded slice ending
        // immediately after a NUL therefore faults in the large sizing branch.
        drop(m);
        let mut data = setup(b"%.128ls\0", &[0x9000], &[]);
        let mut terminator = [0u8; 2];
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.add(0x9000, &mut terminator, false, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, Options::default(), guard)
        })
        .unwrap_err();
        assert!(matches!(e.kind, Kind::Runtime(_)));
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn wide_copy_encoding_failure_and_success_errno_guard_are_distinct() {
        for format in [b"%ls\0".as_slice(), b"%.2ls\0"] {
            let mut data = setup(format, &[DATA], &[]);
            data[0x500..0x504].copy_from_slice(&[65, 0, 0, 1]);
            let mut owner = OwnedMappings::default();
            let mut m = Memory::default();
            m.add(SLOT, &mut data, true, false).unwrap();
            m.attach_owned(&mut owner).unwrap();
            let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
                render(b, FMT, VA, Options::default(), guard)
            })
            .unwrap_err();
            assert_eq!(e.kind, Kind::Libc(86));
            drop(m);
            assert_eq!(owner.bytes_allocated(), 0);
        }
        let mut data = setup(b"%.128ls\0", &[DATA], &[]);
        data[0x500..0x502].copy_from_slice(&256u16.to_le_bytes());
        let mut owner = OwnedMappings::default();
        let mut m = Memory::default();
        m.add(SLOT, &mut data, true, false).unwrap();
        m.attach_owned(&mut owner).unwrap();
        m.store(SLOT + 8, 99, 4).unwrap();
        let options = Options {
            errno_address: Some(SLOT + 8),
            ..Options::default()
        };
        let e = printf_buffer::run(&mut m, SLOT, 100, guard, |b| {
            render(b, FMT, VA, options, |_, address, _| {
                if address == SLOT + 8 {
                    Err("protected errno".into())
                } else {
                    Ok(())
                }
            })
        })
        .unwrap_err();
        assert!(matches!(e.kind, Kind::Runtime(_)));
        assert_eq!(m.load(SLOT + 8, 4).unwrap(), 99);
        assert_eq!(m.load(SLOT, 8).unwrap(), 0);
        drop(m);
        assert_eq!(owner.bytes_allocated(), 0);
    }
}
