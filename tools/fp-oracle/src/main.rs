//! Independent finite binary64 multiplication corpus, LLVM APFloat Rust port.
//! IEEE results/status only: does not establish x86 NaN, DAZ/FTZ or traps.
use rustc_apfloat::{Float, Round, ieee::Double};
use std::{fs::File, io::{BufWriter, Write}};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("Provide output TSV path")?;
    let mut out = BufWriter::new(File::create(path)?);
    writeln!(out, "# rustc_apfloat 0.2.3+llvm-462a31f5a5ab finite binary64 multiplication")?;
    writeln!(out, "# a_hex b_hex rc(0=nearest,1=down,2=up,3=zero) result_hex apfloat_status_hex")?;
    let rounds = [Round::NearestTiesToEven, Round::TowardNegative, Round::TowardPositive, Round::TowardZero];
    let edges = [0u64, 1, 2, 0x000f_ffff_ffff_ffff, 0x0010_0000_0000_0000,
        0x0010_0000_0000_0001, 0x3ca0_0000_0000_0000, 0x3fe0_0000_0000_0000,
        0x3fef_ffff_ffff_ffff, 0x3ff0_0000_0000_0000, 0x3ff0_0000_0000_0001,
        0x3ff8_0000_0000_0000, 0x4000_0000_0000_0000, 0x7fdf_ffff_ffff_ffff,
        0x7fe0_0000_0000_0000, 0x7fef_ffff_ffff_ffff];
    let mut count = 0usize;
    let mut emit = |a: u64, b: u64| -> std::io::Result<()> {
        for (rc, round) in rounds.into_iter().enumerate() {
            let result = Double::from_bits(a as u128).mul_r(Double::from_bits(b as u128), round);
            writeln!(out, "{a:016x}\t{b:016x}\t{rc}\t{:016x}\t{:02x}", result.value.to_bits(), result.status.bits())?;
            count += 1;
        }
        Ok(())
    };
    for a in edges { for b in edges { for sa in [0, 1u64 << 63] { for sb in [0, 1u64 << 63] { emit(a | sa, b | sb)?; } } } }
    let mut seed = 0x6b48_c195_f32e_07adu64;
    let mut next = || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
    for i in 0..50_000 {
        let mut a = next(); let mut b = next();
        // Keep finite; systematically include subnormal operands as well.
        if a >> 52 & 0x7ff == 0x7ff { a ^= 1u64 << 52; }
        if b >> 52 & 0x7ff == 0x7ff { b ^= 1u64 << 52; }
        if i % 7 == 0 { a &= !(0x7ffu64 << 52); }
        if i % 11 == 0 { b &= !(0x7ffu64 << 52); }
        emit(a, b)?;
    }
    out.flush()?;
    println!("Generated {count} independent finite multiplication cases across four rounding modes");
    Ok(())
}
