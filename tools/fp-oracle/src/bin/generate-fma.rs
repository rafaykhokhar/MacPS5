use finite_fp_oracle::finite_fma_reference;
use std::{
    fs::File,
    io::{BufWriter, Write},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args().nth(1).ok_or("Provide output TSV")?;
    let mut file = BufWriter::new(File::create(output)?);
    writeln!(
        file,
        "# Independent LLVM fused binary64 reference: a b c rc bits x86flags"
    )?;
    let mut count = 0;
    let mut emit = |a: u64, b: u64, c: u64| -> std::io::Result<()> {
        for rc in 0..4 {
            let (bits, flags) = finite_fma_reference(a, b, c, rc);
            writeln!(
                file,
                "{a:016x}\t{b:016x}\t{c:016x}\t{rc}\t{bits:016x}\t{flags:02x}"
            )?;
            count += 1;
        }
        Ok(())
    };
    let edges = [
        0u64,
        1,
        0x000fffffffffffff,
        0x0010000000000000,
        0x3fefffffffffffff,
        0x3ff0000000000000,
        0x3ff0000000000001,
        0x4000000000000000,
        0x7fefffffffffffff,
    ];
    for a in edges {
        for b in edges {
            for c in edges {
                for sign in 0..8 {
                    emit(
                        a | ((sign & 1) << 63),
                        b | (((sign >> 1) & 1) << 63),
                        c | (((sign >> 2) & 1) << 63),
                    )?;
                }
            }
        }
    }
    let mut seed = 0x135d4f30a79be625u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut x = seed;
        if x >> 52 & 0x7ff == 0x7ff {
            x ^= 1 << 52;
        }
        x
    };
    for i in 0..50_000 {
        let a = next();
        let b = next();
        let mut c = next();
        if i % 3 == 0 {
            c = a ^ (1 << 63);
        }
        emit(a, b, c)?;
    }
    file.flush()?;
    println!("Generated {count} independent finite fused cases");
    Ok(())
}
