use std::{
    fs::File,
    io::{BufRead, BufReader},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Provide reference TSV")?;
    let (mut count, mut bits_errors, mut status_errors) = (0usize, 0usize, 0usize);
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        if line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 6 {
            return Err("Invalid fused corpus row".into());
        }
        let hex = |index: usize| u64::from_str_radix(fields[index], 16);
        let (a, b, c) = (hex(0)?, hex(1)?, hex(2)?);
        let rc = fields[3].parse::<u32>()?;
        let (bits, flags) = (hex(4)?, hex(5)? as u32);
        let result = native_ps5::scalar_fp::fma213_binary64(a, b, c, 0x1f80 | (rc << 13))
            .map_err(|e| format!("Rejected finite masked case: {e:?}"))?;
        count += 1;
        bits_errors += usize::from(result.bits != bits);
        status_errors += usize::from(result.mxcsr & 63 != flags);
        if (result.bits != bits || result.mxcsr & 63 != flags) && bits_errors + status_errors <= 12
        {
            eprintln!(
                "FMA a={a:016x} b={b:016x} c={c:016x} rc={rc}: got {:016x}/{:02x} expected {bits:016x}/{flags:02x}",
                result.bits,
                result.mxcsr & 63
            );
        }
    }
    println!(
        "FMA LLVM corpus: {count} cases; {bits_errors} result-bit mismatches; {status_errors} status mismatches"
    );
    if count == 0 || bits_errors != 0 || status_errors != 0 {
        return Err("Fused comparison failed".into());
    }
    Ok(())
}
