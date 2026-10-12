# CPU completion criteria

**CPU completion is unproven. No overall CPU percentage is available.** The existing progress graphic tracks selected checkpoints, not the complete instruction set. Instruction discovery counts from one executable are not a compatibility denominator, and discovered instructions are not necessarily executed instructions.

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
