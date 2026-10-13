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
| Performance | Reproducible cold-start, translation latency, steady-state throughput and memory measurements on the target Mac | Synthetic loop baseline below; representative workloads, process cold-start and RSS still unmeasured |
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

## Retained-library budget

Generated libraries now have a configurable executor-wide budget: by default,
256 MiB of page-rounded Mach-O segment mappings and 256 retained libraries.
`translation_cache_usage` deduplicates shared library ownership across native
entries. `set_translation_cache_limits` rejects limits below current usage;
zero limits can disable compilation on an empty executor. No code is evicted
to satisfy a reduction. Removing an image releases its cache ownership.

The compiler reads its generated thin arm64 dylib and charges LC_SEGMENT_64
virtual sizes, including zero-fill, rounded to 16 KiB pages. It validates load
commands, section-table bounds, file spans, VM overflow and overlapping rounded
ranges. A batch exceeding the remaining byte budget is rejected before dlopen
and its temporary artifacts are removed. The library-count limit is checked
before compilation. The format reference is Apple's SDK `mach-o/loader.h`
(`mach_header_64`, `segment_command_64`, `section_64`) and `mach/machine.h`.

This is mapped-segment accounting, not measured RSS. Compiler peak memory,
dyld metadata, shared dependencies, host allocations, translation source data,
guest data and graphics resources remain outside this particular budget. The
16-attempt bound is retained. This does not prove the 16 GB total-memory target.

Verification: 601 library tests, three integration tests, strict Clippy and
release compilation pass (`work/translator-cache-{tests,clippy,release}.log`,
process 4452 exit 0). Native checks verify disabled compilation, both budget
failure paths, retained old execution, unchanged artifacts on rejection and
zero usage after unload (`work/translator-incremental/cache-native-final.log`,
process 67343 exit 0). This small fixture charges 32,768 bytes for its first
library and 98,304 bytes for three libraries; these are not game measurements.
Existing automatic/explicit translation regressions pass under default limits
(`work/translator-incremental/cache-regression.log`, process 95230 exit 0).
The real homebrew startup-only regression also passes under default limits
(`work/arm64-prototype/translator-cache-entry.log`, process 74074 exit 0),
stopping at the existing missing fscanf import. Game main remains unexecuted.

## Synthetic translation latency baseline

`rust/verification/translation_benchmark.rs` measures five fresh executors:
initial translation/publication, first execution, incremental CALL/RET batch
publication, and 100 checked executions through that batch. The guest loop sums
10000 down to 1; every result must equal the independently calculated 50005000.
Each warm invocation executes 30005 guest instructions, including CALL/RET.
Every sample verifies complete library retirement and removal of its artifacts.
The output directory must not exist; existing directories are never removed.

Reproduce from the repository root, with Rust on PATH:

```sh
cargo build --release --manifest-path rust/Cargo.toml
rustc --edition=2024 -O -Dwarnings rust/verification/translation_benchmark.rs \
  --extern native_ps5=rust/target/release/libnative_ps5.rlib \
  -L dependency=rust/target/release/deps -o /tmp/macps5-translation-benchmark
NATIVE_PS5_RUNTIME_ARCHIVE="$PWD/rust/target/release/libnative_ps5.a" \
NATIVE_PS5_AOT_DIRECTORY="$PWD/benchmark-artifacts-new" \
  /tmp/macps5-translation-benchmark
```

On 2026-10-13, Apple M5 Pro, arm64 macOS 26.5.1 (25F80), release runtime
at `ddcf3ed`, strict-warning compilation and the benchmark passed (process
10421 exit 0; workspace `work/translation-benchmark/confirmed.csv`). Results:

| Sample | Initial compile ms | Extension ms | First run μs | Warm mean μs/run |
| --- | ---: | ---: | ---: | ---: |
| 0 | 673.414 | 613.356 | 391.792 | 175.083 |
| 1 | 637.323 | 614.885 | 456.042 | 172.151 |
| 2 | 651.740 | 614.269 | 368.666 | 186.719 |
| 3 | 653.516 | 632.075 | 327.666 | 172.757 |
| 4 | 674.755 | 616.395 | 298.167 | 175.311 |

Each sample retained two libraries charging 4915200 mapped segment bytes.
These are repeated in-process measurements with warm filesystem/compiler caches,
not cold OS launches. Compile timings include discovery, assembly emission,
clang/linking, dylib loading and publication; they do not isolate each stage.
Warm timings include host dispatch and result checks. First-run timing uses the
original loop entry, while warm timing includes the additional CALL/RET batch.
No native reference ratio, game FPS, SIMD/memory workload coverage, peak RSS,
16 GB fit or production readiness follows from this tiny integer loop. The
baseline makes compilation overhead measurable before replacing the backend.

## Generated-library export pruning

The module compiler now exports only the `_module_aot_<block start>` symbols
that it resolves with `dlsym`, and passes `-dead_strip` to Apple's linker.
Runtime helpers remain private; functions and data reachable from block entries
remain linked. Export names are generated from the emitted batch manifest,
written inside the owned temporary directory, and removed with the library.
The installed Apple `ld(1)` manual documents both `-exported_symbols_list` and
`-dead_strip`. This retains clang compilation and the matching archive contract.

The same five-sample benchmark now charges 458752 bytes for its two libraries,
down from 4915200 bytes in the preceding baseline (90.7% less mapped segment
space for this fixture). Initial compilation ranges from 605–631 ms and
extensions from 611–644 ms. The runs are not controlled enough to claim a timing
speedup, and they still expose substantial compilation latency. The mapped-byte
reduction is not an RSS or whole-game memory reduction.

`incremental_translation_native.rs` also inspects the initial generated dylib
with `nm -gjU` and verifies that only its expected block entry is exported.
Its execution, cumulative-budget, rejection-preservation and complete-retirement
checks pass. The benchmark's 505 checked executions and retirement checks pass.
682 library tests, three integration tests, strict Clippy and release build pass
(`work/translation-link-pruning/`; build process 25428, benchmark/incremental
process 35394, both exit 0). No new instruction coverage is claimed.
Explicit and automatic translation regressions also pass, including cross-image
CALL/JMP/RET, cached-target reuse and exactly-once effects. The homebrew
startup-only check still accepts ten dependencies and stops at missing `fscanf`
before game main (process 25355 exit 0, `regression.log` and `entry.log` in the
same directory). This does not verify retail games or graphics.

## Compilation stage timing

Set `NATIVE_PS5_TRANSLATION_TIMINGS=1` when running the benchmark (or another
host using the module compiler) to emit one `TRANSLATION_TIMING` line to stderr
for each successfully compiled batch. Other values and an unset variable leave
diagnostics silent. Stdout remains available for the benchmark CSV.

The nanosecond wall-clock stages are disjoint: ELF discovery/manifest/assembly
emission, filesystem preparation, the xcrun clang/link subprocess, generated
Mach-O validation, `dlopen`, and symbol lookup/entry construction. Emitted
instruction/block counts describe the new batch. The timings end before the
executor's final live-byte validation and publication. Failed compilations do
not emit this success-only timing record. The compiler still enforces the same
translation and mapped-memory budgets.

On the same M5 Pro, ten measured batches (five initial and five incremental)
showed `dlopen` taking 498.5–533.0 ms, versus 81.2–92.5 ms in clang/linking.
Discovery/emission took 0.108–0.827 ms. This identifies library loading as the
dominant measured stage for this fixture; it does not identify the underlying
OS cause or extrapolate to large workloads. The existing direct ARM64 backend
plan should target this repeated dylib-load cost as well as subprocess latency.
It must retain executable-memory permissions, instruction-cache coherency,
ownership, relocation and guarded-memory correctness.

Evidence: `work/translation-stage-timing/stages.log` contains all ten records;
`results.csv` contains five samples with all 505 result checks and complete
retirement passing. Release build, strict-warning benchmark compilation and
execution completed with process 46692 exit 0.
682 library tests, three integration tests and strict Clippy also pass. A second
benchmark run without the variable passes all result/retirement checks and
produces zero stderr bytes (`default.csv`, `default.stderr`; process 34676
exit 0). The diagnostics add no instruction support or gameplay compatibility.

## Direct-backend executable storage

`rust/src/jit_memory.rs` adds bounded append-only ARM64 storage using MAP_JIT,
per-thread write protection and instruction-cache invalidation, following
[Apple's JIT guidance](https://developer.apple.com/documentation/apple-silicon/porting-just-in-time-compilers-to-apple-silicon)
and installed SDK declarations. Capacity is an exact whole number of host pages.
Empty, overflowing and exhausted ranges are rejected before writes. Published
code cannot be overwritten through the API; entry offsets must be aligned and
inside the published allocation.

Rc ownership pins the mapping after allocator destruction until the last code
handle drops. Handles cannot cross host threads. Only one arena is permitted
through this module at a time; a failed unmap retains the ownership lock.
There is no individual allocation reuse, eviction or host parallelism. Arena
accounting is not yet integrated with the executor's dylib budget.

Construction/publication are unsafe: callers must supply trusted fully relocated
ARM64 with a known ABI, retain external targets, coordinate JIT protection and
prevent signal-handler reentry. The write window performs no allocation or user
callbacks. A guard restores executable protection before cache invalidation and
publication. Hardened-runtime hosts require compatible signing entitlements;
callback-allowlist signing modes and other JIT engines are not integrated.

Verification on M5 Pro: 683 library tests, three integration tests, strict Clippy,
release build and strict-warning native verifier compilation pass (process
95294 exit 0, `work/jit-memory/`). `jit_memory_native.rs` executes 4098 MOVZ/RET
calls, fills 16 KiB exactly, checks rejected allocations and offsets, executes
old code after new publication and allocator destruction, and remaps only after
final handle release. It does not probe permission faults or hardened signing.

From the repository root after building release:

```sh
rustc --edition=2024 -O -Dwarnings rust/verification/jit_memory_native.rs \
  --extern native_ps5=rust/target/release/libnative_ps5.rlib \
  -L dependency=rust/target/release/deps -o /tmp/macps5-jit-memory-check
/tmp/macps5-jit-memory-check
```

Guest translation still uses dylibs. Direct instruction encoding, relocations,
helper calls, executor publication integration and correctness gates remain.

## Direct ARM64 branch emission

`rust/src/arm64_code.rs` adds a bounded instruction buffer with local labels and
final branch fixups: B, BL, B.cond (EQ–LE), CBZ/CBNZ (W/X) and TBZ/TBNZ
(bits 0–63). Displacements are signed instruction offsets relative to the branch
itself. Operand validation, duplicate/unresolved labels, targets at the end of
the buffer, emission budgets and displacement overflow fail before executable
publication. It has no external relocations or automatic long-branch veneers.
Raw instruction emission is not a validator; JitArena publication remains unsafe.

Verification: 685 library tests, three integration tests, strict Clippy and
release compilation pass. The standalone Rust verifier compares 8480 forward/
backward encodings against xcrun clang's assembled text bytes, covering all
register encodings, all bit indices and supported conditions. Unit checks cover
signed displacement bounds and rejection behavior. A generated loop performs
10000 iterations and returns the independent sum 50005000 on each of 100 native
runs in the executable arena. Process 66170 exit 0; `work/arm64-code/` logs.
The native loop covers B.NE execution; other branch forms have assembler-byte
comparison, not separate native execution coverage. No guest instruction has
been migrated to this backend by this change.

Reproduce after a release build:

```sh
rustc --edition=2024 -O -Dwarnings rust/verification/arm64_code_native.rs \
  --extern native_ps5=rust/target/release/libnative_ps5.rlib \
  -L dependency=rust/target/release/deps -o /tmp/macps5-arm64-code-check
/tmp/macps5-arm64-code-check arm64-oracle-new
```

The oracle output directory must not exist. Clang is used only as the encoding
oracle here; the tested buffer construction and native loop publication use
Rust and MAP_JIT directly.
