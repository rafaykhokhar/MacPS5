# Independent finite arithmetic comparison

Generate a deterministic 204,096-case LLVM APFloat corpus and compare it with the experimental multiplication engine:

```sh
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin finite-fp-oracle -- /tmp/macps5-fp.tsv
cargo run --release --locked --manifest-path tools/fp-oracle/Cargo.toml --bin compare -- /tmp/macps5-fp.tsv
```

At the initial repository snapshot the comparison **fails**: 159,752 accepted cases, 9,780 result/status mismatches, and 44,344 explicitly unsupported tiny-inexact cases. Mismatches need classification: LLVM IEEE status semantics do not by themselves establish AMD/x86 exception behavior. This is a diagnostic gate, not a passing validation claim. It is separate from the unit suite. The experimental engine is not wired to VMULSD execution.

The generator uses pinned rustc_apfloat (LLVM APFloat port), not the implementation under test, to calculate expected binary64 values and IEEE status. The comparison maps those statuses and separately expects a denormal-operand flag. No NaN-priority, DAZ/FTZ, unmasked exception or hardware x86 equivalence is established by this finite corpus.
