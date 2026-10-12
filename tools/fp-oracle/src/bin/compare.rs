//! Compare the independent saved LLVM corpus to the experimental CPU engine.
//! Unsupported cases are counted separately, never counted as successes.
use std::{collections::BTreeMap, fs::File, io::{BufRead, BufReader}};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Provide corpus TSV")?;
    let mut accepted = 0usize;
    let mut mismatch = 0usize;
    let mut unsupported = BTreeMap::<String, usize>::new();
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.starts_with('#') { continue; }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 5 { return Err("Invalid corpus row".into()); }
        let a = u64::from_str_radix(fields[0], 16)?;
        let b = u64::from_str_radix(fields[1], 16)?;
        let rc = fields[2].parse::<u32>()?;
        let expected = u64::from_str_radix(fields[3], 16)?;
        let status = u32::from_str_radix(fields[4], 16)?;
        let subnormal = |v: u64| v & 0x7ff0_0000_0000_0000 == 0 && v & 0x000f_ffff_ffff_ffff != 0;
        let expected_flags = (status & 1) | ((status & 0x1e) << 1)
            | if subnormal(a) || subnormal(b) { 2 } else { 0 };
        match native_ps5::scalar_fp::multiply_binary64(a, b, 0x1f80 | rc << 13) {
            Ok(result) => {
                accepted += 1;
                if result.bits != expected || result.mxcsr & 0x3f != expected_flags {
                    mismatch += 1;
                    if mismatch <= 12 {
                        eprintln!("Mismatch a={a:016x} b={b:016x} rc={rc} got={:016x}/{:02x} expected={expected:016x}/{expected_flags:02x}", result.bits, result.mxcsr & 0x3f);
                    }
                }
            }
            Err(reason) => { *unsupported.entry(format!("{reason:?}")).or_default() += 1; }
        }
    }
    println!("Finite LLVM corpus: {accepted} accepted; {mismatch} mismatches; unsupported={unsupported:?}");
    if mismatch != 0 { return Err("Arithmetic/status comparison failed".into()); }
    if accepted == 0 { return Err("No cases executed".into()); }
    Ok(())
}
