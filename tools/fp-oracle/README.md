# Independent finite arithmetic comparison

## Fused multiply-add

```sh
cargo run --release --locked --no-default-features --manifest-path tools/fp-oracle/Cargo.toml --bin generate-fma -- /tmp/macps5-fma.tsv
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin compare-fma -- /tmp/macps5-fma.tsv
```

The fused comparison passes 223,328 finite cases in four rounding modes, with zero result-bit or status differences. Expected bits come from LLVM's fused operation. A separate LLVM format with 53-bit precision and a wider exponent range supplies x86 overflow and tininess classification; it does not call the production accumulator. Reference tests include exact cancellation and cancellation of an otherwise overflowing product. This corpus does not cover NaN operand priority or native lowering. `--no-default-features` lets reference generation run without building the implementation under test.

## Multiplication

Generate a deterministic 204,096-case LLVM APFloat corpus and compare it with the experimental multiplication engine:

```sh
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin finite-fp-oracle -- /tmp/macps5-fp.tsv
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin compare -- /tmp/macps5-fp.tsv
```

The comparison passes for **all 204,096 finite cases**, with zero result-bit or normalized-status mismatches. The engine is now wired to VEX VMULSD and passes focused native checks. Nonfinite values and unmasked exception status are implemented; unmasked exceptions stop with a prototype diagnostic rather than delivering a complete guest OS exception.

The initial comparison found 9,780 raw status differences and no result-bit differences. LLVM APFloat reports precision alone for some overflowing results saturated by directed rounding; x86 also reports overflow. The comparator now uses independent 113-bit Quad arithmetic to prove that the exact product reaches the overflow threshold before adjusting that expectation. A boundary test checks saturated results below the threshold, which must not be adjusted. Raw differences and normalization counts remain visible in output. See Intel SDM volume 1, section 4.9.1.4 and table 4-10 (linked in the comparator source).

Adding tiny results exposed 16 further status differences at the minimum-normal boundary. x86 determines tininess after precision rounding with an unbounded exponent, whereas the LLVM result can round to minimum normal without reporting underflow. A second independent Quad calculation and boundary test implement that distinction without calling production arithmetic. The output preserves all 9,796 raw status differences and reports the overflow and tininess adjustments separately.

The generator uses pinned rustc_apfloat (LLVM APFloat port), not the implementation under test, to calculate expected binary64 values and IEEE status. The comparison maps those statuses and separately expects a denormal-operand flag. No NaN-priority, DAZ/FTZ, unmasked exception or hardware x86 equivalence is established by this finite corpus.

## Masked multiplication controls

```sh
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin compare-controls
```

This separate comparison passes 320,000 finite control combinations: all four rounding modes, DAZ on/off, FTZ on/off and preservation of an unrelated sticky flag. Expected values use LLVM; the shared independent Quad reference supplies x86 overflow and tininess decisions. FTZ flushes tiny results to signed zero with underflow and precision flags. These checks cover masked exceptions, not unmasked guest exception delivery or native instruction lowering.

## Truncating integer conversion

```sh
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin compare-truncate
```

This separate model comparison passes 400,000 cases against LLVM's signed32 conversion, normalizing invalid results to the x86 integer-indefinite value. It covers all guest rounding-control settings (the operation must ignore them), DAZ on/off, signed boundaries, special values and an existing unrelated sticky flag. It does not prove native instruction lowering or unmasked exception delivery.
