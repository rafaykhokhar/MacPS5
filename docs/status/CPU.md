# CPU completion criteria

## Scalar single/double moves

VEX VMOVSS and VMOVSD now support all eight decoded memory and register-merge
forms, extending the previous VMOVSD memory-load-only implementation. Loads
copy exactly four/eight guarded bytes and clear all higher destination bits.
Stores copy exactly the scalar and preserve the source. Register forms take
the low scalar from the second source and the remainder of bits127:0 from
the first, then clear bits255:128. Both opcode directions use the decoder's
normalized operands, and input snapshots preserve aliases. All operations
copy raw bits, without floating-point arithmetic.

Reference: AMD APM Volume 4 rev.3.24 pp.224–225 and 230–231
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)),
including ignored W/L encoding fields. Legacy/EVEX, reserved memory-form vvvv
and unsupported FS/GS/address32 forms remain rejected. Architectural
alignment-control behavior and full fault delivery remain outside the guard
model.

The matching build passes 674 library tests, three integration tests, strict
Clippy and release compilation (`work/scalar-move-native/{tests,clippy,release}.log`,
process 44622 exit 0). The old load-only test's store/merge rejection assertions
were updated to the newly supported behavior. The
[native verifier](../../rust/verification/scalar_move_native.rs) passes 80,952
checks across 192 decoded fixtures: all W/L combinations, both register opcode
directions, merge ordering, aliases/high registers, raw FP bits/random data,
exact readonly loads/writable stores with sentinels, high/unaligned/RIP
operands, every short span, readonly stores and flags/FP/fuel/prior faults.
The existing VMOVSD verifier (including its MXCSR and signed32 VCVTSI2SD checks)
also passes (`work/scalar-move-native/{native,regression}.log`, process 16051
exit 0).

Discovery reaches 57,838 instructions and 13,101 blocks with 142 unsupported
sites: six old stops removed and six later stops exposed. These counts do not
establish CPU completion. The entry runner compiles with warnings denied;
startup-only verification passes at the missing `fscanf` boundary, with ten
accepted dependencies, 20,288 owned dependency bytes and pending cleanup.
Game main has not executed (`work/scalar-move-native/entry.log`, process 34889
exit 0). No full GPU suite was run for this CPU change. Production readiness
and retail gameplay remain unverified.

## Packed double moves

VEX VMOVAPD and VMOVUPD now support their eight XMM/YMM load/store encodings,
including register-to-register copies in either opcode direction. They use the
existing guarded vector copy path. VMOVAPD enforces 16/32-byte alignment;
VMOVUPD accepts unaligned operands. Loads guard the full source before writing
the destination and clear the upper half for XMM. Stores guard the full output
before writing and preserve source registers. All bits are copied without FP
arithmetic. Only exact VEX forms are accepted; reserved vvvv, legacy/EVEX and
unsupported FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.182–183 and 232–233
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Alignment failures use runtime guard stops; architectural #GP classification,
delivery and recovery remain unimplemented.

The matching build passes 673 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-double-move-native/{tests,clippy,release}.log`,
process 29681 exit 0). The [native verifier](../../rust/verification/packed_double_move_native.rs)
passes 38,016 checks across 72 decoded fixtures: both register opcode
directions, aliases/high registers/WIG, raw FP bits/random lanes, upper
clearing, every aligned-move misalignment, unaligned copies, readonly loads,
writable stores with sentinels, high/RIP operands, every short span, readonly
store faults and flags/FP/fuel/prior faults. The existing MOVDQU regression
also passes (`work/packed-double-move-native/{native,regression}.log`, process
83578 exit 0).

Discovery reaches 57,827 instructions and 13,097 blocks with 142 unsupported
sites: four former stops removed and one later stop exposed. These counts do
not establish CPU completion. The entry runner compiles with warnings denied;
startup-only verification passes at the missing `fscanf` boundary, with ten
accepted dependencies, 20,288 owned dependency bytes and pending cleanup.
Game main has not executed (`work/packed-double-move-native/entry.log`, process
24128 exit 0). No full GPU suite was run for this CPU change. Production
readiness and retail gameplay remain unverified.

## Scalar floating-point broadcasts

VEX VBROADCASTSS (XMM/YMM) and VBROADCASTSD (YMM) now support register and
memory sources through the existing scalar broadcast path. The six exact
decoded forms copy the low 32/64 source bits, preserving NaN payloads,
subnormals, signed zero and infinities without floating-point arithmetic.
Memory sources read exactly four/eight guarded bytes. Register aliases are
safe, and XMM results clear the upper half. W1, reserved vvvv, SD128,
legacy/EVEX and unsupported FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.599–602
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural fault delivery remains outside the current guard model.

The matching build passes 672 library tests, three integration tests, strict
Clippy and release compilation (`work/scalar-broadcast-native/{tests,clippy,release}.log`,
process 40530 exit 0). The [native verifier](../../rust/verification/scalar_broadcast_native.rs)
passes 34,632 checks across 27 decoded fixtures, including raw FP special
values, walking bits/random patterns, unused source poison, aliases/high
registers, exact readonly high/unaligned/RIP scalar loads, every short span,
flags/FP/fuel/prior faults. The existing packed integer broadcast verifier
passes 25,074 regression checks (`work/scalar-broadcast-native/{native,regression}.log`,
process 8224 exit 0).

Discovery reaches 57,795 instructions and 13,096 blocks with 145 unsupported
sites: five former stops removed and two later stops exposed. These counts
do not establish CPU completion. The entry runner compiles with warnings
denied; startup-only verification passes at the existing missing `fscanf`
boundary, with ten accepted dependencies, 20,288 owned dependency bytes and
pending cleanup. Game main has not executed (`work/scalar-broadcast-native/entry.log`,
process 21175 exit 0). No full GPU suite was run for this CPU change.
Production readiness and retail gameplay remain unverified.

## Saturating integer packing

VEX VPACKSSWB/SSDW/USWB/USDW now support eight XMM/YMM forms. Inputs are
signed words/dwords even for unsigned-output packing. Each 128-bit half packs
the first source's clipped values followed by the second source's clipped
values. ARM64 register lowering uses scalar comparisons and selection instead
of saturating NEON operations that would set host FPSR.QC. Destination writes
follow all source reads; memory helpers guard the complete 16/32-byte operand
before committing. XMM writes clear the upper half; guest flags/FP state are
unchanged.

References: AMD APM Volume 4 rev.3.24 pp.259–266
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)) and
Intel SDM Volume 2B, PACK instruction operation definitions
([manual](https://cdrdv2-public.intel.com/825757/253667-sdm-vol-2b.pdf)).
Intel's VEX256 encoding and per-half operation definition resolve the AMD
VPACKUSDW YMM table's printed L=0 typo. Architectural alignment-control
behavior and full exception delivery remain outside the current guard model.

The matching build passes 671 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-pack-native/{tests,clippy,release}.log`,
process 2312 exit 0). The [native verifier](../../rust/verification/packed_pack_native.rs)
passes 287,168 checks across 96 decoded fixtures using an independent signed
input/clamp oracle: every i16 input, i32 clipping boundaries/extrema, per-half
ordering, random lanes, aliases/high registers/WIG, readonly high/unaligned/RIP
operands, every short span, guest flags/FP/fuel/prior faults and direct host
FPSR preservation (`work/packed-pack-native/native.log`, process 38241 exit 0).

Discovery reaches 57,756 instructions and 13,095 blocks with 148 unsupported
sites: one former stop removed and no new stops. These counts do not measure
CPU completion. The entry runner compiles with warnings denied; startup-only
verification passes at the unchanged missing `fscanf` boundary, with ten
accepted dependencies, 20,288 owned dependency bytes and cleanup pending.
Game main has not executed (`work/packed-pack-native/entry.log`, process 74877
exit 0). No full GPU suite was run for this CPU change. Production readiness
and retail gameplay remain unverified.

## Per-lane variable shifts

VEX VPSLLVD/Q, VPSRLVD/Q and VPSRAVD now support ten XMM/YMM forms. The
translator checks each full unsigned 32/64-bit count before selecting a result;
it does not inherit ARM's masked scalar shift count. Oversized logical shifts
produce zero, and oversized arithmetic dword shifts produce the sign fill.
Register results are assembled before either destination half is written,
preserving source/count/destination aliases. Memory helpers guard the complete
16/32-byte count operand before committing. XMM results clear the upper half;
flags and FP state are preserved. Only the exact VEX forms are accepted.

Reference: AMD APM Volume 4 rev.3.24 pp.845–854
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control behavior and full exception delivery remain
outside the current guard model.

The matching build passes 667 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-variable-shift-native/{tests,clippy,release}.log`,
process 37890 exit 0). The [native verifier](../../rust/verification/packed_variable_shift_native.rs)
passes 124,080 checks across 120 decoded fixtures with an independent widened
multiply/divide oracle: counts 0–255, 256, every higher bit, maximum counts,
boundaries, mixed/random lanes, sign extrema, aliases/high registers, readonly
high/unaligned/RIP operands, every short span, flags/FP/fuel/prior faults
(`work/packed-variable-shift-native/native.log`, process 42646 exit 0).

Discovery reaches 57,737 instructions and 13,095 blocks with 149 unsupported
sites: one former stop removed and no new stops. These counts do not measure
CPU completion. The entry runner compiles with warnings denied and startup-only
verification passes at the existing missing `fscanf` boundary: ten accepted
dependencies, 20,288 owned dependency bytes and cleanup pending. Game main has
not executed (`work/packed-variable-shift-native/entry.log`, process 37955 exit
0). No full GPU suite was run for this CPU change. Production readiness and
retail gameplay remain unverified.

## Packed widening multiply and word multiply-add

VEX VPMADDWD, VPMULUDQ and VPMULDQ now support their six XMM/YMM forms.
ARM64 lowering uses signed widening word multiplication plus pairwise wrapping
addition for VPMADDWD. Its all-0x8000 input case produces 0x80000000 rather
than saturating. Dword multiplication selects only the even dword lanes and
writes full signed/unsigned 64-bit products. Both destination halves are
committed after the old sources are read, preserving aliases. Memory helpers
guard the complete 16/32-byte operand before committing. XMM writes clear the
upper half; flags and FP state remain unchanged.

Reference: AMD APM Volume 4 rev.3.24 pp.366–367, 418–419 and 430–431
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control behavior and full exception delivery remain
outside the current guard model.

The matching build passes 663 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-multiply-native/{tests,clippy,release}.log`,
process 57802 exit 0). The [native verifier](../../rust/verification/packed_multiply_native.rs)
passes 19,152 checks across 72 decoded fixtures using an independent i128
oracle: overflow, signed/unsigned extrema, cancellation, random lanes, ignored
odd-dword poison, aliases/high registers/WIG, readonly high/unaligned/RIP
operands, every short span, flags/FP/fuel/prior faults
(`work/packed-multiply-native/native.log`, process 89903 exit 0).

Discovery reaches 57,726 instructions and 13,093 blocks with 150 unsupported
sites: three former stops removed and one later stop exposed. These are not
CPU coverage percentages. The entry runner compiles with warnings denied and
startup-only verification passes at the unchanged missing `fscanf` boundary:
ten accepted dependencies, 20,288 owned dependency bytes and cleanup pending.
Game main has not executed (`work/packed-multiply-native/entry.log`, process
33726 exit 0). No full GPU suite was run for this change. Production readiness
and retail gameplay remain unverified.

## Packed integer minimum and maximum

VEX VPMIN/MAX signed and unsigned byte, word and dword operations now support
all 24 XMM/YMM forms. Register lowering uses ARM64 signed/unsigned vector
min/max and captures both inputs before destination stores. Memory helpers
guard the complete 16/32-byte operand before committing; signed comparisons
use sign-bit bias. XMM writes clear the upper half, and flags/FP state remain
unchanged. Legacy/EVEX and unsupported FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.368–391
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control behavior and full exception delivery remain
outside the current guard model.

The matching build passes 659 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-minmax-native/{tests,clippy,release}.log`,
process 72903 exit 0). The [native verifier](../../rust/verification/packed_minmax_native.rs)
passes 599,872 checks across 288 decoded fixtures, with an independent widened
signed-number oracle: all byte-value pairs, wider sign boundaries/extrema,
equal/random lanes, aliases/high registers/WIG, readonly high/unaligned/RIP
operands, every short span, flags/FP/fuel/prior faults
(`work/packed-minmax-native/native.log`, process 87559 exit 0).

Discovery reaches 57,665 instructions and 13,088 blocks with 152 unsupported
sites: four former stops removed and no new stops. These counts do not prove
CPU coverage. The entry runner compiles with warnings denied and startup-only
verification passes at the existing missing `fscanf` boundary: ten accepted
dependencies, 20,288 owned dependency bytes and cleanup pending. Game main has
not executed (`work/packed-minmax-native/entry.log`, process 21440 exit 0).
No full GPU suite was run for this CPU change. Production readiness and retail
gameplay remain unverified.

## Immediate integer vector blending

VEX VPBLENDW and VPBLENDD now select word/dword lanes from two sources in
128-bit and 256-bit forms. Word selection repeats the eight mask bits in each
128-bit half; dword selection uses one bit per lane across the full vector.
Register lowering snapshots both inputs before writing the result. Memory
helpers guard the full 16/32-byte operand even for a zero selection mask, then
commit atomically. XMM results clear the upper half; flags and FP state are
preserved. Exact VEX forms are required, with word WIG and dword W0 handling.

Reference: AMD APM Volume 4 rev.3.24 pp.295–296 and 698–699
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control behavior and full exception delivery remain
outside the current guard model.

The matching build passes 655 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-blend-native/{tests,clippy,release}.log`,
process 61173 exit 0). The [native verifier](../../rust/verification/packed_blend_native.rs)
passes 111,360 checks across 2,208 decoded fixtures, including every immediate,
independent lane-selection expectations, aliases/high registers, word WIG,
readonly high/unaligned/RIP operands, every short span, and flags/FP/fuel/prior
fault preservation (`work/packed-blend-native/native.log`, process 83976 exit 0).

Discovery reaches 57,282 instructions and 12,988 blocks with 156 unsupported
sites: eight former stops removed and two later stops exposed. These counts
are not CPU completion percentages. The entry runner compiles with warnings
denied; startup-only verification still reaches missing `fscanf`, with ten
accepted dependencies, 20,288 owned dependency bytes and pending cleanup.
Game main has not executed (`work/packed-blend-native/entry.log`, process 92491
exit 0). No full GPU suite was run for this CPU change. Production readiness
and retail gameplay remain unverified.

## Full-width integer permutations

VEX VPERMD and VPERMQ now select values across the complete 256-bit source.
VPERMD masks each dword index to three bits before reading the corresponding
source dword. VPERMQ uses the four immediate two-bit fields to select qwords.
Register lowering builds both result halves before any destination store, so
destination/index/source aliases are safe. Memory helpers guard all 32 bytes
before committing. Flags/FP state remain unchanged. Only the exact VEX256
forms are accepted; invalid W/L, reserved VPERMQ vvvv, legacy/EVEX and unsupported
FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.730–731 and 751–752
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control behavior and full exception delivery remain
outside the current guard model.

The matching build passes 651 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-permute-native/{tests,clippy,release}.log`,
process 45033 exit 0). The [native verifier](../../rust/verification/packed_permute_native.rs)
passes 66,192 independent full-width source-selection checks across 3,084 decoded
fixtures: all qword immediates, all dword selectors in each output, each ignored
index bit, mixed/reverse/rotate/broadcast/random indices, unused source poison,
aliases/high registers, full readonly high/unaligned/RIP operands, every short
span, flags/FP/fuel/prior faults (`work/packed-permute-native/native.log`, process
41823 exit 0).

Discovery reaches 56,984 instructions and 12,959 blocks with 162 unsupported
sites: six former stops removed and five later stops exposed. These counts do
not establish CPU coverage. The entry runner compiles with warnings denied;
startup-only verification retains the missing `fscanf` boundary, ten accepted
dependencies, 20,288 owned dependency bytes and pending cleanup. Game main has
not executed (`work/packed-permute-native/entry.log`, process 43921 exit 0).
No full GPU suite was run for this CPU change. Production readiness and retail
gameplay remain unverified.

## Scalar fused multiply-add 231 ordering

VEX VFMADD231SD now lowers through the existing exact fused binary64 helper,
with its inputs reordered to compute explicit-register source times final source
plus old destination. NaN payload priority follows that same arithmetic order;
any signaling NaN still raises invalid. The destination's old bits127:64 are
preserved and bits255:128 cleared. Aliased inputs are captured before commit.
Memory forms guard exactly eight bytes before arithmetic. The existing 213
form remains supported; 132 and other unimplemented forms remain rejected.

Reference: [Intel SDM Volume 1](https://cdrdv2-public.intel.com/843827/253665-sdm-vol-1-dec-24.pdf)
§14.5.1–2, Table14-17, printed14-22–24, and TableD-18, printedD-15.
The latter gates new precision status when overflow/underflow is unmasked;
preexisting sticky flags remain set. Intel's explicit upper-lane preservation
rule is used rather than the conflicting AMD rev.3.24 p.620 text.

The matching build passes 647 library tests, three integration tests, strict
Clippy and release compilation (`work/fma231-native/{tests,clippy,release}.log`,
process 62384 exit 0). The [native verifier](../../rust/verification/fma231_native.rs)
passes 14,202 checks across 24 decoded fixtures using exact small-integer
arithmetic and independently specified fused cancellation, rounding, NaN payload,
DAZ/FTZ and exception vectors. It covers aliases/high registers/LIG, preserved
high64/cleared upper128, readonly high/unaligned/RIP inputs, every short span,
reserved controls, memory-fault precedence and preserved flags/GPR/x87/fuel.
The existing 213 native regression also passes (`work/fma231-native/native.log`,
process 30507 exit 0). An initial verifier expectation incorrectly included new
precision status on unmasked overflow; it was corrected against TableD-18,
without changing the arithmetic helper. These are selected floating-point
checks, not exhaustive arithmetic or architectural exception-delivery proof.

Discovery reaches 56,950 instructions and 12,954 blocks with 163 unsupported
sites: the former 231 stop is removed and a later stop exposed (process 8533
exit 0). These counts do not establish CPU coverage. The entry runner compiles
with warnings denied; its startup-only run still stops at missing `fscanf`,
with ten accepted dependencies, 20,288 owned dependency bytes and pending cleanup.
Game main has not executed (`work/fma231-native/entry.log`, process 89926 exit 0).
No full GPU suite was run for this CPU change. Production readiness and retail
gameplay remain unverified.

## Packed low/high interleaving

VEX VPUNPCKL/H BW, WD, DQ and QDQ now support XMM/YMM register and memory
operands (16 exact forms). ARM64 ZIP1/ZIP2 interleaves the selected low/high
halves at byte/word/dword/qword width independently within each 128-bit half.
Both sources are consumed before destination stores; XMM writes clear upper
YMM bits. Flags/FP state remain unchanged. WIG is accepted; legacy/EVEX and
unsupported FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.499–522
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The memory helper retains our full 16/32-byte operand guard before committing
the result. This is a guard-policy check, not proof of PS5 partial-fetch fault
behavior: [Intel SDM Volume 2B](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf)
permits a low-unpack implementation to fetch only the relevant half of a
128-bit memory operand. Architectural alignment controls and complete exception
delivery remain unimplemented.

The matching build passes 646 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-unpack-native/{tests,clippy,release}.log`,
process 2018 exit 0). The [native verifier](../../rust/verification/packed_unpack_native.rs)
passes 23,232 interleave-reference checks across 192 decoded fixtures: all lane
widths/low/high forms, distinct sources/halves, ignored-half poison, edge/random
bytes, aliases/high registers/WIG, full readonly high/unaligned/RIP operands,
every short span under the stated guard policy, flags/FP/fuel/prior faults
(`work/packed-unpack-native/native.log`, process 23129 exit 0).

Discovery reaches 56,949 instructions and 12,954 blocks with 163 unsupported
sites: five old stops are removed and one later stop is exposed. These counts
do not establish CPU coverage. The entry runner compiles with warnings denied
and startup-only verification retains the missing `fscanf` boundary, ten
accepted dependencies, 20,288 owned dependency bytes and pending cleanup.
Game main has not executed (`work/packed-unpack-native/entry.log`, process 8590
exit 0). Full GPU checks were not run for this CPU change. Production readiness
and retail gameplay remain unverified.

## Signed64 truncating double conversion

VEX VCVTTSD2SI now supports 64-bit GPR destinations as well as the existing
32-bit form. The new integer-only helper handles binary64 significands and
exponents without depending on host floating-point state. Negative 2^63 is
valid; positive 2^63, larger magnitudes, infinities and NaNs raise invalid.
Masked invalid returns the integer-indefinite bit pattern. Fractional truncation
raises precision, RC is ignored, DAZ zeroes subnormal inputs and FTZ has no effect.
Sticky MXCSR status is retained. Unmasked exceptions publish status and stop
before the GPR write; full guest exception delivery remains unimplemented.
The existing i32 helper is unchanged. Memory reads guard exactly eight bytes;
unsupported addressing is rejected, including 32-bit absolute addressing.

Reference: AMD APM Volume 4 rev.3.24 pp.117–119
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf));
the encoding has ignored L, W selecting integer width and reserved vvvv.

The matching build passes 642 library tests, three integration tests, strict
Clippy and release compilation (`work/truncate64-native/{tests,clippy,release}.log`,
process 37083 exit 0). The [native verifier](../../rust/verification/truncate64_native.rs)
passes 2,160,068 independent binary-power reference checks over 12 decoded
register/memory fixtures. It covers all exponents, random significands, both
signs, boundary neighbors, NaNs/infinities/zeros/subnormals, all RC/DAZ/FTZ/IM/PM
combinations with sticky status, L0/L1, high registers, exact-end readonly
high/unaligned/RIP reads, every short span, fault precedence, reserved MXCSR,
and preserved registers/vector/x87/flags/fuel. The existing signed32 native
regression also passes (`work/truncate64-native/native.log`, process 54525 exit 0).

Discovery reaches 56,589 instructions and 12,860 blocks with 167 unsupported
sites; all eight former signed64 conversion stops are removed with no new stops.
This is not a CPU coverage percentage. The integrated entry runner compiles
with warnings denied and startup-only verification retains the expected missing
`fscanf` boundary: ten dependencies accepted, 20,288 dependency bytes owned,
cleanup pending and game main not executed (`work/truncate64-native/entry.log`,
process 19235 exit 0). Full GPU checks were not run for this CPU change.
Production readiness and retail gameplay remain unverified.

## 128-bit vector extraction

VEXTRACTI128 and VEXTRACTF128 now extract either half of a YMM register into
an XMM register or exactly 16 bytes of guest memory. Only immediate bit 0
selects the half. Register writes snapshot the source before writing and clear
the destination's upper YMM half, including self-aliases. Stores preserve all
vectors and validate the complete writable span before copying. Flags/FP state
are unchanged. W1, L0, reserved vvvv, legacy/EVEX and unsupported FS/GS/address32
forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.610–613
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The current guard fault model does not establish architectural exception delivery
or alignment-control emulation.

The matching build passes 638 library tests, three integration tests, strict
Clippy and release compilation (`work/vector-extract128-native/{tests,clippy,release}.log`,
process 97151 exit 0). The [native verifier](../../rust/verification/vector_extract128_native.rs)
passes 78,336 byte-copy reference checks over 4,096 decoded fixtures: all 256
immediates, high registers, high-half self-aliasing, upper clearing, exact-end and
sentinel-protected high/unaligned/RIP stores, every short span, readonly failures,
flags/FP/fuel/prior faults (`work/vector-extract128-native/native.log`, process
11501 exit 0). Helper tests additionally check invalid parameters and write barriers.

Discovery reaches 56,566 instructions and 12,859 blocks with 175 unsupported
sites; all four previous extraction stops are removed, with no new stops exposed.
These counts do not establish CPU coverage. The entry runner compiles with
warnings denied and its startup-only run still stops at missing `fscanf`, after
accepting ten dependencies. The 20,288 dependency bytes remain owned, cleanup
is pending and game main has not executed (`work/vector-extract128-native/entry.log`,
process 26066 exit 0). No full GPU run was performed for this CPU change.
Production readiness and retail gameplay remain unverified.

## Packed byte and doubleword shuffles

VEX VPSHUFB and VPSHUFD now cover XMM and YMM register/memory forms.
Byte shuffles use ARM64 TBL independently per 128-bit half, masking controls
with 0x8f so bit 7 zeroes the output and bits 6:4 are ignored. Doubleword
shuffles use immediate-selected ARM64 lane inserts. Sources are consumed before
destination writes, including aliases; XMM writes clear the upper YMM half.
Memory helpers guard the complete 16/32-byte operand before committing a result.
Flags/FP state are preserved. Legacy/EVEX, reserved VPSHUFD vvvv and unsupported
FS/GS/address32 forms remain rejected; WIG is accepted.

Reference: AMD APM Volume 4 rev.3.24 pp.436–440
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control exceptions and full exception delivery remain
outside the guarded-memory model.

The matching build passes 634 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-shuffle-native/{tests,clippy,release}.log`,
process 53180 exit 0). The [native verifier](../../rust/verification/packed_shuffle_native.rs)
passes 95,616 independent source-selection checks across 2,136 decoded fixtures:
all doubleword immediates for register and memory forms; all byte controls;
mixed random controls, single-position zeroing, reverse/permutation patterns,
and separate upper/lower lane selection; aliases/high registers/WIG; exact-end
readonly high/unaligned/RIP reads; every short span; flags/FP/fuel/prior faults
(`work/packed-shuffle-native/native.log`, process 28308 exit 0).

Discovery reaches 56,528 instructions and 12,853 blocks with 179 unsupported
sites: 11 former stops removed, six later stops exposed (process 37241 exit 0).
These are discovery counts, not CPU coverage percentages. The integrated entry
runner compiles with warnings denied. Its startup-only run still stops at
missing `fscanf`, with ten accepted dependencies and 20,288 owned dependency
bytes; cleanup remains pending and game main has not executed
(`work/packed-shuffle-native/entry.log`, process 65294 exit 0). Full GPU checks
were not run for this CPU change. Production readiness and retail gameplay
remain unverified.

## Packed logical operations

VEX VPAND, VPANDN, VPOR and VPXOR now cover both XMM and YMM operands
(eight exact forms). Register operations lower to ARM64 AND/BIC/ORR/EOR;
BIC inputs are reversed to implement x86's complement of the first source.
Both source halves are read before destination stores. Memory operands use a
guarded 16/32-byte read and commit the result only after the complete read
succeeds. XMM results clear the upper YMM half; flags and FP state are preserved.
WIG is accepted; legacy/EVEX and unsupported FS/GS/address32 forms remain rejected.

Reference: AMD APM Volume 4 rev.3.24 pp.285–288, 432–433 and 523–524
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Architectural alignment-control exceptions and full exception delivery remain
outside the current guarded-memory model.

The matching build passes 630 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-logical-native/{tests,clippy,release}.log`,
process 23347 exit 0). The [native verifier](../../rust/verification/packed_logical_native.rs)
passes 534,880 independent bit truth-table checks across 96 decoded fixtures,
including every byte pair, asymmetric AND-NOT inputs, distinct upper lanes,
source/destination aliases, high registers, WIG, exact-end readonly high/unaligned/
RIP memory inputs, every short span, flags/FP, fuel and prior faults. The existing
VPXOR verifier also passes 7,257 regression checks after removing its obsolete
256-bit rejection (`work/packed-logical-native/native.log`, process 48325 exit 0).

Discovery reaches 56,452 instructions and 12,849 blocks, with 184 unsupported
sites: 11 former stops are removed and eight later stops become visible.
These are discovery counts, not CPU coverage percentages. The integrated entry
runner compiles with warnings denied; its startup-only run still stops at missing
`fscanf`, with ten accepted dependencies and 20,288 owned dependency bytes.
Cleanup remains pending and game main has not executed
(`work/packed-logical-native/entry.log`, process 54077 exit 0). The full GPU suite
was not run for this CPU change. Retail gameplay and production readiness remain
unverified.

## Packed sign and zero extension

VEX VPMOVSX/ZX now support BW/BD/BQ/WD/WQ/DQ widening for XMM and YMM
destinations (24 forms). Register sources are always XMM and lower through
staged ARM64 SSHLL/USHLL, splitting the final stage for 256-bit outputs.
Memory reads use exactly `destination_bytes / output_lane * input_lane`,
between 2 and 16 bytes. Failed reads preserve the whole destination. XMM
outputs clear the upper YMM half; flags/FP state are unchanged. Reserved vvvv,
legacy/EVEX and FS/GS/address32 remain rejected; WIG is supported.

Reference: AMD APM Volume 4 rev.3.24 pp.394–417
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The matching build passes 626 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-extend-native/{tests,clippy,release}.log`,
process 10442 exit 0). The [native verifier](../../rust/verification/packed_extend_native.rs)
passes 50,340 byte/sign-fill reference checks over 192 decoded fixtures:
all byte values, word/dword edge/random inputs, distinct lanes, ignored source
bytes poisoned with zero/ones, high/self aliases, WIG, exact-end readonly
high/unaligned/RIP input spans, every short span, flags/FP and execution guards.
Helper unit tests also cover invalid widths/modes, numeric signed boundaries
and atomic FFI failures (`work/packed-extend-native/native.log`, process
10954 exit 0).

Pinned discovery advances to 56,149 instructions in 12,782 blocks: twelve old
boundaries disappear and seven new ones become reachable, leaving 187
unsupported stops. The combined verifier compiles and startup-only execution
retains the expected missing `fscanf` exit: ten named dependencies accepted,
20,288 dependency bytes owned, cleanup pending and game main not executed
(`work/packed-extend-native/entry.log`, process 10954 exit 0).
No CPU completion percentage or retail-game compatibility is established.

## Packed vector bit tests

VEX VPTEST supports XMM and YMM register or memory sources. ZF reports whether
the intersection is empty; CF reports whether any bit in the second source is
absent from the first. OF/SF/AF/PF clear and other flags survive. Neither vector
changes. The register emitter reduces over the entire selected width; the
memory helper validates the whole source before publishing flags. Reserved
vvvv, legacy/EVEX and FS/GS/address32 remain rejected; WIG forms are accepted.

Reference: AMD APM Volume 4 rev.3.24 pp.497–498
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The matching build passes 622 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-test-native/{tests,clippy,release}.log`,
process 38550 exit 0). An initial Clippy run found an identity operation in a
test expectation; that expression was simplified before the passing build.
The [native verifier](../../rust/verification/packed_test_native.rs) passes
19,800 comparisons across 18 decoded fixtures: independent per-bit flag
expectations, all four CF/ZF outcomes, each bit including the upper256 half,
asymmetric/random inputs, aliases/high registers/WIG, multiple old-flag patterns,
exact-end readonly high/unaligned/RIP memory, every short span and state/fuel
guards (`work/packed-test-native/native.log`, process 29057 exit 0).

Pinned discovery clears all six former VPTEST boundaries and adds no new
boundaries, reaching 56,022 instructions in 12,766 blocks with 192 unsupported
stops. The combined verifier compiles and startup-only execution retains its
expected missing `fscanf` exit: ten named dependencies accepted, 20,288
dependency bytes owned, cleanup pending and game main not executed
(`work/packed-test-native/entry.log`, process 29057 exit 0).
These counts are not a CPU coverage percentage or proof of gameplay.

## Packed wrapping arithmetic

VEX VPADDB/W/D/Q and VPSUBB/W/D/Q now support XMM/YMM register and memory
operands. Each lane wraps independently at its own width; subtraction computes
first source minus second source, without cross-lane carry or borrow. Register
forms use ARM64 vector ADD/SUB. Memory forms validate parameters and the entire
16/32-byte source before publishing a complete result. XMM results clear upper
YMM bits. Flags/FP state remain unchanged. Saturating, legacy and EVEX forms
are not included; FS/GS/address32 remain rejected.

Reference: AMD APM Volume 4 rev.3.24 ADD pp.267/269/271/281 and SUB
pp.481/483/485/495 ([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Width-specific wrapping is also checked against
[Intel's instruction reference](https://cdrdv2-public.intel.com/835781/325462-sdm-vol-1-2abcd-3abcd-4.pdf),
avoiding the old AMD SUB prose typo that names eight bits for wider forms.

The build passes 618 library tests, three integration tests, strict Clippy and
release compilation (`work/packed-addsub-native/{tests,clippy,release}.log`,
process 8737 exit 0). The [native verifier](../../rust/verification/packed_addsub_native.rs)
passes 284,864 u128-modulo reference comparisons over 192 decoded fixtures:
exhaustive byte pairs, edge/random wider lanes, carry/borrow isolation,
subtraction order, aliases/high registers/WIG, exact-end readonly high/unaligned/
RIP memory, every short span, state preservation and execution guards
(`work/packed-addsub-native/native.log`). Unit tests also cover invalid helper
parameters, FFI errors and prior-fault preservation.

Pinned discovery advances by 259 instructions to 55,856 instructions and
12,707 blocks. Fifteen previous stops disappear and seven new stops become
reachable, leaving 198 unsupported boundaries. These counts describe discovered
code, not CPU completion or game compatibility.

The combined entry verifier compiles and its startup-only run passes the
unchanged missing `fscanf` exit boundary: ten named dependencies accepted,
20,288 dependency bytes owned, cleanup pending and game main not executed
(`work/packed-addsub-native/entry.log`, process 41383 exit 0, including the
native suite above). Full GPU/integrated verification was not run here.

## Packed integer equality

VEX VPCMPEQB/W/D/Q now support XMM and YMM register or memory operands.
Each equal lane becomes all ones, each unequal lane zero. Register forms
lower directly to ARM64 CMEQ; memory forms validate the complete 16/32-byte
operand before committing any vector lane. Both preserve flags and FP state,
capture source aliases safely, and clear upper YMM bits for XMM results.
The helper validates register indices, vector sizes and lane sizes before
mutation. Architectural alignment-check exceptions remain outside the current
guarded-memory model; legacy/EVEX and FS/GS/address32 remain unsupported.

Reference: AMD APM Volume 4 rev.3.24 pp.300–307
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The matching build passes 614 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-equal-native/{tests,clippy,release}.log`,
process 14239 exit 0). The [native verifier](../../rust/verification/packed_equal_native.rs)
passes 20,088 comparisons across 96 decoded fixtures, covering every individual
bit mismatch, randomized/mixed equal lanes, source/destination aliases, high
registers, WIG, exact-end readonly high/unaligned/RIP loads, all short spans,
fault atomicity, flags/FP and execution guards. Unit tests additionally exercise
invalid helper parameters and FFI failure preservation
(`work/packed-equal-native/native.log`, process 41037 exit 0).

Pinned discovery reaches 55,597 instructions in 12,667 blocks. Thirteen former
comparison boundaries disappear and twelve new stops become reachable, leaving
206 unsupported boundaries. The combined verifier compiles and its startup-only
run retains the expected missing `fscanf` exit: ten named dependencies accepted,
20,288 dependency bytes owned, cleanup pending and game main not executed
(`work/packed-equal-native/entry.log`, process 41037 exit 0).
These component checks do not establish full CPU or retail-game compatibility.

## Packed immediate shifts

VEX immediate VPSLLW/D/Q, VPSRLW/D/Q and VPSRAW/D now support both XMM and
YMM destinations (16 decoded instruction forms). Full imm8 counts are used
without modulo masking. Oversized logical shifts zero each lane; arithmetic
right shifts sign-fill. Zero counts copy the source, with XMM destinations
still clearing the upper YMM half. Both source halves are captured before
YMM destination writes. Variable-count forms, legacy and EVEX remain outside
this increment.

Reference: AMD APM Volume 4 rev.3.24 pp.453–479
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
Its PSRAW threshold wording is inconsistent; Intel's
[instruction reference update](https://cdrdv2-public.intel.com/851055/252046-079-sdm-change-document.pdf)
confirms that word counts greater than 15 sign-fill the lane.

The matching build passes 610 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-shift-native/{tests,clippy,release}.log`,
process 20527 exit 0). The existing compare/shift suite passes 82,005 native
checks, including the actual pinned broadcast/compare/shift/store byte sequence
(`work/packed-shift-native/regression.log`, process 82223 exit 0).

The [expanded native verifier](../../rust/verification/packed_shift_native.rs)
passes 79,296 comparisons over 4,956 decoded fixtures. Widened per-lane
multiply/divide references cover all immediate counts, sign/edge/random lane
patterns, WIG=1, high/self aliases, zero-count upper clearing, overshifts and
preserved CPU state (`work/packed-shift-native/native.log`).
Pinned discovery passes eight former shift boundaries and exposes eight new
ones, reaching 55,560 instructions in 12,667 blocks with 207 unsupported
boundaries. This is instruction discovery evidence, not gameplay or a full
CPU compatibility ratio.

The combined entry verifier compiles and its startup-only run passes the
unchanged `fscanf` exit boundary: ten named dependencies accepted, 20,288
dependency bytes owned, cleanup pending and game main not executed
(`work/packed-shift-native/entry.log`, process 52698 exit 0, including the
new native suite above). Full GPU/integrated verification was not run for this
CPU-only increment.

## Packed scalar extraction

VEX VPEXTRB/W/D/Q now read the immediate-selected source lane into a GPR or
exact-width memory destination. Both word encodings are supported, including
the register-only 0F C5 encoding with reversed ModRM fields. B/W W0 and W1
results zero-extend into the entire GPR; memory stores remain 1/2/4/8 bytes.
The complete source YMM and flags/FP state remain unchanged. Immediate lane
masks are 15/7/3/1. Reserved L/vvvv, legacy/EVEX and FS/GS/address32 forms
remain rejected.

Reference: AMD APM Volume 4 rev.3.24, pp.328–335
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The build passes 609 library tests, three integration tests, strict Clippy and
release compilation (`work/packed-extract-native/{tests,clippy,release}.log`,
process 94594 exit 0). The [native verifier](../../rust/verification/packed_extract_native.rs)
passes 33,072 independent lane-byte comparisons over 2,648 decoded fixtures:
all immediate values, both word encodings, W0/W1, high registers, complete
source preservation, exact-end/high/unaligned/RIP stores, adjacent-byte
sentinels, every short span, readonly faults, flags/FP and execution guards
(`work/packed-extract-native/native.log`, process 22246 exit 0).

Pinned homebrew discovery passes the former VPEXTRD stop at 0x1a7e8a and
reaches 55,551 instructions in 12,667 blocks, an increase of 353 instructions.
Three new stops become reachable, leaving 207 unsupported boundaries.
The combined verifier compiles and startup-only execution retains its expected
missing `fscanf` exit: ten named dependencies accepted, 20,288 dependency bytes
owned, cleanup pending and game main not executed
(`work/packed-extract-native/entry.log`, process 22246 exit 0).
Native reference checks do not establish hardware differential equivalence,
full CPU coverage or retail-game execution.

## Packed scalar insertion

VEX VPINSRB/W/D/Q now insert the scalar into the immediate-selected lane of
the merge XMM source, preserving the other low128 bits and clearing the upper
YMM half. Lane indices mask to 4/3/2/1 bits. Byte/word W0 and W1 encodings
both work; their wider GPR operands still supply only the low byte/word.
Memory reads are exactly 1/2/4/8 bytes. The scalar read and fault guard precede
loading the merge vector, avoiding both premature writes and caller-saved
NEON clobbering by memory helpers. Legacy/EVEX, L=1 and FS/GS/address32 remain
explicit rejection boundaries.

Reference: AMD APM Volume 4 rev.3.24 pp.354–362 and Intel's
[VPINSRB/D/Q reference](https://cdrdv2-public.intel.com/782151/253667-sdm-vol-2b.pdf).
VEX.vvvv supplies the merge source; the older AMD table's reserved-vvvv entry
conflicts with its own prose and Intel's operand encoding, so it is not used
as a rejection rule.

The matching build passes 608 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-insert-native/{tests,clippy,release}.log`,
process 35240 exit 0). The [native verifier](../../rust/verification/packed_insert_native.rs)
passes 24,480 byte-array reference comparisons over 2,040 decoded fixtures,
covering all 256 immediate encodings, high/aliased registers, B/W W1 forms,
exact-end readonly high/unaligned/RIP loads, every insufficient memory span,
fault atomicity, flags/FP state and execution guards
(`work/packed-insert-native/native.log`, process 36998 exit 0).

Pinned discovery advances to 55,198 instructions and 12,621 blocks. Twelve
former boundaries disappear and ten new ones become reachable, leaving 205
unsupported boundaries. The combined verifier compiles and startup-only
execution passes the unchanged missing `fscanf` boundary: ten dependencies
accepted, 20,288 dependency bytes owned, cleanup pending and game main not
executed (`work/packed-insert-native/entry.log`, process 36998 exit 0).
This evidence does not establish full CPU coverage or retail-game execution.

## Packed integer broadcasts

VEX VPBROADCASTB/W/D/Q now support both XMM and YMM destinations. Register
sources use only the low scalar bits; memory sources read exactly 1/2/4/8
bytes. Input is captured before aliasing writes. XMM destinations clear the
upper YMM half, and integer copies leave flags and floating-point controls
unchanged. The decoder rejects reserved W=1 and vvvv fields. EVEX and
FS/GS/address32 remain unsupported; this change also closes the former D
branch's missing address32-absolute rejection.

Reference: AMD APM Volume 4 rev.3.24, pp.700–707
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf)).
The matching build passes 607 library tests, three integration tests, strict
Clippy and release compilation (`work/packed-broadcast-native/{tests,clippy,release}.log`,
process 78108 exit 0). An earlier run failed an obsolete test that expected
B/W/Q rejection; these now have positive acceptance tests, while EVEX and
unsupported-addressing rejection tests remain.

[Independent native verification](../../rust/verification/packed_broadcast_native.rs)
passes 25,074 checks over 72 decoded forms. It covers repeated/distinct byte
patterns, narrow source masking, aliases/high registers, both destination
sizes, exact-end readonly scalar loads, high/unaligned/RIP-relative memory,
every insufficient span, fault atomicity, FP/flags and execution guards.
The existing VPBROADCASTD verifier also passes 65,579 checks over every XMM
source/destination pairing (`work/packed-broadcast-native/native.log`, process
79699 exit 0). These comparisons use independent Rust byte-array references,
not x86 hardware differential measurements.

Pinned homebrew discovery reaches 55,128 instructions and 12,619 blocks, up
862 instructions from the prior checkpoint. Eight former boundaries disappear;
seven newly reachable ones appear, giving 207 unsupported boundaries. The
combined entry verifier compiles, and its startup-only run retains the expected
missing `fscanf` exit boundary: ten named dependencies accepted, 20,288 owned
dependency bytes, cleanup pending and game main not executed
(`work/packed-broadcast-native/entry.log`, process 79699 exit 0).

## VEX scalar vector moves

All four VEX VMOVQ encoding families now lower: GPR/m64 to XMM, XMM to
GPR/m64, XMM/m64 to XMM and XMM to XMM/m64. Reverse VMOVD (XMM to GPR/m32)
is also supported. Vector destinations clear modeled bits above the copied
scalar; dword GPR destinations zero-extend. Inputs are captured before aliasing
outputs change, stores preserve the source vector, and failed memory accesses
leave destinations unchanged. Exact Code selection excludes legacy and EVEX
forms. The decoder rejects reserved VEX.L=1; WIG variants remain accepted.
Address32 and FS/GS remain rejected.

Reference: retained AMD APM Volume 4 rev.3.24, pp.186–187 and222–223,
MOVD/VMOVD and MOVQ/VMOVQ ([AMD manual landing page](https://docs.amd.com/v/u/en-US/26568_3.26_APM_Vol4)).

The matching build passes 606 library tests, three integration tests, strict
Clippy and release compilation (`work/vmovq-native/{tests,clippy,release}.log`,
process 59874 exit 0). The [new verifier](../../rust/verification/vmovq_native.rs)
passes 86,352 native ARM64 checks across 42 decoded forms, with independent
byte-copy expectations, high registers, self-aliases, WIG encodings,
unaligned/high/RIP-relative memory, short spans, readonly stores, flags/FP
preservation and execution guards (`work/vmovq-native/native.log`, process
25408 exit 0). The existing forward VMOVD suite also passes all 8,219 checks
(`work/vmovq-native/vmovd-regression.log`, process 98946 exit 0); its obsolete
reverse-direction rejection was removed because the new suite verifies it.

Pinned homebrew discovery advances from 54,225 to 54,266 instructions and
12,507 blocks. Sixteen previous boundaries disappear (15 VMOVQ and one reverse
VMOVD); eighteen newly reachable boundaries appear, giving 208 total. This is
discovery progress, not executed gameplay or a CPU coverage percentage.
The combined verifier compiles and the startup-only run passes its existing
boundary assertions (`work/vmovq-native/entry.log`, process 25408 exit 0):
all ten named dependencies accepted, unresolved `fscanf`, 20,288 dependency
bytes still owned, cleanup pending and game main not executed.

## Byte division

DIV/IDIV r/m8 now translate through the Rust arithmetic helper. The dividend is
AX, with quotient in AL and remainder in AH. Upper RAX and all of RDX are
preserved. The explicit operand is captured before either output changes,
including AL/AH aliases and memory addressed through RAX. Zero divisors and
out-of-range quotients produce modeled divide-error diagnostic 13 without
committing registers; a failed operand load takes priority. Undefined flags
are preserved deterministically. This is not complete architectural exception
delivery.

Reference: AMD APM Volume 3 rev.3.31, DIV pp.173–174 and IDIV pp.177–178
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf)).
The build passes 605 library tests, three integration tests, strict Clippy and
release compilation (`work/byte-divide-native/{tests,clippy,release}.log`,
process 9716 exit 0).

[Native verifier](../../rust/verification/byte_divide_native.rs) passes
1,120,336 checks over 16 decoded forms (`work/byte-divide-native/native.log`,
process 69479 exit 0). Independent u32/i32 references cover every AX value
against six selected divisors, exhaustive AL/AH source aliases, randomized AX
with all byte divisor values, high registers, readonly unaligned/high/RIP
memory, address aliases, load failures, error atomicity, flags, FP state,
fuel and preexisting faults. These are ARM64 executions compared with a Rust
reference, not x86 hardware differential measurements.

Existing DIV/IDIV16/32/64 also pass 175,008 comparisons with the bit-serial
oracle, alongside the other arithmetic regression suites
(`work/byte-divide-native/wide-regression.log`, process 38005 exit 0).
The combined entry verifier compiles with warnings denied and includes the
new suite. Full startup was not rerun for this instruction increment.
LOCK/REP, FS/GS and address32 forms remain unsupported. No retail-game
execution or production-readiness claim follows from these checks.

## Unsigned multiplication

MUL r/m8, r/m16, r/m32 and r/m64 now lower to ARM64. Inputs are captured before
implicit outputs change, including accumulator/high-half aliases. Byte products
write AX; word products write DX:AX while retaining surrounding bits; dword
products zero-extend EDX:EAX; qword products write the complete RDX:RAX result.
CF and OF indicate a nonzero upper half. Undefined arithmetic flags are preserved
deterministically. A rejected memory load leaves all outputs and flags intact.

Reference: AMD APM Volume 3 rev.3.31, MUL p.253
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf)), also
inspected in the local retained manual. This increment does not change IMUL.

The build passes 604 library tests, three integration tests, strict Clippy and
release compilation (`work/mul-native/{tests,clippy,release}.log`, process 20749
exit 0). An old unsupported-instruction test used MUL as its rejection fixture;
it now uses still-unsupported RCL, while new tests assert MUL acceptance.
Independent native checks pass 754,413 cases over 33 decoded forms
(`work/mul-native/native.log`, process 15517 exit 0): exhaustive byte pairs,
wide edge/random inputs, u128 product reference, register aliases, AH/high
registers, partial writes, flags, readonly unaligned/high/RIP-relative memory,
aliased memory bases, load failures, FP-state preservation and fuel.

The full entry verifier includes this suite and compiles with warnings denied
(process 40562 exit 0). Full startup was not rerun for this isolated instruction
addition. No PS5 hardware differential run or gameplay improvement is established.
LOCK/REP, FS/GS and address32 forms remain explicit rejection boundaries.

## Narrow shifts and rotations

SHL/SHR/SAR/ROL/ROR now support byte and word operands, with register or memory
destinations and immediate, CL or implicit-one counts. Narrow counts use the
x86 five-bit mask, not a width-minus-one mask. SAR sign-extends its input;
rotations repeat the narrow bit pattern while preserving the masked count for
flag handling. AH and other partial-register writes retain surrounding bits.
Existing 32/64-bit result and writeback behavior remains unchanged.

Flags follow AMD APM Volume 3 rev.3.31: ROL pp.308–309, ROR pp.310–311,
SAL/SHL pp.315–317, SAR pp.318–320, SHR pp.334–336
([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf)).
CF follows the last shifted-out bit; AF for shifts and OF for multi-bit counts
retain their incoming values as the deterministic undefined-state policy.
Masked count zero preserves flags. The existing memory profile still validates
the read and store at count zero; faults preserve guest data and flags. These
guard checks do not establish complete architectural exception ordering.

The matching build passes 603 library tests, three integration tests, strict
Clippy and release compilation (`work/narrow-shift-native/{tests,clippy,release}.log`,
process 25709 exit 0). Independent native verification passes 502,020 checks
over 300 decoded forms (`work/narrow-shift-native/native.log`, process 34400
exit 0), including every CL byte count, immediate boundaries, partial-register
aliases, high unaligned memory, load/store failures, FP-state preservation,
fuel and 32/64-bit regressions. The reference shifts one bit at a time; no AMD
hardware differential run is claimed.

Static discovery reaches 54,225 instructions and 206 unsupported boundaries,
up from 54,147 instructions and 210 boundaries. All four previous narrow-shift
boundaries disappear, with no new boundary in this snapshot. The pinned sites
include ROL AL,5 and SHR DIL/SIL,CL. This is static reachability, not evidence
those payload paths executed. RCL/RCR, unsupported address modes and complete
fault delivery remain outside this increment.

Startup-only verification passes (`work/arm64-prototype/narrow-shift-entry.log`,
process 64379 exit 0), with the same missing fscanf boundary after all ten named
dependencies are accepted. Game main has not executed.

## Bit test, set and reset

Exact BT/BTS/BTR 32/64-bit forms now lower to ARM64, with register or memory
destinations and register or immediate indices. Register indices select a bit
modulo operand width for register destinations. For memory, signed register
indices adjust the accessed word before selecting the bit; immediate indices
remain within the addressed word. CF receives the original bit after successful
access, including successful stores for BTS/BTR. BT leaves its operand intact.
Non-CF flags are preserved as a deterministic policy; AMD marks OF/SF/ZF/AF/PF
undefined for these instructions. Register writes obey 32-bit zero extension.

Semantics reference: AMD APM Volume 3 rev. 3.31, BT pp.120–121, BTR pp.124–125,
BTS pp.126–127 ([manual](https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf)).
The source was also inspected locally in `work/amd-lzcnt.pdf`.

The build passes 602 library tests, three integration tests, strict Clippy and
release compilation (`work/bit-test-native/{tests,clippy,release}.log`, process
69866 exit 0). The native verifier passes 43,180 checks across 44 decoded
encodings (`work/bit-test-native/native-verified.log`, process 45303 exit 0):
register aliasing, signed indices, immediate masking, high addresses, unaligned
and biased RIP-relative reads, readonly BT, guarded stores, short spans, flags,
FP-state preservation and fuel. An initial verifier incorrectly expected a
load-fault code for readonly stores; it now checks load code 1 and store code 2
separately. State-preservation assertions were retained.

Static discovery of the pinned homebrew increases from 54,009 to 54,147
instructions. Ten old bit-operation boundaries disappear and one newly reached
shift boundary appears, so the total falls from 219 to 210. Before/after
manifests are `work/bit-test-native/{before,after}.json`. These are reachable
translation counts, not executed instruction coverage or a CPU percentage.

16-bit forms, LOCK variants, BTC, FS/GS, address32, architectural fault delivery
and hardware differential validation remain incomplete. The native reference
computes expected bits independently in Rust; it is not a PS5 hardware oracle.

The matching startup-only regression passes (`work/arm64-prototype/bit-test-entry.log`,
process 43037 exit 0), reaching the existing missing fscanf import after all ten
named dependencies are accepted. Game main has not executed.

## Packed signed comparison and logical shift

Exact VEX256 `VPCMPGTD ymm,ymm,ymm/m256` and `VPSRLD ymm,ymm,imm8`
are implemented. Comparison treats each lane as signed32 and emits all ones
or zero. The shift is logical; immediate counts at least32 produce zero.
All source registers are captured before aliased destination writes. Memory
comparison validates the complete32-byte span before committing any lane.
Flags and floating-point state are preserved.

The matching release passes 82,005 independent native checks
(`work/cpu-inventory/packed-release.log`, session39440 exit0), including signed
extrema, mixed random lanes, register aliases/high registers, all256 immediate
counts, unaligned/read-only/full64 inputs, short-source atomic faults and fuel.
Eight cases execute the pinned sequence at `0x11b255`: broadcast, comparison
at `0x11b25b` against signed constants0..7, shift31 at `0x11b263`, and store.
The reference computes eight boolean results independently of ARM64 lowering.
The full runner includes this verifier. VEX128, legacy and other operand forms
remain explicit gaps; this does not establish complete SIMD or game execution.

Semantics: [AMD APM Volume4 rev3.24, pp316–317 and470–471](https://kib.kiev.ua/x86docs/AMD/AMD64/26568_APM_v4-r3.24.pdf).

## VEX VPBROADCASTD

Exact VEX128/256 `VPBROADCASTD xmm/ymm,xmm/m32` now lowers to ARM64.
It snapshots the low 32 source bits before committing aliased destinations,
replicates them across four/eight lanes, and clears upper YMM bits for VEX128.
Memory inputs use guarded four-byte reads; failed reads preserve destination
state. Flags and floating-point state remain unchanged.

The matching release passes 65,579 independent native byte-reference checks
(`work/cpu-inventory/broadcast-release.log`, session88974 exit0): every register
pair at both widths, 128 patterns per pair, actual pinned RIP-relative encodings,
unaligned/read-only/exact-end inputs, short/wrapping faults and fuel stops.
The verifier is registered in the full entry runner. B/W/Q broadcasts, EVEX,
FS addressing and 32-bit addressing remain unsupported. These checks establish
the stated forms only, not full vector or CPU compatibility.

## Observed LZCNT32 boundary

The pinned `f3 0f bd f2` instruction at `0x1b58c3` now lowers to ARM64 leading
zero count for the exact `LZCNT r32,r/m32` form. It clears the destination's upper
32 bits, sets CF for zero input and ZF for zero result. Other arithmetic flags
are architecturally undefined and are preserved as a deterministic policy.
The [AMD APM Volume3, pp.227–228](https://kib.kiev.ua/x86docs/AMD/AMD64/24594_APM_v3-r3.31.pdf)
defines these semantics. Memory faults occur before destination/flag changes.

The matching release passes 24,614 native cases covering bit positions, random
inputs, flag set/clear, aliases/high registers, unaligned and RIP-relative reads,
short/wrapping addresses, FP-state preservation and fuel stops
(`work/lzcnt-native/release.log`, 75425 exit0). A focused form acceptance/rejection
test and strict Clippy pass. This does not implement LZCNT16/64, BSR, complete
ABM support or target feature discovery.

## x87 context integration

### Extended store consumption

Exact `FSTP m80fp` (`DB /7`) now lowers to a guarded Rust helper. It writes ten
payload bytes without binary64 conversion, then commits the stack pop. Failed
memory checks retain the complete stack and status. Empty-stack handling uses
the existing masked-indefinite or explicit unmasked-fault paths. Other FST/FSTP
forms remain unsupported.

The matching ARM64 verifier passes 907 cases spanning all TOP positions and
supported precision/rounding controls, signed zero/infinity/qNaN, precision
beyond binary64, unaligned stores, short/read-only destinations, empty stacks
and pending exceptions (`work/x87-store-review/native.log`). This is independent
byte/state expectation testing, not an AMD hardware differential run. The
verifier is included in the full entry runner; full startup has not been rerun
for this increment.

AMD's [FST/FSTP reference, pp.303–304](https://kib.kiev.ua/x86docs/AMD/AMD64/26569_APM_v5-r3.13.pdf)
specifies invalid-operation status for SNaN and unsupported formats. The helper
uses masked SNaN quieting with retained payload, and indefinite for unsupported
formats, following the general invalid-response policy. Exact FSTx NaN payload
propagation still needs AMD hardware validation. Pseudo-denormals stop explicitly
with unsupported-operand code20 because normalization/denormal-status behavior
is unresolved. Unit regressions cover these policy boundaries and protected
runtime memory.

Wait-first and memory-validation-before-new-stack-fault are prototype policies.
[AMD Volume2 §8.5](https://kib.kiev.ua/x86docs/AMD/AMD64/24593_APM_v2-r3.36.pdf)
allows implementation-dependent priority within the group containing memory and
pending x87 exceptions. Prototype stop18 is not complete architectural exception
delivery. No exact PS5 fault-ordering claim follows from these checks.

The Rust CPU now owns eight raw 80-bit registers and physical tags. Its size is
816 bytes, with the stack at offset 720; all historical prefix offsets remain
unchanged. Historical C++ 720-byte allocations are not valid extended contexts.
Native runners and runtime archives must be rebuilt together.

Continuation and abort snapshots retain the full stack. A dispatcher regression
seeds a nonzero TOP and precision beyond binary64, overwrites the handler state,
and checks exact restoration. Child creation now copies the stack along with its
control/status words, rather than combining inherited TOP with empty tags. This
is an explicit runtime inheritance policy pending PS5-specific verification.

Validation: 484 library tests, three integration tests, strict all-target Clippy,
release build, and the matching native arithmetic suite pass. Native arithmetic
checks preserve compatibility of the existing prefix; they do not verify x87
instruction execution. x87 arithmetic, architectural exception delivery and
callable `strtold` remain incomplete.

**CPU completion is unproven. No overall CPU percentage is available.** The existing progress graphic tracks selected checkpoints, not the complete instruction set. Instruction discovery counts from one executable are not a compatibility denominator, and discovered instructions are not necessarily executed instructions.

The [initial form inventory](CPU_INVENTORY.md) now maps implemented families to focused verification evidence and groups the pinned executable's observed static lowering boundaries. It records exact source/artifact hashes. It remains a starting audit, not an exhaustive target ISA inventory.

## Required evidence

A completion claim requires a versioned target CPU feature profile and an inventory of its instruction forms and architectural behavior. The profile must come from target evidence; unsupported features cannot be removed merely to make the inventory green. Deliberately excluded features require a documented target-based reason. That exhaustive inventory has not yet been established.

Each inventory row must identify the encoding/form, operand widths, memory/register variants, required control state, defined results and faults. Track implementation and verification separately:

| State | Meaning |
| --- | --- |
| Missing | The required behavior has no implementation. |
| Implemented, unverified | Code exists, but the required independent comparisons have not passed. |
| Partially verified | Specific cases passed; uncovered cases are listed. |
| Independently verified | The row's stated coverage and acceptance checks passed against an independent reference. |

For each verified row, record the implementation, verifier, reference provenance, tested commit/build, native host, inputs or reproducible seed, results, and known exclusions. Compare defined outputs and faults; do not incorrectly require equality for architecturally undefined state. A decoder accepting an instruction, or an encoder producing valid ARM64, is not execution verification.

## Completion gates

| Gate | Required proof | Current assessment |
| --- | --- | --- |
| Target inventory | Audited target feature profile and complete form/behavior inventory | Not established |
| Instruction semantics | Register, flag, memory, aliasing and width comparisons for every required form | Selected forms verified; incomplete |
| Floating point | Required rounding, denormals, NaNs, exception effects and x87/SIMD state | Selected scalar/control cases verified; incomplete |
| Faults and memory | Correct fault conditions, delivery, restart state, access boundaries and required memory ordering | Guarded prototype checks exist; architectural coverage incomplete |
| Atomics and concurrency | Required atomic instructions, ordering and contention behavior across guest threads | Not established; cooperative scheduling alone is insufficient |
| Execution integration | Reproducible real-program runs with no unexplained CPU mismatches or missing required behavior | Homebrew startup and selected routines run; no game main verified |
| Reproducibility | Runnable checks and retained evidence tied to the exact claimed revision | Individual verifiers exist; complete inventory-linked evidence is missing |

All required correctness gates must pass before claiming CPU completion. Performance is a separate claim requiring workload benchmarks on the target Mac; correctness tests do not prove native-level performance. Game playability additionally depends on system libraries, graphics, assets and the rest of the runtime.

## Existing evidence anchors

These are examples of current focused checks, **not an exhaustive coverage matrix**:

- [Implicit IMUL](../../rust/verification/imul_implicit_native.rs): selected signed multiply forms and result/flag checks.
- [x87 control](../../rust/verification/x87_control_native.rs): control/state behavior; not full x87 arithmetic or exception delivery.
- [Scalar multiply](../../rust/verification/vmulsd_native.rs), [fused multiply-add](../../rust/verification/fma213_native.rs), and [integer conversion](../../rust/verification/truncate_native.rs): selected native floating-point behavior.
- [Real tiler worker](../../rust/verification/tiler_native.rs) and [threaded wrapper](../../rust/verification/tiler_threads_native.rs): execution of selected homebrew routines and inputs.
- [Implementation history and recorded checks](../../rust/README.md): scope and limitations of individual milestones.

The next tracking step is to construct the target inventory, then link each row to this evidence or mark the gap. Until that inventory is audited, even a ratio of passing tests cannot serve as a CPU completion percentage.

## Observed allocator gaps

The native semaphore verifier exercises the pinned homebrew's actual SDL allocator rather than replacing its guest instruction sequence. The executable SHA-256 is `308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567`. These are concrete required forms discovered in that path, not the complete CPU inventory:

| Guest offset and bytes | Required behavior | Evidence and verification still required |
| --- | --- | --- |
| `0x18d0a`: `48 83 d7 00` (`ADC rdi, 0`) | Add incoming carry to the allocator size, with correct 64-bit result and arithmetic flags | Implemented. [Native verifier](../../rust/verification/adc_native.rs) passes 58,752 independent edge/random cases across selected 8/16/32/64-bit register, memory, immediate and alias forms, plus carry chains, guarded accesses and the allocator's CMP/ADC pair. The actual SDL allocator also passes in the semaphore fixture. |
| `0x18b52`: `f0 0f c1 07` (`LOCK XADD [rdi], eax`) | Atomically update SDL's allocation counter, return its previous value in EAX, and update arithmetic flags | Aligned 32-bit form implemented with sequentially consistent atomic RMW. Actual SDL allocator/counter execution passes in the semaphore fixture. [Dedicated native checks](../../rust/verification/atomic_xadd_native.rs) pass for flags, fuel, register preservation, address/source aliasing, access boundaries and guest/host alignment guards. A separate helper test exercises host-thread atomic contention; this does not prove concurrent guest execution. |

Evidence for this checkpoint: `allocator-cpu-native.log` (ADC), `atomic-xadd-native.log` (corrected standalone XADD verifier), and `semaphore-native.log` (actual SDL paths), against the matching release build. The ADC comparison uses an independently written widened-integer oracle, not an x86 hardware differential run.

Unsupported instruction forms, such as misaligned locked accesses, remain explicit gaps. These allocator and semaphore checks do not establish full atomics, memory-ordering or CPU coverage.

## Legacy MOVDQU discovered by signal verification

The expanded abort verifier stopped at synthetic handler offset `0x357` on `f3 0f 6f 83 d0 00 00 00` (`MOVDQU xmm0,[rbx+208]`). This was a verifier instruction, not a discovered retail-game or pinned-homebrew address. Existing VEX VMOVDQU support did not cover the legacy instruction.

Legacy 128-bit MOVDQU register copies and memory loads/stores are now lowered. Unlike VEX128 loads, legacy loads preserve the destination YMM upper half. Unaligned memory operands use the existing guarded 16-byte helpers; unsuccessful loads preserve every vector lane, and unsuccessful stores leave memory unchanged.

[Focused native verifier](../../rust/verification/movdqu_native.rs) passed on ARM64 against an isolated HEAD-plus-AOT debug build (`work/movdqu-review/native.log`, terminal session43488). Its independent byte-copy expectations cover both register encodings, self-copy, xmm15 loads, unaligned addresses, the exact abort displacement form, legacy upper-half preservation versus VEX128 zeroing, and short/unmapped/readonly memory guards with unchanged GPRs, flags and FP controls. The initial verifier incorrectly expected load-fault code1 for stores; it was corrected to the existing store-fault code2 before the passing run. No release archive was overwritten. This closes one concrete instruction gap, not the CPU inventory or full signal execution requirements.

The integrated tree subsequently passes 375 library tests, three integration tests, strict all-target Clippy and release compilation (88301). The same independent native checks also pass against that matching release (35321, `work/movdqu-review/release-native.log`). The verifier is registered in the full entry runner; that full runner has not been rerun for this CPU-only increment.

## VEX VMOVD input form

The exact `VEX_Vmovd_xmm_rm32` form is now lowered, including the pinned executable's `c5 f9 6e c1`. It copies the low32 source bits and clears the remaining 224 modeled vector bits only after a successful guarded read. The reverse direction, legacy encoding and VEX L=1 remain rejected.

An isolated HEAD-plus-lowering build passes 8,219 independent native byte-copy cases (`work/vmovd-review/native.log`, 62020), including register edges/random values, high registers, readonly unaligned/full-width/RIP-relative loads, short/unmapped memory, unchanged unrelated state and fuel exhaustion. Isolated strict Clippy passes. These checks do not establish support for other MOVD forms; matching full-run integration remains separate. The baseline inventory's static counts predate this change.

The same 8,219 cases also pass against the standard release archive (`work/vmovd-review/release-native.log`, 65815). With VMOVD lowering available, pinned startup discovery reaches 53,616 instructions and 228 unsupported boundaries: following previously blocked paths exposes additional boundaries rather than proving a monotonic percentage increase (`strftime-entry.log`).

## Signed64 VCVTSI2SD

The signed r/m64 form now converts using an integer-only rounding helper, preserves the source merge lane and clears the upper YMM half after successful conversion. It handles all MXCSR rounding modes, sticky precision and the prototype's explicit unmasked-precision fault without committing destination lanes. Invalid controls and failed memory loads retain their defined guard behavior.

An independent exact binary64-lattice oracle passes 79,714 native comparisons against the standard release (`work/cvtsi2sd-review/release-native.log`, 24378), including integer boundaries/random inputs, exact pinned encoding, merge aliases, high registers, DAZ/FTZ combinations, masked/unmasked precision, full-width unaligned memory and short-read guards. Nineteen focused scalar tests and isolated strict Clippy also pass. This closes the stated instruction form, not general CPU or architectural exception-delivery coverage.

## VEX128 VPXOR

The exact VEX128 `VPXOR xmm,xmm,xmm/m128` form is implemented. An isolated native verifier passes 7,257 independent byte-XOR cases, including destination/source aliases, high registers, pinned memory displacements, unaligned full-width addresses, readonly inputs, upper YMM clearing and failed reads of every insufficient span from zero through fifteen bytes (`work/vpxor-review/native.log`, 93509). GPRs, flags and FP controls remain unchanged. Isolated build and strict Clippy pass; standard-release verification is separate. VEX256, EVEX and legacy forms are not covered by this increment.

The same 7,257 cases pass against the standard protected-formatting release archive (`work/vpxor-review/release-native.log`, 56711). The verifier is now wired into the next full entry run.
