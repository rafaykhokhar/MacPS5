//! Opt-in register snapshots at explicitly selected guest PCs.
//! Emission only: ordinary generated blocks contain no tracing calls.
use crate::Result;
use std::{collections::BTreeSet, io::Write};
pub fn selected() -> Result<BTreeSet<u64>> {
    let value = match std::env::var("NATIVE_PS5_TRACE_PCS") {
        Ok(v) => v,
        Err(std::env::VarError::NotPresent) => return Ok(BTreeSet::new()),
        Err(e) => return Err(e.to_string()),
    };
    if value.trim().is_empty() {
        return Ok(BTreeSet::new());
    }
    value
        .split(',')
        .map(|v| {
            u64::from_str_radix(v.trim().trim_start_matches("0x"), 16)
                .map_err(|e| format!("Invalid trace PC {v}: {e}"))
        })
        .collect()
}
// SAFETY: unique exported symbol, integer-only ABI, no pointers or unwinding.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn native_ps5_trace_v1(
    pc: u64,
    rax: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rsp: u64,
    flags: u64,
) {
    let _ = writeln!(
        std::io::stderr().lock(),
        "GUEST pc={pc:x} rax={rax:x} rdi={rdi:x} rsi={rsi:x} rdx={rdx:x} rsp={rsp:x} flags={flags:x}"
    );
}
