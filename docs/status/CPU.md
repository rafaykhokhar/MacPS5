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
