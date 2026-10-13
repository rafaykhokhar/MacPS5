# CPU completion criteria

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
