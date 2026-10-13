//! FreeBSD11.4 __hdtoa binary64 allocation/effect lifecycle for one printf field.
//! https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gdtoa/_hdtoa.c
//! Uses real owned gdtoa storage; no executor or va_list implementation here.
//! Caller guards State::owned_ranges and performs Retained::release at call-end.
//! A fault leaves any allocated string in Retained for deterministic cleanup.
use crate::{
    guest_gdtoa::{self, Retained, State},
    guest_memory::Memory,
    printf_float,
    printf_float_field::{self, Prepared},
    printf_spec::{Conversion, Length},
};
#[derive(Debug)]
pub enum Error<E> {
    Format,
    Fetch(E),
    Storage(guest_gdtoa::Error),
    Float(printf_float::Error),
    Field(printf_float_field::PrepareError),
    Invariant,
}
#[derive(Debug)]
pub struct Converted {
    pub field: Prepared,
    pub digits_address: u64,
    pub digits_len: usize,
}
/// Validate bounded policy, release the preceding field's retained buffer, then
/// fetch exactly once. Publish normalization flags before allocating, rounding
/// flags after allocating, and finish source writes before field emission.
/// Storage allocation failure is not a manufactured libc ENOMEM return: upstream
/// Balloc does not check its malloc result before touching the returned pointer.
pub fn convert<E>(
    state: &mut State,
    retained: &mut Retained,
    memory: &mut Memory<'_>,
    spec: Conversion,
    mxcsr: &mut u32,
    fetch: impl FnOnce(&mut Memory<'_>) -> Result<u64, E>,
) -> Result<Converted, Error<E>> {
    if !b"aA".contains(&spec.conversion)
        || !matches!(spec.length, Length::Default | Length::Long)
        || spec.flags.grouping
    {
        return Err(Error::Format);
    }
    printf_float::hex_effects(0, spec.precision, *mxcsr).map_err(Error::Float)?;
    retained.release(state, memory).map_err(Error::Storage)?;
    let bits = fetch(memory).map_err(Error::Fetch)?;
    let exponent = (bits >> 52) & 2047;
    let fraction = bits & ((1u64 << 52) - 1);
    let special = exponent == 2047;
    let zero = exponent == 0 && fraction == 0;
    let requested = if special {
        if fraction == 0 { 8 } else { 3 }
    } else if zero {
        1
    } else {
        spec.precision.map_or(15, |p| p + 1)
    };
    // Source scales a subnormal by 2^514 before rv_alloc; it is exact but its
    // denormal operand raises DE. Controls were prevalidated above.
    if exponent == 0 && fraction != 0 {
        *mxcsr |= 2;
    }
    let string = state.rv_alloc(memory, requested).map_err(Error::Storage)?;
    let address = string.address;
    // Retained is known empty because release succeeded, and only this function
    // holds its mutable reference. No fallible guest operation occurs at install.
    if retained.install(string).is_err() {
        return Err(Error::Invariant);
    }
    let field = Prepared::prepare(bits, spec, *mxcsr).map_err(Error::Field)?;
    let payload: Vec<u8>;
    let actual;
    if special {
        let text: &[u8] = if fraction == 0 { b"Infinity" } else { b"NaN" };
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(text.len())
            .map_err(|_| Error::Float(printf_float::Error::Allocation))?;
        bytes.extend_from_slice(text);
        actual = bytes.len();
        payload = bytes;
    } else if zero {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(1)
            .map_err(|_| Error::Float(printf_float::Error::Allocation))?;
        bytes.push(b'0');
        payload = bytes;
        actual = 1;
    } else {
        let rounding = match (*mxcsr >> 13) & 3 {
            0 => printf_float::Rounding::NearestEven,
            1 => printf_float::Rounding::Downward,
            2 => printf_float::Rounding::Upward,
            _ => printf_float::Rounding::TowardZero,
        };
        // Default hdtoa reserves and fills SIGFIGS=15, then trims its returned
        // end pointer. Generate all14 fractional digits to preserve tail bytes.
        let full = printf_float::format(
            bits,
            spec.conversion,
            Some(spec.precision.unwrap_or(14)),
            false,
            rounding,
        )
        .map_err(Error::Float)?;
        let end = full
            .body
            .iter()
            .position(|&b| b == b'p' || b == b'P')
            .ok_or(Error::Invariant)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(requested)
            .map_err(|_| Error::Float(printf_float::Error::Allocation))?;
        bytes.extend(full.body[2..end].iter().copied().filter(|&b| b != b'.'));
        if bytes.len() != requested {
            return Err(Error::Invariant);
        }
        let mut len = bytes.len();
        if spec.precision.is_none() {
            while len > 1 && bytes[len - 1] == b'0' {
                len -= 1;
            }
        }
        actual = len;
        payload = bytes;
    }
    // The source's finite rounding add/subtract precedes writing its digits.
    *mxcsr = field.mxcsr;
    let string = retained.current().ok_or(Error::Invariant)?;
    state
        .write_string(memory, string, &payload)
        .map_err(Error::Storage)?;
    if actual != payload.len() {
        state
            .write_string(memory, string, &payload[..actual])
            .map_err(Error::Storage)?;
    }
    Ok(Converted {
        field,
        digits_address: address,
        digits_len: actual,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        guest_gdtoa::{POOL_BYTES, Store},
        guest_memory::OwnedMappings,
        printf_spec::Flags,
    };
    fn spec(precision: Option<usize>) -> Conversion {
        Conversion {
            flags: Flags::default(),
            width: 0,
            precision,
            length: Length::Default,
            conversion: b'a',
        }
    }
    #[test]
    fn default_fills_fifteen_digits_then_trims_and_retains() {
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let mut retained = Retained::default();
        let mut control = 0x1f80;
        let result = convert(
            state,
            &mut retained,
            &mut memory,
            spec(None),
            &mut control,
            |_| Ok::<_, ()>(1.5f64.to_bits()),
        )
        .unwrap();
        assert_eq!(state.pool_used(), 32);
        assert_eq!(result.digits_address, 0x10004);
        assert_eq!(result.digits_len, 2);
        assert_eq!(memory.read(result.digits_address, 4).unwrap(), b"18\x000");
        assert_eq!(control, 0x1f80);
        assert!(retained.current().is_some());
        retained.release(state, &mut memory).unwrap();
    }
    #[test]
    fn release_precedes_argument_fetch_even_when_fetch_fails() {
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let mut retained = Retained::default();
        let mut control = 0x1f80;
        convert(
            state,
            &mut retained,
            &mut memory,
            spec(None),
            &mut control,
            |_| Ok::<_, ()>(1.0f64.to_bits()),
        )
        .unwrap();
        // Freedtoa restores Bigint maxwds@12 before invoking fetch; digits had
        // overwritten that field with ASCII zeroes on the preceding conversion.
        let result = convert(
            state,
            &mut retained,
            &mut memory,
            spec(None),
            &mut control,
            |m| {
                assert_eq!(m.load(0x1000c, 4).unwrap(), 1);
                Err::<u64, _>(7)
            },
        );
        assert!(matches!(result, Err(Error::Fetch(7))));
        assert!(retained.current().is_none());
    }
    #[test]
    fn allocation_failure_keeps_normalization_but_not_rounding_flags() {
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        for _ in 0..72 {
            state.balloc(&mut memory, 0).unwrap();
        }
        assert_eq!(state.pool_used(), POOL_BYTES);
        let mut retained = Retained::default();
        let mut control = 0x1f80;
        let result = convert(
            state,
            &mut retained,
            &mut memory,
            spec(Some(0)),
            &mut control,
            |_| Ok::<_, ()>(3),
        );
        assert!(matches!(
            result,
            Err(Error::Storage(guest_gdtoa::Error::GuestAllocation(12)))
        ));
        assert_eq!(control, 0x1f82);
        assert!(retained.current().is_none());
    }
    #[test]
    fn large_precision_real_heap_cleanup_and_special_requests() {
        let mut owner = OwnedMappings::default();
        let mut store = Store::new(0x10000).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory.attach_owned(&mut owner).unwrap();
        memory
            .add_runtime_data(0x10000, pool, state.runtime_owner())
            .unwrap();
        let mut retained = Retained::default();
        let mut control = 0x1f80;
        let out = convert(
            state,
            &mut retained,
            &mut memory,
            spec(Some(4096)),
            &mut control,
            |_| Ok::<_, ()>(1.0f64.to_bits()),
        )
        .unwrap();
        assert!(
            memory
                .runtime_heap_allocation_size(out.digits_address - 4, state.runtime_owner())
                .unwrap()
                >= 4128
        );
        assert_eq!(out.digits_len, 4097);
        assert!(memory.heap_allocation_size(out.digits_address - 4).is_err());
        assert!(memory.free(out.digits_address - 4).is_err());
        assert!(memory.store(out.digits_address, 0, 1).is_err());
        assert_eq!(state.pool_used(), 0);
        retained.release(state, &mut memory).unwrap();
        assert!(memory.heap_allocation_size(out.digits_address - 4).is_err());
        for (bits, text) in [
            (0u64, b"0".as_slice()),
            (f64::INFINITY.to_bits(), b"Infinity"),
            (0x7ff0000000000001, b"NaN"),
        ] {
            let out = convert(
                state,
                &mut retained,
                &mut memory,
                spec(Some(4096)),
                &mut control,
                |_| Ok::<_, ()>(bits),
            )
            .unwrap();
            assert_eq!(out.digits_len, text.len());
            assert_eq!(memory.read(out.digits_address, text.len()).unwrap(), text);
        }
        assert_eq!(state.pool_used(), 32);
    }
}
