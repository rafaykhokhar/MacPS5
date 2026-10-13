# CPU completion criteria

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
