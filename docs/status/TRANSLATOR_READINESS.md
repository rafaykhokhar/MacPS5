# CPU translator production readiness

CPU translation is the current implementation priority. System-library import
expansion is deferred while translator correctness and execution infrastructure
are developed. This document is an acceptance plan, not a completion claim.

## Release requirements

| Area | Required evidence | Current boundary |
| --- | --- | --- |
| Target CPU contract | Versioned PS5 feature/control-state profile and complete encoding/operand inventory, independently established | No complete profile or coverage denominator |
| Instruction semantics | Independent register, flags, alias, width and memory-result checks for every required form | Selected forms tested; integer, vector and addressing gaps remain |
| Floating point | Required precision, rounding, NaN, denormal, exception and saved-state behavior | Selected scalar and x87 operations only |
| Faults and recovery | Correct fault classification, delivery, ordering and restart state | Runtime guard stops do not establish architectural exception delivery |
| Atomics and threading | Required atomic operations and memory ordering under contention | Cooperative execution and aligned LOCK XADD32 are insufficient |
| Translation lifecycle | Incremental compilation, bounded code ownership, safe publication/retirement and repeatable cache identity | Incremental compilation verified below; no production code cache yet |
| Performance | Reproducible cold-start, translation latency, steady-state throughput and memory measurements on the target Mac | No native-level performance claim established |
| Deployment | Standalone installation, tested macOS execution permissions, diagnostics and failure recovery | Compilation currently depends on xcrun/clang and a matching runtime archive |
| Integration | Sustained representative CPU workloads with indirect branches, mixed instruction families and thread interactions | Existing native fixtures and partial homebrew startup are narrower evidence |

The 16 GB target includes code cache, runtime state, game memory and graphics
resources together. A guest virtual-address reservation is not evidence of a
16 GB host-memory fit. CPU readiness also does not establish GPU, system-library
or retail game compatibility.

## Immediate sequence

1. Stop rebuilding old ARM64 code on every newly discovered target. Preserve
   union instruction-overlap and budget checks while publishing new batches.
2. Add bounded cache accounting and measured translation costs before relaxing
   the current 16-attempt limit. Avoid unbounded accumulation of linked runtimes.
3. Retain discovery ownership information and introduce a direct ARM64 code
   generation backend with macOS executable-memory and cache-coherency handling.
4. Establish the target inventory and independent per-form verification matrix;
   expand missing instructions and architectural behavior against that matrix.
5. Run correctness, stress, performance and deployment gates before calling the
   translator production ready.

These tracks can overlap, but correctness and lifetime checks must survive
backend changes. Removing safety limits or hiding unsupported instructions is
not a readiness milestone.

## Audit references

`module_compiler.rs` emits assembly, invokes clang and loads a dylib.
`NativeExecutor::extend_image_translation` originally rebuilt and replaced the
whole root union. Blocks retain their libraries through reference ownership.
The existing `modules_native.rs` tests cover observed-target compilation,
CALL/JMP/RET continuation, repeated-target reuse and rejection boundaries.
Quantum, scheduler and thread-pinning verifiers cover additional lifecycle
behavior and must remain regression gates.

`CPU_INVENTORY.md` is a dated snapshot, not the current missing-instruction list.
Later entries in `CPU.md` supersede individual rows. A regenerated inventory
must distinguish accepted forms from independently verified forms.

## Incremental compilation checkpoint

Extensions now emit and compile only previously unregistered instruction PCs.
Complete-union discovery still enforces overlap and cumulative instruction
limits. Both old and new live executable bytes are checked before an atomic
append. Existing libraries stay pinned, and removing the image translations
retires every batch. The public manifest continues to describe the full union.

Build verification passes 599 library tests, three integration tests, strict
Clippy and release compilation (process 8385 exit 0;
`work/translator-incremental-{tests,clippy,release}.log`). Native verification
passes (`work/translator-incremental/native.log`): retained original artifacts,
new assembly excluding old labels, cross-batch JMP into an existing instruction
boundary, CALL/RET, unchanged code after rejected overlap/unsupported/modified
code, and complete retirement. The existing explicit and automatic translation
regressions also pass, including cumulative budgets and exactly-once effects
(`work/translator-incremental/regression.log`, process 55242 exit 0).

The dedicated verifier is `rust/verification/incremental_translation_native.rs`.
After building release, compile it with the matching `libnative_ps5.rlib` and
release dependency directory. Run with `NATIVE_PS5_RUNTIME_ARCHIVE` pointing to
the matching `libnative_ps5.a` and `NATIVE_PS5_AOT_DIRECTORY` naming a fresh empty
directory. It rejects a nonempty directory to avoid stale artifact evidence.

This checkpoint retains synchronous full-union discovery, clang/link subprocess
latency and the 16-attempt bound. Multiple dylibs retain linked runtime code;
their resident-memory cost is not yet budgeted or measured. No speedup percentage,
persistent cache, parallel host execution or production JIT claim follows.
