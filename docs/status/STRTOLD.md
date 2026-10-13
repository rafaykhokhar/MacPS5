# strtold requirement and initial extended80 model

**The actual pinned wrapper passes focused native verification.** The pinned executable's `strtold_l` at `0x1161f0` is a six-byte tail jump (`ff 25 f2 98 15 00`) through GOT `0x26fae8`. Resolving that import to a binary64 or XMM0 result would violate the return ABI.

The AMD64 ABI classifies long double as X87/X87UP and returns it in ST(0). Its 80-bit value contains an explicit 64-bit significand followed by a 16-bit sign/exponent. A memory long-double object's ABI padding is separate from that ten-byte payload. `legacy_cpu::Cpu` now owns the eight raw registers and tags alongside CW/SW; the context integration is recorded in [CPU status](CPU.md). A host Rust `f64` cannot represent this return value. [AMD64 psABI source](https://gitlab.com/x86-psABIs/x86-64-ABI/-/blob/master/x86-64-ABI/low-level-sys-info.tex).

## Source contract

FreeBSD 11.4's ordinary `strtold` forwards to `strtold_l` using the current locale. For extended precision, `machdep_ldisx.c` calls `strtorx_l` with `FLT_ROUNDS`; `strtorx_l` invokes `strtodg_l` using FPI `{64,-16445,16320,rounding,SI}`. `ULtox` serializes five 16-bit words, including the explicit integer bit, sign, infinity, denormals and target NaN payload rules. This shares a source algorithm with strtof but not its fixed 24-bit precision or exponent bounds. [strtold wrapper](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdlib/strtold.c), [machine glue](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gdtoa/machdep_ldisx.c), [strtorx and serialization](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/contrib/gdtoa/strtorx.c).

The existing source-faithful strtof decimal module is specialized to 24 bits. Its algorithm/FP arithmetic machinery is useful, but changing only output packing or casting an already rounded binary64 result is insufficient. End-pointer, NaN payload, errno and intermediate FP effects require their own 64-bit-precision audit.

## Concrete independent source difference

A Rust x86_64 harness linked the existing unmodified FreeBSD `strtodg` reference objects and ran under Rosetta with FPI64, matching x87/SSE rounding controls and masked exceptions. It reports raw source status, significand, exponent, end offset, errno and MXCSR. This is a compiled upstream-source reference, not measured PS5 firmware or physical x86 hardware.

For exact decimal `18446744073709551615` (2^64−1):

| Rounding | Source significand | Source binary exponent | Source end / errno / MXCSR |
| --- | --- | --- | --- |
| Nearest | `ffffffffffffffff` | 0 | 20 / 0 / `1fa0` |
| Down | `ffffffffffffffff` | 0 | 20 / 0 / `3fa0` |
| Up | `8000000000000000` | 1 | 20 / 0 / `5fa0` |
| Toward zero | `ffffffffffffffff` | 0 | 20 / 0 / `7fa0` |

The integer is exactly representable in extended80, yet the upstream upward path returns 2^64, and all four modes accumulate intermediate precision status. Consequently, even exact rational extended80 conversion is not a source-faithful strtold implementation. Keep this discrepancy as a regression requirement; do not infer errno or FP flags from mathematical inexactness.

Evidence in development workspace: `work/strtold-review/oracle.rs`, `oracle`, `source-policy.log`, `compare.rs` and `compare.log`. The oracle calls `strtodg_l` directly, so final `strtorx` NaN packing and x87 ABI return remain outside that run.

## New pure mathematical building block

[extended80.rs](../../rust/src/float_parse/extended80.rs) implements bounded integer-rational finite conversion directly to ten raw extended80 bytes, with all four rounding directions. It uses the existing bounded lexer only for the consumed input extent, and never converts the binary64 numerical result. A dedicated u128 quotient accumulator handles the full 64-bit significand boundary that the older small-quotient helper cannot safely assume.

The result records bytes, consumed extent, mathematical range and inexactness. Those last two fields explicitly do not describe source errno or FP effects. Canonical infinities/NaNs are represented, while consumed NaN payload syntax is rejected pending source-policy implementation. Existing input/significand limits remain explicit. The module is registered for integrated testing and is not wired to a guest service.

Three standalone tests pass with warnings denied (`f932b7`, exit0), covering exact integers beyond binary64 precision, largest finite/minimum normal/minimum subnormal hex values, ties, carry, directed tiny values, end offsets and bounds. An independent 4,140-case decimal/hex comparison across all rounding modes found 4,139 matching value/end results and the single documented upstream-policy discrepancy above; **that comparison intentionally failed its all-equal assertion** (`95e3d0`, exit101). It is not reported as a passing strtold compatibility test.

## Required route to a real wrapper test

The source-ordered FPI64 decimal model is now implemented separately in
`strtold_decimal.rs`. Its 5,872 independent comparisons with the unmodified
FreeBSD reference pass for raw80 output, errno and MXCSR effects, including both
signs, all four rounding modes, exponent extremes and coefficients up to 4096
digits (`work/strtold-review/decimal-expanded.log`). Three focused tests cover
the initial exact-integer rounding discrepancy and signed-zero refinement.
The source hex/NaN adapter now passes 8,136 comparisons against actual unmodified
`strtorx_l`/`ULtox`, checking ten output bytes, consumed input, errno and MXCSR.
The cases include randomized hex inputs, extended exponent extremes, two-word
NaN payloads, malformed payloads and source rounding quirks. Evidence:
`work/strtold-review/effects-compare.log` and `EFFECTS-PROVENANCE.md`. These are
upstream FreeBSD source comparisons under Rosetta, not console firmware tests.

`Service::Strtold` is integrated with guarded input/endptr/errno and a staged
raw x87 push; XMM0 is preserved. The callable domain is C locale with bounded
input, masked exceptions, gradual underflow and matching x87/SSE rounding.
Internal gdtoa allocation scheduling is not modeled. The wrapper's native
verification is a separate gate below.

CPU context storage is integrated, and guarded `FSTP m80fp` consumption has
focused native evidence; see [CPU status](CPU.md) for operand and fault-policy
limitations. The actual `strtold_l` wrapper now passes 96 recorded raw80/endptr/
errno/MXCSR cases through direct and pinned-wrapper calls in all four rounding
modes. It consumes results with translated `FSTP m80`, including twelve
consecutive call/store pairs within one caller. Checks cover precision beyond
binary64, NaN/tiny/overflow source policies, unchanged XMM0/RAX, output aliases,
read-only/crossing/unmapped/live-frame guards, short errno storage, unterminated
input and unsupported FP controls. Evidence: `work/strtold-native/native.log`
(14736, exit0), [native verifier](../../rust/verification/strtold_native.rs).

AMD's [FLD reference, pp.264–265](https://kib.kiev.ua/x86docs/AMD/AMD64/26569_APM_v5-r3.13.pdf)
exempts double-extended denormals from DE on load. Canonical parser output can
therefore use the raw push without adding a denormal flag. No general x87
arithmetic, physical PS5 ABI differential or retail compatibility claim follows.

1. Introduce owned architectural x87 data/tag/TOP state preserved by every CPU clone, suspension, callback and thread handoff. Audit internal ABI offsets and any retained native fixtures; do not silently append state without updating size/layout contracts.
2. Implement the source-faithful FPI64 decimal and hex/NaN policies with errno and source-ordered FP effects. Preserve unsupported control environments explicitly until independently covered.
3. Return the raw value through the modeled x87 stack, including occupied-stack, tag/TOP and exception behavior. No XMM0 or RAX numeric substitute.
4. Add at least guarded `FSTP m80` consumption and its stack-pop/fault semantics for a translated caller. Then execute the actual pinned `strtold_l` wrapper, snapshot ten payload bytes, inspect endptr/errno/control/status, and test repeated returns, aliases, guards and suspension preservation.

Each part can be developed independently with stable interfaces, but a pure parser pass alone does not complete the import or its CPU prerequisites.
