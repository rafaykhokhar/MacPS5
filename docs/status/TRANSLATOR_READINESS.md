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

## Direct helper calls

The ARM64 buffer now emits full-width constants into X0–X30 with four MOVZ/MOVK
instructions, plus absolute calls through ABI scratch register X16 and BLR.
Constant materialization preserves condition flags. Multiword emission checks
the complete instruction budget before modifying the buffer; rejected targets,
registers or insufficient capacity leave previous emission intact. Null and
unaligned call addresses are rejected. The caller still owns stack alignment,
LR/caller-saved preservation, target lifetime and function-ABI validity.

The extended `arm64_code_native.rs` matches 155 constant sequences and an
absolute-call sequence to clang bytes, in addition to all 8480 branch cases.
A generated stack-preserving call stub invokes the actual Rust
`native_ps5_arithmetic_flags_v1` helper for 1440 argument combinations, checking
six integer arguments and its two-register result against direct invocation.
Invalid operation/width cases verify fault results too. This is an ABI test;
independent arithmetic semantics remain covered by the helper's existing tests.
The 100 direct loop runs also pass. Matching release build, strict Clippy,
686 library tests and three integration tests pass (process 73719 exit 0,
`work/arm64-helper-call/`). Use the preceding verifier command to reproduce.

This removes the need for linker-resolved calls in this direct emission path.
It does not yet connect guest instruction translation, guarded-memory calls,
CPU context handling or executor publication to the new backend. The normal
guest execution path still uses dylibs.

## Direct CPU context and guarded-memory bridge

The direct emitter supports W/X unsigned-offset LDR/STR for trusted host context
fields. It validates registers, scaled alignment and the 12-bit offset range.
These operations must not be used to dereference guest virtual addresses.
`jit_guarded_memory_native.rs` generates call stubs for the existing guarded runtime
load/store helpers. The load stub reads `Cpu.fault` and writes its result to the
guest register only on success; callee-saved context and LR survive helper calls.

The encoding verifier compares 12288 context-access instructions with clang
(all registers and bases, both widths/directions, zero/one/max offsets), along
with the previous branch/constant/call cases. The generated memory bridge passes
392 native calls covering aligned/unaligned valid accesses, invalid widths,
unmapped/boundary/overflow addresses, read-only storage and preexisting faults.
Failed stores preserve all bytes; failed loads preserve destination registers.
Load values/faults are compared to the existing Memory API; store expectations
are computed from the test mapping and byte pattern. This is bridge verification,
not independent proof of every guest-memory semantic.

687 library tests, three integration tests, strict Clippy, release build,
strict-warning verifiers, 100 native loop sums and 1440 arithmetic helper ABI
checks pass (process 24038 exit 0; `work/direct-memory/` final logs).
Compile/run `rust/verification/jit_guarded_memory_native.rs` with the matching release
rlib/dependency flags used by the preceding verifiers; it needs no arguments.
The normal guest translator/executor still uses dylibs. Guest decoding, fuel,
PC progression and block publication have not yet been connected to this path.

## Initial direct guest translation integrated with executor

`direct_cpu.rs` now decodes selected x86 register MOV32/MOV64, immediate
MOV32/MOV64, NOP and RET64 forms and emits ARM64 words directly. Blocks must end
in RET; unsupported instructions, memory operands, truncated bytes, trailing
bytes after RET and guest/emission budget exhaustion return explicit errors.
32-bit writes zero-extend the destination. The generated Cpu ABI entry preserves
callee-saved state, honors preexisting faults, selects an instruction boundary
from guest RIP, charges fuel per instruction, and uses the existing return/fuel
dispatcher stop values. Invalid entry PCs fault before consuming fuel.

`NativeExecutor::register_direct_in_image` validates live executable bytes and
registration conflicts before publication, compiles and publishes to the caller's
JitArena, and pins the code in the registered block. Removal releases that code
ownership. Arena capacity remains an explicit separate budget from the dylib
cache; capacity is not reclaimed per block. This opt-in registration does not
silently fall back or replace automatic dylib compilation. It is a first
integrated slice, not a complete backend migration.

`direct_cpu_native.rs` verifies 4096 native register-move state/fuel/fault cases
(both widths and all source/destination registers), 512 invalid-PC cases, and
32 full-width/immediate/NOP sequences. An actual Image/NativeExecutor run of
`MOV EAX,42; RET` returns 42 after the arena allocator is dropped, proving code
ownership survives registration. Wrong and modified guest bytes are rejected;
duplicate registration consumes no code space, and removing the final block
allows a new arena. Native execution performs no clang or dlopen operation.

Matching release build, strict Clippy, 687 library tests and three integration
tests pass (process 58988 exit 0). The standalone verifier uses the same release
rlib/dependency flags as previous native verifiers and requires no arguments.
Logs are in `work/direct-cpu/`. Guest arithmetic, memory operands, branches,
calls, vectors, floating point and automatic translation discovery remain on
the existing backend. This does not demonstrate retail game compatibility.
Final native verifier and integrated startup-only regression pass (process
11361 exit 0, `native-final.log` and `entry.log`). Existing startup still stops
at missing `fscanf`, after accepting ten dependencies and before game main.

## Direct memory MOV lowering

The direct translator now lowers MOV32/MOV64 between general registers and
memory through the existing Rust guarded load/store helpers. Effective-address
generation handles 64-bit base/index/scale/displacement and RIP-relative image
bias; arithmetic wraps at 64 bits. FS/GS and 32-bit addressing remain explicit
errors. Loads commit their destination only after a successful guard check;
stores stop on faults before subsequent guest instructions execute. Existing
fuel and PC behavior is preserved at the faulting instruction.

`direct_mov_memory_native.rs` passes 10260 native cases: all data/base register
aliases, both widths/directions, signed negative displacement, unaligned spans,
read-only/unmapped/overflow addresses, RIP-relative, absolute and every index
scale with and without a base. Expected register and memory bytes are calculated
from the fixture, independently of the guard helper. An integrated Image and
NativeExecutor block loads an immediate guest pointer, reads 42 from the image,
and returns 42 using direct translated execution without clang/dlopen.

687 library tests, three integration tests, strict Clippy and release build pass
(process 54819 exit 0). Native addressing cases and the previous 4128 direct
state/fuel/fault plus 512 invalid-PC regressions pass (process 4604 exit 0).
The initial executor fixture used unmapped offset 0x300 and correctly faulted
(process 74013 exit 101). Correcting the data to mapped offset 0x180 makes the
full verifier, including the executor memory read, pass (process 69500 exit 0,
`corrected.log`). Evidence is in `work/direct-mov-memory/`. Compile/run this standalone
verifier with the preceding release rlib/dependency flags; no arguments required.
Byte/word MOV, memory-immediate MOV and other instruction families remain outside
this direct path. The backend remains opt-in and incomplete.

## Direct guest control flow

The direct translator now accepts short/near JMP and all 16 short/near Jcc
conditions within the submitted block. It resolves guest targets against decoded
instruction boundaries before emission. Instruction-interior and external
targets fail explicitly; indirect JMP, LOOP and calls remain unsupported here.
Conditional branches use the existing guest-flags condition helper, then branch
on its result. Every iteration re-enters the target's guest PC/fuel checks.
The block still must end in RET64; this does not implement arbitrary control-flow
discovery or cross-block direct linking.

`direct_branch_native.rs` passes 5120 independent truth-table/fuel cases:
16 conditions × short/near × 32 flag combinations × five fuel budgets. It checks
register preservation, flags, PC, consumed fuel and stop reason. Three backward
loops (short/near JMP and JNE) exhaust fuel deterministically. Invalid target and
unsupported-form rejection pass. An Image/NativeExecutor branch program returns
7 instead of the skipped immediate 42. Direct memory MOV (10260 cases and image
read) and register MOV (4128 cases, 512 invalid PCs, ownership and stale-byte
checks) regressions also pass against the final build. 687 library tests, three
integration tests, strict Clippy and release build pass (process 57795 exit 0;
`work/direct-branch/` final logs). Compile/run the new standalone verifier with
the preceding release rlib/dependency flags; no arguments required.

This adds functional guest control flow to the opt-in backend. Arithmetic,
cross-block translation, automatic discovery and retail gameplay remain
unverified or unimplemented in this direct path.

## Direct integer arithmetic

Direct lowering now supports register/register ADD, SUB, CMP, AND, OR, XOR and
TEST at 32/64-bit widths. ARM64 computes the result and the existing Rust helper
computes x86 flags from captured operands/result. Operand capture precedes alias
writes; 32-bit destination writes zero-extend. CMP/TEST preserve the destination.
The helper uses fixed valid width/operation arguments and does not fail for these
inputs. Logical AF follows the existing helper's zero policy for undefined AF.
Memory/immediate arithmetic, byte/word widths, carry-input and multiplication
families remain unsupported by this direct lowering.

`direct_arithmetic_native.rs` passes 86016 native cases covering both widths,
all source/destination register aliases and integer extrema. Its independent
oracle uses widened signed arithmetic for overflow, widened unsigned arithmetic
for carry, nibble arithmetic for AF and result bits for sign/zero/parity. This
verifier exercises one register encoding direction per family; it is not an
exhaustive encoding inventory. A real Image/NativeExecutor program computes
1+...+10000 = 50005000 using direct MOV/ADD/SUB/JNE/RET, with no clang/dlopen.

687 library tests, three integration tests, strict Clippy, release build and
strict-warning native verifier pass (process 7573 exit 0). Direct register
(4128 plus 512 invalid-PC), memory (10260) and branch (5120) regressions pass
against the same build (process 35157 exit 0). Logs: `work/direct-arithmetic/`.
Compile/run the new standalone verifier with the preceding release rlib and
dependency flags; no arguments required. The direct backend remains opt-in and
incomplete; this integer-loop result is not game compatibility evidence.

## Direct immediate arithmetic

ADD/SUB/CMP/AND/OR/XOR/TEST direct lowering now accepts the corresponding
immediate operands on 32/64-bit general-register destinations. It distinguishes
unsigned 32-bit immediates in 32-bit operations from sign-extended imm8 and
sign-extended imm32 in 64-bit operations using decoded operand kinds. Results
and flags use the existing captured-operand arithmetic path. TEST has no imm8
form. Memory and byte/word arithmetic remain unsupported in this direct path.

`direct_immediate_native.rs` passes 20640 native cases with an independent
widened/nibble/result-bit oracle: both widths, all destination registers,
selected positive/negative/extreme imm8 and imm32 values, group and accumulator
encodings, arithmetic and logical flags, and destination preservation for
CMP/TEST. A real executor loop using `SUB ECX,1` computes 50005000 from 10000
iterations without clang/dlopen. This is sampled constant coverage, not an
exhaustive immediate-value inventory.

687 library tests, three integration tests, strict Clippy, release build and
the new strict-warning verifier pass (process 17829 exit 0). Register arithmetic
(86016), register MOV (4128 plus 512 invalid PCs), memory MOV (10260), and branch
(5120) regressions pass against the same build (process 21085 exit 0). Logs:
`work/direct-immediate/`. Compile/run the standalone verifier with the preceding
release rlib/dependency flags; no arguments required. Automatic translation,
broader instruction coverage and retail compatibility remain unfinished.

## Identical-workload backend comparison

`backend_benchmark.rs` runs identical MOV/ADD/SUB-immediate/JNE/RET guest bytes
through direct registration and dylib compilation. It uses fresh executors and
images for five samples per backend, alternating backend order. Every first run
and each of 100 warm runs verifies 50005000; each invocation executes 30003 guest
instructions. Retirement checks follow every sample. It measures arena setup
separately, then translation/publication, first execution and warm execution.

On M5 Pro with the matching release runtime at `45322bd`, all 1010 result checks
and retirement checks pass (process 30601 exit 0). `work/backend-benchmark/`
contains the build log and raw `results.csv`:

| Sample | Direct arena μs | Direct translation μs | Dylib translation ms | Direct warm μs/run | Dylib warm μs/run |
| --- | ---: | ---: | ---: | ---: | ---: |
| 0 | 15.750 | 27.375 | 585.498 | 76.775 | 108.873 |
| 1 | 1.667 | 7.125 | 581.584 | 73.368 | 119.453 |
| 2 | 5.000 | 21.458 | 583.813 | 71.412 | 125.110 |
| 3 | 1.333 | 5.792 | 591.660 | 78.723 | 110.429 |
| 4 | 4.459 | 21.958 | 592.262 | 74.762 | 121.355 |

Direct emission occupies 732 bytes in a 16384-byte arena; the dylib path charges
425984 mapped bytes. Direct helper code resides in the host executable, outside
this arena accounting, while dylib mappings include linked helper code. These
figures are not total-RSS measurements or a whole-game memory comparison. CSV
`direct_code_bytes=0` for dylibs means not measured, not zero executable code.
The repeated in-process runs are not cold OS startup tests. Warm timings include
host dispatch and result checks. This selected supported loop demonstrates the
benefit of removing repeated library loads; it provides no compatibility,
native-port performance or game FPS claim.

Reproduce from the repository root after a release build:

```sh
rustc --edition=2024 -O -Dwarnings rust/verification/backend_benchmark.rs \
  --extern native_ps5=rust/target/release/libnative_ps5.rlib \
  -L dependency=rust/target/release/deps -o /tmp/macps5-backend-benchmark
NATIVE_PS5_RUNTIME_ARCHIVE="$PWD/rust/target/release/libnative_ps5.a" \
NATIVE_PS5_AOT_DIRECTORY="$PWD/backend-benchmark-new" \
  /tmp/macps5-backend-benchmark
```

The artifact directory must not exist. Backend selection remains explicit; the
benchmark does not enable incomplete direct translation for other workloads.

## Direct branches between registered blocks

Direct JMP/Jcc targets outside the submitted span now emit dispatcher exits
with guest RIP/target and stop reason 2. Conditional fallthrough at the span end
uses the same path. Spans may end in supported branches as well as RET64.
Targets inside the submitted span still require decoded instruction boundaries;
labels and emission budgets are checked before publication. This is dispatcher
handoff, not patched native cross-block branches or automatic discovery. The
existing executor enforces target registration, live guest bytes, ownership,
fuel and transition limits. Missing targets do not silently execute or fall back.

`direct_cross_block_native.rs` passes 1024 short/near external Jcc truth-table
and state checks. Real executor tests verify separately registered taken and
fallthrough destinations returning 42/7, missing-target rejection, and a loop
between two separately registered JMP blocks stopped by fuel. Its transition
budget is deliberately larger than fuel to distinguish the two limit checks;
the first equal-budget run stopped at the transition limit instead. The initial
verifier build also needed `.err().expect(...)` because Execution lacks Debug.
After those verifier corrections the strict-warning build/run passes (command
chunk f93dbc, exit 0; `work/direct-cross-block/native.log`).

687 library tests, three integration tests, strict Clippy and release build
passed before the standalone verifier step (logs in the same directory; process
64369 subsequently exited 1 on that verifier compilation error). Direct branch,
immediate arithmetic, memory and register regression verifiers pass against the
same runtime (process 16274 exit 0). Reproduce the new standalone verifier using
the preceding release rlib/dependency flags; no arguments required. Calls,
indirect branches and automatic direct translation remain unfinished.

## Direct relative calls

Direct `CALL rel32` now publishes the image-biased target, image-relative return
PC and call stop reason to the existing executor. The executor validates the
destination and performs guarded guest stack push/pop. CALL must be the last
instruction of a submitted span; its return continuation must be registered
separately. Instructions after CALL in the same span are explicitly rejected.

`rust/verification/direct_call_native.rs` passes 108 native state checks across
positive/negative displacements, image biases, fuel and pre-existing faults.
Twenty nested executor runs return 42 and verify stack restoration. Additional
executor cases reject a missing callee, missing return continuation and an
unmapped return stack. These are synthetic checks, not game compatibility proof.

Validation on the local Apple Silicon host: 687 library tests, three integration
tests, strict Clippy, release build and the strict-warning standalone CALL
verifier all passed (process 74532, exit 0; logs under the original workspace's
`work/direct-call/`). Build the standalone verifier with the release rlib and
dependency flags shown above; it takes no arguments. Indirect calls, automatic
direct translation and production readiness remain unproved.

Existing direct cross-block, internal-branch, immediate-arithmetic, memory-MOV
and register-MOV native verifiers also pass against this runtime (process
92757, exit 0; logs in the same directory).

## Direct indirect calls

Direct `CALL r/m64` resolves an absolute guest target from a general register or
the existing guarded 8-byte memory-load helper. It publishes call state only
after a successful pointer read. Faulting reads preserve target, continuation,
registers and flags; they consume instruction fuel and report the memory fault.
The dispatcher still owns destination validation and stack writes. `[RSP]`
therefore reads its pointer before the return-address push. Existing unsupported
FS/GS and 32-bit addressing restrictions remain explicit.

`rust/verification/direct_indirect_call_native.rs` verifies 960 native cases:
all 16 register operands and memory bases, unaligned/boundary/invalid addresses,
zero/nonzero fuel and prior faults. Three real executor programs return 42 via
register, RIP-relative pointer-table and `[RSP]` calls. The latter stores a
pointer on the guest stack and discards it after return, also checking that the
executor's call push did not overwrite the pointer before target resolution.

687 library tests, three integration tests, strict Clippy and release build
pass (process 20595, exit 0). The final strict-warning standalone verifier plus
relative-call, cross-block branch, memory-MOV and register-MOV regressions pass
(process 64054, exit 0). Reproduce with the release rlib/dependency flags above;
the verifier takes no arguments. This expands explicit direct registration;
automatic direct translation, indirect jumps and production readiness remain
unfinished. No PS5 game execution is established by these checks.

## Direct indirect jumps

Direct `JMP r/m64` reuses guarded indirect operand resolution, then publishes
the absolute target with dispatcher stop reason 5. It leaves the return-PC
field and guest stack unchanged. The existing executor validates the target
and performs a tail transfer. The indirect jump must end its registered span;
far jumps and unsupported memory address forms remain explicit errors.

The shared `direct_indirect_call_native.rs` now runs 1920 CALL/JMP state cases
across all 16 register operands/memory bases, pointer boundary faults, fuel and
prior faults. `direct_indirect_jump_native.rs` executes register and RIP-relative
pointer-table jumps, checks unchanged stack depth at the destination, rejects
missing registrations, and verifies that an indirect self-loop exhausts fuel.
Relative-call and conditional-branch native regressions also pass.

Validation: process 51652 exited 0 after 687 library tests, three integration
tests, strict Clippy, release build, and all four strict-warning native verifier
builds/runs. Compile the two standalone verifiers with the release rlib and
dependency flags documented above; neither takes arguments. Automatic direct
translation, broader instruction coverage and production readiness remain
unfinished; this evidence does not establish PS5 game execution.

## Observed-target direct translation

`NativeExecutor::register_direct_source` retains an immutable ELF snapshot and
an `Rc<JitArena>`. With `set_automatic_translation(true)`, the existing suspended
translation-miss path can decode and lower a newly observed entry, call, return
or branch target directly. It retries the saved dispatch phase after publication,
without replaying guest instructions or creating a dylib. Worker entry preparation
also selects this backend for direct-owned images; worker scheduling itself is
not covered by the new native verifier.

Discovery scans one straight-line span through its first control-flow boundary.
Each decoded instruction PC shares the emitted function's dispatcher and pinned
allocation. New overlapping instructions are rejected, including instruction-interior
targets. The subsequent join change below allows newly discovered predecessors
to hand off at an existing instruction boundary. Live
executable bytes must match the immutable source before publication and execution.
Unsupported instructions fail explicitly with no dylib fallback.

The initial limits are the caller's cumulative instruction budget, 16 translation
attempts per source (including failures), shared immutable-source byte/count
limits, and the arena's append-only capacity. Arena accounting remains separate
from the dylib cache; these limits are not a whole-process 16 GB memory proof.
Retirement removes the source and instruction entries; arena bytes are not reused.
Default backend selection remains unchanged.

`direct_automatic_native.rs` verifies 20 executions returning 42 through newly
discovered entry/callee/return/tail blocks, entry at an existing internal boundary,
overlap rejection, instruction and attempt budgets, stale-byte rejection, and
unsupported instructions without arena allocation. Dylib library usage is zero.
The first overlap test exhausted its deliberately tight seven-instruction budget;
giving that test more budget isolated overlap rejection, while a separate
one-instruction case verifies exhaustion. The corrected strict-warning verifier
passes (process 67040, exit 0).

687 library tests, three integration tests, strict Clippy and release build passed
before that first verifier assertion (process 57679). Existing indirect-call,
indirect-jump, cross-block and register-MOV native regressions pass (process 72649,
exit 0). Reproduce the new verifier with the release rlib/dependency flags above;
it takes no arguments. This is initial opt-in automatic direct execution, not
production readiness or PS5 game compatibility.

## Joining existing direct spans

Observed-target discovery now stops before an already registered instruction PC.
The direct emitter lowers the preceding nonterminal span with an explicit
dispatcher fallthrough to that PC. The handoff adds no synthetic guest instruction
and consumes no extra instruction fuel. Cached code and backing bytes remain
pinned; instruction-interior or crossing overlaps still fail. Manual block
registration retains its terminal-instruction requirement.

`direct_join_native.rs` first translates a suffix, then discovers its predecessor
at runtime. Two suffix boundaries and fuel limits 2/3/100 verify exact guest
instruction accounting: a three-instruction program fits the source limit of
three and returns 42 with fuel three, but stops with fuel two. Forty additional
cached runs allocate no new arena bytes. Mutating an existing suffix rejects
predecessor publication before any arena allocation.

Process 18007 exited 0 after 687 library tests, three integration tests, strict
Clippy, release build, the new strict-warning native verifier and automatic,
indirect-call/jump and conditional-branch regressions. Reproduce the standalone
verifier with the release rlib/dependency flags above; it takes no arguments.
The direct backend remains opt-in with limited instruction coverage and a
16-attempt source limit. PS5 game compatibility remains unverified.

## Direct register stack operations and ELF probe

Direct `PUSH r64` and `POP r64` now use the guarded memory helpers. A fault leaves
the registers and flags unchanged. Successful PUSH commits RSP after storing
the original operand value; POP commits the increment before assigning the
destination, so PUSH RSP stores the old pointer and POP RSP uses the loaded
value. Immediate/memory PUSH, memory POP and 16-bit stack forms remain unsupported.

`direct_stack_native.rs` passes 1344 native cases across all registers, stack
boundaries, unaligned addresses, writable/read-only mappings and fuel limits.
It checks memory contents and register/flag preservation on faults. A complete
automatically discovered register-save/frame/restore function returns 42.
687 library tests, three integration tests, strict Clippy, release build and
the strict-warning verifier pass (process 98054, exit 0). Automatic translation,
span-join and indirect-call/jump native regressions also pass (83077, exit 0).

`direct_elf_probe.rs` takes an ELF path and reports bounded raw entry execution
through the direct backend. It does not bind imports, load dependencies or set
up the guest process. The existing crispy-doom ELF (SHA256
`308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567`)
has entry 0x40. Before this change its entry span failed translation on
`Push_r64`; afterward it fails on `Lea_r64_m` at 0x51. In both cases zero JIT
bytes were published and the entry never executed. This is an instruction-gap
probe, not evidence of homebrew startup or gameplay. The probe's initial build
was corrected to print decoded instruction codes because the configured iced-x86
Instruction type does not implement Display.

Compile the stack verifier using the release rlib/dependency flags above. The
ELF probe additionally needs `--extern iced_x86=<release deps/libiced_x86-*.rlib>`
and takes the ELF pathname as its sole argument. Its STOP output must be read;
a successful probe process exit does not mean guest execution succeeded.

## Direct LEA address calculation

Direct `LEA r32,m` and `LEA r64,m` now reuse effective-address calculation without
calling a memory helper. The 32-bit destination truncates and zero-extends the
result; register aliases are read before destination commit. FS/GS prefixes are
ignored for LEA only, as no segment base participates in the calculation. Actual
loads/stores retain their explicit unsupported FS/GS boundary. 16-bit destinations
and 32-bit address-size forms remain unsupported.

`direct_lea_native.rs` passes 92268 native cases with null memory context:
destination/base/index combinations (excluding the SIB no-index encoding), four
scales, wraparound values, 32/64-bit results, RIP-relative bias, segment prefixes,
fuel and prior faults. This is selected form coverage, not the complete x86 ISA.
687 library tests, three integration tests, strict Clippy, release build and the
strict-warning LEA verifier pass (process 76006, exit 0). Guarded MOV, stack and
automatic translation regressions also pass (process 7321, exit 0).

The same crispy-doom raw entry probe now rejects `Cmp_r64_rm64` at 0x58, a memory
operand form. The entry span still publishes zero JIT bytes and never executes;
accepting preceding instructions during translation is not homebrew startup.
Reproduce the LEA verifier with the release rlib/dependency flags above and no
arguments; use the preceding ELF probe instructions for the real-file boundary.

## Direct arithmetic with memory sources

Direct ADD/SUB/CMP/AND/OR/XOR now accept 32/64-bit register destinations with
memory source operands. Address calculation and a guarded load precede any
destination or flag commit. The register operand is reloaded after the helper
call, preserving base/destination alias behavior and avoiding caller-saved host
register assumptions. Faults preserve all guest registers and flags. The checked
emission allowance grows to 96 words per instruction to accommodate both memory
and flag helper sequences; arena capacity still bounds actual publication.
Memory destinations and byte/word arithmetic remain unsupported here.

`direct_arithmetic_memory_native.rs` passes 196608 native cases with an independent
widened arithmetic/flag oracle: six operations, 32/64-bit widths, all destination
and base registers, selected overflow/carry values, aligned/unaligned pointers
and guarded failures. Read-only source memory is unchanged. The first verifier
format attempt required a space between `<` and unary `-`; after that correction,
687 library tests, three integration tests, strict Clippy, release build and the
strict-warning native verifier pass (process 74100, exit 0). Register arithmetic,
immediate arithmetic and automatic translation regressions pass (53077, exit 0).

The same raw crispy-doom ELF probe now passes initial entry translation and
execution, reaching a miss at image-relative 0x3a0 on `Mov_rm64_imm32`.
It reports 2040 published JIT bytes and zero dylibs. This probe provides no
imports, dependencies or process setup; the result proves bounded direct entry
progress only, not initialized homebrew or gameplay. Reproduce the new verifier
with the release rlib/dependency flags above and no arguments; the existing
ELF probe reproduces the real-file boundary.

## Direct immediate MOV stores

Direct C7 `MOV r/m32,imm32` and `MOV r/m64,imm32` now support register and memory
destinations. The 64-bit form sign-extends the immediate; the 32-bit register
form zero-extends its result. Memory destinations use guarded stores, stop on
faults and preserve registers/flags. Byte/word stores remain unsupported here.

`direct_mov_constant_native.rs` passes 23042 native cases: all register/base
operands, signed boundary constants, alignment and mapping boundaries, read-only
destinations, fuel and prior faults. RIP-relative stores explicitly check that
the effective address uses the end of the whole instruction, including its
immediate. 687 library tests, three integration tests, strict Clippy, release
build and the initial 23040-case verifier pass (40506, exit 0). The final verifier
with RIP cases, memory-MOV and automatic translation regressions pass (99078,
exit 0). Use the release rlib/dependency flags above to reproduce; no arguments.

The same raw homebrew probe now publishes 3212 JIT bytes, loads zero dylibs and
stops with guest read fault 1 at 0x3c1. Decoding 0x3a0 onward shows `CALL [RBX]`
there; the function assigned RBX from incoming RDI at 0x3a6. The raw probe passes
default arguments and supplies no startup callback table, imports or process
setup. This fault is therefore a runtime-setup boundary to investigate with the
configured startup path, not evidence that another instruction must be stubbed.
Homebrew initialization and PS5 gameplay remain unverified on this backend.

## Configured direct startup and multi-byte NOPs

`entry_native.rs --direct-startup` now selects an immutable direct source for
the root ELF and enables observed-target translation in the existing configured
startup harness. It preserves process arguments, callback table, kernel mappings,
service exports and startup assertions. This mode skips component/Metal checks,
as `--startup-only` does. The normal dylib backend remains the default. The pinned
ELF check now uses `Image::source_sha256` before either backend is selected;
the digest covers retained file bytes rather than relocated/live segments.

The first configured run stopped translating the span at 0x61 on `Nop_rm32`,
after passing the raw probe's missing-callback boundary. Direct NOP r/m16/32/64
now consumes fuel without reading its encoded register/memory operand. The
strict-warning `direct_nop_native.rs` verifier passes 120 cases with null memory,
invalid-looking addresses, segment/address/operand prefixes, registers, fuel and
prior faults. All registers and flags are preserved.

687 library tests, three integration tests, strict Clippy, release build and the
NOP verifier pass. The full configured startup harness compiles with strict
warnings, then exits 1 on the next explicit unsupported instruction:
`Translation miss at 0x10000061 (bias 0x10000000): Direct lowering unsupported: Mov_rm8_imm8`.
This is process 38123; the startup log is `/tmp/macps5-direct-startup2.log`.
No successful startup or Metal/gameplay result is claimed from this run.

After a release build, compile `rust/verification/entry_native.rs` using the
release rlib/dependency flags above. Run the resulting executable with:

```sh
NATIVE_PS5_RUNTIME_ARCHIVE="$PWD/rust/target/release/libnative_ps5.a" \
NATIVE_PS5_AOT_DIRECTORY=/tmp/macps5-direct-startup-artifacts \
  /tmp/macps5-direct-startup /path/to/crispy-doom.elf --direct-startup
```

The archive/directory remain required by the shared harness and potential module
loading paths. The root direct source has no unsupported-instruction fallback to
the dylib compiler. The next CPU boundary is byte-immediate MOV, rather than a
fabricated callback or successful stub for the raw probe's missing environment.

## Direct byte-immediate MOV

Direct B0 and C6 immediate MOV forms now write byte registers, and C6 memory
destinations use the guarded one-byte store helper. Register lowering distinguishes
legacy AH/CH/DH/BH from REX-selected SPL/BPL/SIL/DIL and R8L–R15L. It writes exactly
the selected byte in the trusted CPU context without zero-extending the containing
register. Flags and neighboring register/memory bytes remain unchanged.

`direct_byte_constant_native.rs` passes 2016 native cases: both register encodings,
legacy/REX byte-register selection, selected immediate values, fuel/prior faults,
all memory bases, unaligned/end-of-mapping stores and unmapped/read-only faults.
687 library tests, three integration tests, strict Clippy, release build and the
strict-warning verifier pass. The configured startup harness also compiles, then
exits 1 at the next explicit unsupported instruction (process 13958):
`Translation miss at 0x10000061 (bias 0x10000000): Direct lowering unsupported: Inc_rm64`.
The log is `/tmp/macps5-byte-startup.log`; accepting the byte store does not imply
the rest of that span executed. Existing C7 immediate MOV and register-MOV native
regressions pass (process 84435, exit 0).

Reproduce the standalone verifier with the release rlib/dependency flags above
and no arguments. The preceding `--direct-startup` command reproduces the real
startup boundary. Byte register-to-register/load/store MOV forms, word forms and
INC remain unfinished in the direct backend; no game compatibility is claimed.

## Direct register INC/DEC

Direct INC/DEC32/64 now supports register operands, using flag-helper operations
3/5 to preserve CF while computing OF/SF/ZF/AF/PF. A 32-bit result zero-extends
the containing register. Memory operands and byte/word forms remain explicitly
unsupported. Inspection of the configured startup gap identifies `INC RCX` at
0x74 in the BSS-clearing loop.

`direct_incdec_native.rs` passes 12288 native cases: both operations and widths,
all registers, selected wraparound/nibble/sign boundaries, both carry values,
fuel and prior faults. Its result/flag oracle uses widened signed arithmetic
and nibble comparisons rather than the runtime helper. 687 library tests, three
integration tests, strict Clippy, release build and the strict-warning verifier
pass. Register/immediate arithmetic native regressions also pass (75159, exit 0).

The configured startup harness now prints:
`Real entry reached dlsym: handle=0x1 symbol="sceKernelDlsym" output=0x201fffa0`.
It subsequently exits 1 on
`Translation miss at 0x100003d9 (bias 0x10000000): Direct lowering unsupported: Cmp_rm64_r64`.
This is process 82183, with `/tmp/macps5-inc-startup.log` as its log. Reaching
the first real symbol lookup is verified progress; complete bootstrap, dependency
initialization, game main and gameplay have not succeeded on this backend.
Use the preceding standalone verifier build flags and configured startup command
to reproduce these boundaries.

## Direct memory-first CMP and TEST

Direct `CMP r/m32,r32`, `CMP r/m64,r64` and the corresponding TEST forms now
accept a memory first operand. A guarded read supplies the left operand; the
register operand is captured afterward. CMP preserves subtraction order and
TEST computes the conjunction; neither writes a destination. A failed read
preserves flags and all registers. Immediate second operands for these memory
forms remain unsupported.

`direct_memory_compare_native.rs` passes 81920 native cases with independent
flag calculations: both operations/widths, all register/base combinations,
selected signed/unsigned edge values, read-only/unaligned sources and boundary
faults. 687 library tests, three integration tests, strict Clippy, release build
and the strict-warning verifier pass. Memory-source arithmetic, register
arithmetic and INC/DEC regressions pass (process 79939, exit 0).

Configured startup passes the previous CMP boundary and reaches a translation
miss at image-relative 0x20e9, where the current per-source 16-attempt limit
rejects further compilation. Process 48307 exits 1 at that explicit budget
boundary; `/tmp/macps5-memcmp-startup.log` retains the first real dlsym lookup
followed by `Direct translation attempt budget exhausted`. This requires a
separate bounded direct-translation budget, not pretending startup completed
or disabling resource limits. Full startup and gameplay remain unverified.
Reproduce with the preceding standalone verifier flags and configured startup
command.

## Separate direct translation budgets

`DirectTranslationLimits` separates per-image direct compilation from the dylib
rebuild budget. Its defaults are 65536 instructions and 4096 attempts; the existing
registration method retains its caller-supplied instruction limit and uses the
new default attempt limit. `register_direct_source_with_limits` accepts explicit
values, requiring positive limits no greater than 1048576 instructions and 65536
attempts. Limits cannot be changed on a live source. Dylib rebuilds retain their
16-attempt limit.

Failed translations consume attempts, successfully published spans consume the
instruction budget, and cached execution consumes neither. Source bytes/counts
and the append-only JIT arena remain independently bounded. The new read-only
`direct_translation_usage` reports per-image instructions/attempts; retirement
removes that source's accounting without reclaiming individual arena allocations.

`direct_budget_native.rs` runs 33 discovered spans (34 guest instructions) to
return 42. It checks exact budget boundaries, invalid zero/overlarge limits,
retirement and ten cached runs without allocation or usage growth. The existing
automatic verifier now supplies its explicit 16-attempt failure-test limit,
independently of the dylib constant. Both strict-warning native verifiers pass,
along with 687 library tests, three integration tests, strict Clippy and release
build (process 86577 before its final startup probe).

Configured startup now passes the former attempt limit at 0x20e9 and exits 1 at
`Translation miss at 0x1000215e (bias 0x10000000): Direct lowering unsupported: Movzx_r32_rm16`.
The final process status is therefore a known incomplete-startup result, not a
passing startup check; log: `/tmp/macps5-budget-startup.log`. The next CPU gap is
zero extension from a 16-bit operand. Reproduce the budget verifier with the
release rlib/dependency flags above, and startup with `--direct-startup`.

## Direct MOVZX byte and word sources

Direct MOVZX now handles byte/word sources and 32/64-bit destinations. Register
sources use narrow CPU-context loads, including legacy AH/CH/DH/BH and REX low
bytes, before committing the full zero-extended destination. Memory sources use
guarded one/two-byte reads; faults preserve destination registers and flags.
Byte-register offset selection is shared with immediate MOV. Sixteen-bit
destinations and sign extension remain unsupported in this direct path.

`direct_movzx_native.rs` passes 24608 native cases covering destination/source
and address-base aliases, selected upper-bit/sign-bit patterns, read-only and
unaligned memory, boundary failures and legacy high bytes. The byte-immediate
MOV regression also passes. 687 library tests and three integration tests pass
(22099 before Clippy); strict Clippy initially requested explicit parentheses
around an encoding shift. After that readability correction, Clippy, release
build and both strict-warning native verifiers pass (96277 before startup).

Configured startup still reaches its first real dlsym lookup. It now stops at
`Translation miss at 0x1000215e (bias 0x10000000): Direct lowering unsupported: Shl_rm32_imm8`.
The span has moved past MOVZX during translation but has not executed as a whole.
The startup process exits 1; `/tmp/macps5-movzx-startup.log` records the result.
Use the preceding standalone build flags for this verifier and the configured
startup command to reproduce. Bootstrap and gameplay remain incomplete.

## Direct immediate register shifts

Direct immediate/implicit-one SHL, SHR and SAR now support 32/64-bit register
operands. Counts are masked to five or six bits before ARM64 variable-shift
execution; the existing shift flag helper preserves zero-count flags and the
runtime's undefined-AF/multi-bit-OF policy. Results at 32 bits zero-extend their
containing register. CL-count, memory and byte/word shift forms remain unsupported
in this direct path.

`direct_shift_native.rs` passes 294918 native checks with a single-bit iterative
result/flag oracle: all 256 immediate counts, three operations, both widths,
all registers, selected signed/unsigned edge values, incoming flag patterns,
implicit-one encoding and fuel/prior faults. 687 library tests, three integration
tests, strict Clippy, release build and the strict-warning native verifier pass.

The configured startup probe then exits 1 at its next explicit unsupported form:
`Translation miss at 0x10002ca0 (bias 0x10000000): Direct lowering unsupported: Setne_rm8`.
This is process 67206; `/tmp/macps5-shift-startup.log` records the result. The
first real dlsym boundary is still reached, but complete bootstrap/gameplay is
not established. Reproduce the standalone verifier using the release rlib flags
above and no arguments; rerun `--direct-startup` for the real-file boundary.

## Direct SETcc conditions

The direct backend now handles all 16 SETcc encodings using the existing guest
condition helper. Register destinations write only the selected low/high byte.
Memory destinations use guarded one-byte stores and preserve registers/flags on
failure. Effective-address lowering preserves the computed condition result until
the store; SETcc never substitutes host condition flags for guest flags.

`direct_setcc_native.rs` passes 139264 native truth-table cases: every condition
and relevant flag combination, legacy/REX byte registers, all address bases,
mapping boundaries, read-only/unmapped writes, fuel and prior faults. The initial
build revealed that this iced-x86 version has no `Instruction::is_setcc`; an
explicit match on its 16 SETcc codes corrected that API assumption. Afterward,
687 library tests, three integration tests, strict Clippy, release build and the
strict-warning native verifier pass (process 12273 before startup). The existing
5120-case direct branch verifier also passes (command chunk 4d8a6f, exit 0).

Configured startup now rejects `And_rm8_r8` in the span at image-relative 0x2ca0.
The process exits 1 at that explicit boundary; `/tmp/macps5-setcc-startup.log`
records the first real dlsym lookup and subsequent translation error. SETNE is
accepted during translation, but the complete span has not executed. The next
CPU gap is byte logical arithmetic; bootstrap and gameplay remain incomplete.
Use the existing standalone verifier flags and configured startup command to
reproduce.

## Direct byte register arithmetic

Direct byte-register ADD/SUB/CMP/AND/OR/XOR/TEST now supports register or immediate
second operands. Narrow CPU-context loads capture both operands before any write;
one-byte stores preserve surrounding bits, including aliases within one physical
register. CMP/TEST never write a destination. The existing integer flag helper
receives width eight, preserving the runtime's chosen undefined-flag policy.
Memory byte arithmetic and word arithmetic remain unsupported in this path.

Inspection of the real startup span identifies `AND CL,AL` at 0x2cb9.
`direct_byte_arithmetic_native.rs` passes 143430 native cases with independent
widened/nibble flag calculations: seven operations, legacy/high-byte and REX
register combinations, aliasing, selected sign/carry/nibble boundaries and group/
accumulator immediate forms. 687 library tests, three integration tests, strict
Clippy, release build and the strict-warning native verifier pass (process 74372
before its final startup run).

Configured startup advances to a translation miss at image-relative 0x2d2f:
`Direct lowering unsupported: VEX_Vxorps_xmm_xmm_xmmm128`.
The final process exits 1; `/tmp/macps5-byte-arith-startup.log` records the first
real dlsym lookup and this later boundary. The next gap is vector logical lowering,
not complete bootstrap or gameplay. Reproduce the standalone verifier with the
release rlib/dependency flags above, and the real-file run with `--direct-startup`.

## Direct VEX vector XOR

The direct backend accepts exactly the VEX128/256 VXORPS and VXORPD forms.
Register operands lower to independent ARM64 64-bit EOR pieces; each piece reads
both inputs before writing its destination, preserving all source/destination
aliases. VEX128 clears the destination's modeled upper 128 bits. Memory operands
use the existing Rust guarded vector-logical helper, which validates and reads
the entire operand before committing any destination byte. Flags and MXCSR are
unchanged. Legacy SSE and EVEX XOR forms remain outside this direct support.

`rust/verification/direct_vector_xor_native.rs` passes 98308 native state checks:
all 16x16x16 register combinations for both instructions and widths, every GPR
memory base, displacement/SIB decoding, unaligned reads, exact end boundaries,
short reads and address wraparound, aliasing, upper clearing, prior faults and
fuel exhaustion. Expected XOR values use per-bit Boolean comparisons. Memory is
read-only and verified unchanged. FS/GS and 32-bit memory addressing are rejected.
The strict-warning standalone verifier, 687 library tests, three integration
tests, strict Clippy and release build pass. An initial compile caught an offset
type mismatch; using usize for vector context offsets fixed it before these runs.

Configured `--direct-startup` still reaches the first real dlsym lookup and then
exits 1 at the explicit translation boundary in the block at image-relative
0x2d2f: `Direct lowering unsupported: VEX_Vmovups_xmmm128_xmm`.
VXORPS is accepted during translation, but this whole block has not executed.
Bootstrap, GPU gameplay and PS5 game compatibility remain unproven.
Logs: `/tmp/macps5-vector-xor-{tests,clippy,build,startup}.log`.

Reproduce the independent native check after building the release library:

```sh
rustc --edition=2024 -O -Dwarnings rust/verification/direct_vector_xor_native.rs \
  --extern native_ps5=rust/target/release/libnative_ps5.rlib \
  -L dependency=rust/target/release/deps -o /tmp/macps5-direct-vector-xor
/tmp/macps5-direct-vector-xor
```

The configured startup command is unchanged; the next direct CPU gap is vector
move lowering with guarded reads/writes and VEX register semantics.

## Direct VEX packed vector moves

Direct lowering now explicitly accepts the 24 VEX128/256 load/store encodings
of VMOVUPS, VMOVUPD, VMOVAPS, VMOVAPD, VMOVDQA and VMOVDQU. Register copies use
ARM64 context loads/stores and support either encoding direction, including
self-copies. VEX128 clears the destination's modeled upper 128 bits. Guarded
memory helpers enforce full operand bounds, destination writability and, for
aligned forms, 16/32-byte alignment before committing. Stores preserve the
source vector including its upper half. RFLAGS and MXCSR remain unchanged.
Legacy SSE, EVEX and scalar vector move forms are not added by this change.

`rust/verification/direct_vector_move_native.rs` passes 92160 native state
checks: every vector register pair and GPR memory base, both widths and encoding
directions, aliases, unaligned addresses, aligned-form failures, exact mapping
ends, short operands, address overflow, and read-only versus writable mappings.
The verifier compares the entire vector register bank, GPR bank, mapped bytes,
flags, MXCSR and fault/fuel/stop state. FS/GS and address-size overrides fail
translation. The strict-warning verifier, 687 library tests, three integration
tests, strict Clippy and release build pass. Reproduce using the standalone
release rlib/dependency flags above with `direct_vector_move_native.rs`.

The configured `--direct-startup` run now proceeds beyond the previous block at
image-relative 0x2d2f and exits 1 at 0x31e8 with
`Direct lowering unsupported: Cmovl_r32_rm32`. It still records the first real
dlsym lookup. Logs are `/tmp/macps5-vector-move-{tests,clippy,build,startup}.log`.
The next observed CPU gap is conditional moves; complete bootstrap and PS5
gameplay remain unverified. This startup-only run skips Metal presentation.

## Direct conditional moves

The direct backend explicitly accepts all 48 legacy CMOVcc encodings (16
conditions at 16/32/64 bits) with register or guarded memory sources. Source
resolution happens before condition evaluation; a false condition does not
suppress a memory fault. A 16-byte aligned host stack slot preserves the resolved
source across the existing condition helper. ARM64 CSEL selects the value;
16-bit stores preserve surrounding bits, while 32-bit results zero the upper
half even when the condition is false. Guest RFLAGS are unchanged. Fault paths
exit before the temporary host stack slot is allocated or the destination changes.

These rules follow the CMOVcc operation in
[Intel SDM volume 2A, page 3-159](https://cdrdv2-public.intel.com/868140/253666-089-sdm-vol-2a.pdf#page=277).
The implementation rejects other encodings rather than treating newer APX
conditionally faulting instructions as ordinary CMOVcc.

`rust/verification/direct_cmov_native.rs` passes 2359300 native state checks.
It uses independent fixed truth tables for all 32 CF/PF/ZF/SF/OF combinations,
all GPR source/destination pairs at all three widths, aliases and memory-address
aliases, unaligned reads, exact-end/short/unmapped operands and wrapping
addresses. Expected state includes false-condition memory faults and CMOV32
zero-extension. Prior faults and exhausted fuel preserve destination state;
FS/GS and 32-bit addressing are rejected. The strict-warning verifier, 687
library tests, three integration tests, strict Clippy and release build pass.
An initial verifier parsing error in cast/shift expressions was corrected before
the native run. Reproduce with the standalone release rlib/dependency flags
above, substituting `direct_cmov_native.rs`.

Configured `--direct-startup` still reaches the first real dlsym lookup and exits
1 at image-relative 0x31e8, now with
`Direct lowering unsupported: Movsxd_r64_rm32`. CMOVL is accepted during
translation, but this entire block has not executed. Logs are
`/tmp/macps5-cmov-{tests,clippy,build,startup}.log`. Sign-extending move lowering is
the next observed CPU gap. Complete bootstrap and PS5 gameplay remain unproven;
this startup-only run skips Metal presentation.

## Direct sign-extending moves

The direct backend now accepts MOVSX from byte sources to 16/32/64-bit
registers, MOVSX from word sources to 32/64-bit registers, and MOVSXD from dword
sources to 64-bit registers. Operand loading is shared with the existing MOVZX
path. Memory reads are guarded before destination writes; register reads handle
legacy high bytes and REX byte registers. ARM64 SBFM sign-extends to the requested
width. A 16-bit destination write preserves surrounding bits, while a 32-bit write
clears its upper half. Flags remain unchanged. Non-widening MOVSXD forms are not
added by this change.

The standalone `rust/verification/direct_movsx_native.rs` exercises every GPR
source/destination pair for the six forms, sign boundaries from each source
width, base/destination aliases, unaligned and exact-end memory operands,
short reads and address overflow. An independent widened signed-arithmetic
reference computes expected destination bits. All 256 legacy high-byte values
are checked with both 16-bit and 32-bit destinations. The existing MOVZX native
verifier is also rerun because its load path is shared.

Validation passes: 102400 signed-move native cases, 24608 MOVZX regression cases,
687 library tests, three integration tests, strict Clippy and release build.
Both standalone verifiers compile with warnings denied. Reproduce using the
release rlib/dependency flags above with `direct_movsx_native.rs` and
`direct_movzx_native.rs`.

Configured `--direct-startup` proceeds beyond the previous block at 0x31e8 and
exits 1 at the new observed block at image-relative 0x880:
`Direct lowering unsupported: Cmp_rm8_imm8`. The first real dlsym lookup remains
recorded. Logs are `/tmp/macps5-movsx-{tests,clippy,build,startup}.log`.
The next CPU gap is byte-memory comparison; bootstrap and PS5 gameplay are still
incomplete. The startup-only harness skips Metal presentation.

## Direct byte memory comparisons

Direct byte arithmetic selection now accepts memory CMP with an immediate or
byte-register operand, CMP with a byte-register first and memory second, and
memory TEST with an immediate or byte-register operand. The emitted sequence
resolves and guards the one-byte memory read before loading the other operand
and calculating flags. Explicit operand ordering preserves subtraction direction;
legacy high bytes use the existing byte-offset mapping. Neither instruction
writes GPRs or guest memory. A failed read exits before flag calculation.
Byte memory arithmetic that modifies its destination is still unsupported.

`rust/verification/direct_byte_compare_native.rs` uses independent signed and
nibble arithmetic for expected flags. It exercises every memory-byte value,
selected carry/sign/nibble-boundary operands, both register encodings, all GPR
bases and byte registers, source/base aliases, immediate forms, read-only
mappings, the final mapped byte and the first unmapped byte. Expected state
includes all GPRs, flags, memory and fault/fuel/stop. The existing byte-register
arithmetic verifier is rerun because its emission path is shared.

Validation passes: 6193152 byte-memory comparison native cases, 143430 existing
byte-register arithmetic cases, 687 library tests, three integration tests,
strict Clippy and release build. Both standalone verifiers compile with warnings
denied. Reproduce with the release rlib/dependency flags above using
`direct_byte_compare_native.rs` and `direct_byte_arithmetic_native.rs`.

Configured `--direct-startup` advances beyond the block at image-relative 0x880
and exits 1 at 0x8e0 with `Direct lowering unsupported: Add_rm32_r32`.
Logs are `/tmp/macps5-byte-compare-{tests,clippy,build,startup}.log` and still record
the first real dlsym lookup. The next observed gap is memory-destination integer
arithmetic, requiring guarded read-modify-write semantics. Bootstrap and PS5
gameplay remain incomplete; the startup-only run skips Metal presentation.

## Direct memory destination arithmetic

The direct backend now accepts unlocked legacy ADD/SUB/AND/OR/XOR memory
destinations at 8/16/32/64 bits, with register or encoded immediate sources.
The new Rust helper `native_ps5_module_integer_rmw_v1` uses the existing guarded
Memory load/store operations. It calculates flags without changing CPU state,
then commits them only after the store succeeds. Read failures produce fault 1;
write failures produce fault 2, leaving flags and bytes unchanged. Invalid helper
parameters produce fault 3. Prior faults return without work. The exclusive
Memory contract still applies: this helper is not a host atomic operation, and
LOCK-prefixed instructions are explicitly rejected by direct lowering.

`rust/verification/direct_memory_arithmetic_native.rs` passes 407205 checks,
including native translated cases for all five operations and four widths, every
GPR source/address pairing, narrow and full immediate encodings, sign extension,
unaligned/end-of-mapping accesses, read-only versus writable mappings, legacy
high-byte sources and source/base aliases. An independent widened arithmetic
reference checks flags and the complete modified mapping. Ancillary helper cases
check null CPU, missing Memory, invalid widths/operations and prior faults. Every
emitted instruction fixture is also retried with LOCK and must fail translation.
The verifier compiles with warnings denied; 687 library tests, three integration
tests, strict Clippy and release build pass. An initial verifier parse error
from adjacent comparison/minus tokens was fixed before its native run.
Reproduce with the release rlib/dependency flags above using
`direct_memory_arithmetic_native.rs`.

The first configured startup run passes the memory ADD blocker at 0x8e0 and
reaches unsupported `Nopw` at 0x969. The direct backend now also accepts this
operand-size-prefixed 0x90 no-op. The existing NOP verifier includes 0x90 with
its prefix combinations in addition to 0F 1F forms, retaining prior-fault,
fuel, null-memory and state-preservation checks. Initial startup evidence is
`/tmp/macps5-memory-arith-startup.log`; the post-NOP run uses
`/tmp/macps5-memory-arith-nop-startup.log`.

After the NOP addition, all 150 native NOP cases pass, as do the rebuilt 687
library tests, three integration tests, strict Clippy and release build.

The post-NOP configured run exits 1 in the same span at image-relative 0x969,
now with `Direct lowering unsupported: Mov_rm8_r8`. NOPw is accepted during
translation, but that entire span has not executed. The next gap is byte MOV
between registers and memory. Bootstrap and PS5 gameplay remain incomplete;
these startup-only runs skip Metal presentation.

## Direct byte and word register/memory moves

Direct lowering accepts both encoding directions of byte and word MOV between
registers and between registers and memory. Narrow ARM64 context loads/stores
preserve surrounding destination bits, including legacy AH/CH/DH/BH aliases.
Guest addresses are calculated before a guarded memory helper runs, and register
loads commit only after fault checking. Stores use the existing complete-range
write validation and preserve CPU flags and registers on failure. Immediate-word
and moffs encodings are not added by this change.

`rust/verification/direct_narrow_mov_native.rs` checks both widths and directions,
legacy and REX register combinations, source/destination aliases, address-base
aliases, high-byte sources and destinations, read-only/writable mappings,
unaligned/final-byte/short/unmapped reads and writes, and address overflow.
The oracle compares all GPRs, the entire mapping, flags and fault/fuel/stop.
FS/GS and 32-bit memory addressing are explicitly rejected. Reproduce using the
standalone release rlib/dependency flags above with `direct_narrow_mov_native.rs`.

Validation passes: 84480 native narrow-MOV cases, 687 library tests, three
integration tests, strict Clippy and release build. The standalone verifier
compiles with warnings denied.

Configured `--direct-startup` proceeds past the block at image-relative 0x969 and
exits 1 at 0x99d: `Direct lowering unsupported: VEX_Bextr_r32_rm32_r32`.
Logs are `/tmp/macps5-narrow-mov-{tests,clippy,build,startup}.log` and still record
the first real dlsym lookup. The next CPU gap is BMI bit-field extraction.
Bootstrap and PS5 gameplay remain incomplete; this startup-only run skips Metal
presentation.

## Direct BEXTR bit-field extraction

Direct lowering now accepts the VEX32/64 BEXTR forms with register or guarded
memory data sources and a register control. The implementation emits ARM64
UBFX, conditional selects, shifts and masks: start and length come from the low
two control bytes, length is clipped to the remaining source bits, and empty or
out-of-range fields yield zero. Both inputs are captured before the destination
write. Results zero-extend for BEXTR32. CF/OF clear, ZF reflects the result, and
the runtime retains undefined SF/AF/PF as the existing AOT implementation does.
A memory source is read even for a zero-length field; read failure preserves the
destination and flags. XOP/immediate BEXTR forms remain unsupported.

All 17 raw instruction encodings used in the extraction/flag sequence were
compared with Apple clang's ARM64 assembler and otool section bytes
(`/tmp/macps5-bextr-encoding.s` and `.o`). The independent native verifier
`rust/verification/direct_bextr_native.rs` uses a per-bit extraction oracle,
exercises all 65536 start/length controls at both widths with six input patterns,
and checks all GPR destination/source/control combinations, aliases, ignored
high control bits, unaligned and short memory operands, and address overflow.
It compares the complete GPR bank, flags, mapped bytes and fault/fuel/stop.
Reproduce with the standalone release rlib/dependency flags above using
`direct_bextr_native.rs`.

Validation passes: 1277952 native BEXTR cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied. Configured `--direct-startup` passes the former BEXTR block at
0x99d and exits 1 at image-relative 0xa2a with
`Direct lowering unsupported: Not_rm8`. Logs are
`/tmp/macps5-bextr-{tests,clippy,build,startup}.log` and record the first real dlsym
lookup. The next gap is byte NOT; bootstrap and PS5 gameplay remain incomplete.
The startup-only harness skips Metal presentation.

## Direct bitwise NOT

Direct lowering now accepts unlocked NOT on byte/word/dword/qword registers or
memory. Register forms emit ARM64 MVN with narrow writes for 8/16-bit operands
and zero-extension for 32-bit destinations. Legacy high-byte registers retain
their correct position. Memory forms use operation 5 of the guarded integer RMW
helper; its RHS is ignored and it preserves flags on successful writes as well
as faults. Read and write failures preserve memory and flags. LOCK forms remain
explicitly rejected because this helper is not a host atomic operation.

`rust/verification/direct_not_native.rs` uses subtraction from an all-ones mask
as its independent complement oracle. It checks every register/base encoding,
legacy high bytes, all four widths, unaligned/final-byte/short/unmapped operands,
read-only and writable mappings, source values with varied high bits, prior
faults and zero/one/two instruction budgets. It compares all GPRs, mapped bytes,
flags, RIP and fault/fuel/stop state. The existing memory-arithmetic verifier is
also rerun for the shared helper; its invalid-operation case now uses 6 because
5 is the newly supported NOT operation.

Validation passes: 27648 native NOT cases, 407205 memory-arithmetic regression
checks, 687 library tests, three integration tests, strict Clippy and release
build. Both standalone verifiers compile with warnings denied. Reproduce with
the release rlib/dependency flags above using `direct_not_native.rs` and
`direct_memory_arithmetic_native.rs`.

Configured `--direct-startup` still reaches the first real dlsym lookup, then
exits 1 in the span at image-relative 0xa2a with
`Direct lowering unsupported: VEX_Shrx_r32_rm32_r32`. NOT is accepted during
translation, but the entire span has not executed. Logs are
`/tmp/macps5-not-{tests,clippy,build,startup}.log`. The next gap is BMI variable
shift lowering. Bootstrap and PS5 gameplay remain incomplete; this startup-only
run skips Metal presentation.

## Direct BMI variable shifts

The direct backend now accepts VEX SHLX/SHRX/SARX at 32 and 64 bits with register
or guarded memory data sources and a register count. Data/count resolution is
shared with BEXTR, capturing both inputs before any destination write. ARM64
LSLV/LSRV/ASRV use the same five/six-bit count masks; W-register results clear
the upper half of the destination. All guest flags remain unchanged, including
for zero counts. Memory is still read for a zero count and faults preserve the
destination and flags. Other variable-shift instruction families are unchanged.

`rust/verification/direct_bmi_shift_native.rs` uses repeated single-bit arithmetic
steps as its independent reference. It exercises all GPR destination/source/count
triples, aliases including address bases, masked and high count bits, counts
0..255 with six input patterns, both widths, all three operations, unaligned and
short memory reads, and address overflow. Expected state includes the full GPR
bank, flags, mapped bytes and fault/fuel/stop. The existing BEXTR verifier is
rerun because its operand-resolution path is shared. Reproduce with the release
rlib/dependency flags above using `direct_bmi_shift_native.rs` and
`direct_bextr_native.rs`.

Validation passes: 1975296 native BMI-shift cases, 1277952 BEXTR regression cases,
687 library tests, three integration tests, strict Clippy and release build.
Both standalone verifiers compile with warnings denied. Configured
`--direct-startup` passes the former block at 0xa2a and exits 1 at image-relative
0xc90 with `Direct lowering unsupported: Movbe_m32_r32`. Logs are
`/tmp/macps5-bmi-shift-{tests,clippy,build,startup}.log` and record the first real
dlsym lookup. The next CPU gap is MOVBE byte-swapping memory moves. Bootstrap
and PS5 gameplay remain incomplete; this startup-only run skips Metal presentation.

## Direct MOVBE byte-swapping moves

Direct lowering now accepts the six MOVBE memory/register forms at 16/32/64
bits. ARM64 REV16/REV reverses the bytes before a guarded store or after a
successful guarded load. Word destinations preserve their surrounding bits;
dword destinations zero-extend. Addresses and source registers are captured
before any destination write, including base/destination aliases. All guest
flags remain unchanged. FS/GS, 32-bit addressing and LOCK prefixes are rejected.

`rust/verification/direct_movbe_native.rs` checks all GPR/base combinations,
all three widths and both directions, varied source patterns, unaligned and
exact-end operands, short/unmapped spans, address overflow, read-only/writable
mappings, prior faults and zero/one/two instruction budgets. Its oracle treats
memory as big-endian bytes, independently of the emitted REV instructions, and
compares every GPR, the full mapping, flags, RIP and fault/fuel/stop state.
Reproduce with the standalone release rlib/dependency flags above using
`direct_movbe_native.rs`.

Validation passes: 442368 native MOVBE cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied. Configured `--direct-startup` passes the former block at 0xc90
and exits 1 at image-relative 0xcd2 with
`Direct lowering unsupported: VEX_Andn_r32_r32_rm32`. Logs are
`/tmp/macps5-movbe-{tests,clippy,build,startup}.log` and record the first real dlsym
lookup. The next gap is BMI AND-NOT. Bootstrap and PS5 gameplay remain incomplete;
this startup-only run skips Metal presentation.

## Direct BMI AND-NOT

Direct lowering now accepts VEX ANDN32/64 with register or guarded memory second
sources. The shared BMI source-loading path accounts for ANDN's operand order:
ARM64 BIC receives the second x86 source as its data and the first as the bits
to clear. Both inputs are captured before destination writes. Dword results
zero-extend. CF/OF clear, SF/ZF derive from the result, and undefined AF/PF retain
the runtime's prior values, matching the existing AOT policy. Memory operands
are still validated when the first source guarantees a zero result; a failed
read leaves the destination and flags unchanged.

`rust/verification/direct_andn_native.rs` uses independent per-bit Boolean
comparisons and expected sign/zero flags. It covers every GPR destination/first/
second combination, aliases including address bases, both widths, varied and
all-ones operands, unaligned/short/unmapped reads and address overflow. A second
sweep covers all 65536 low-word first operands with six second-operand patterns.
The BEXTR and BMI-shift native verifiers are rerun because operand resolution is
shared. Reproduce with the standalone release rlib/dependency flags above using
`direct_andn_native.rs`, `direct_bextr_native.rs`, and `direct_bmi_shift_native.rs`.

Validation passes: 1359872 native ANDN cases (rerun with all arithmetic flags
initially set), 1277952 BEXTR cases, 1975296 BMI-shift cases, 687 library tests,
three integration tests, strict Clippy and release build. Standalone verifiers
compile with warnings denied. Configured `--direct-startup` exits 1 in the same
span at image-relative 0xcd2, now with
`Direct lowering unsupported: VEX_Rorx_r32_rm32_imm8`. ANDN is accepted during
translation, but this entire span has not executed. Logs are
`/tmp/macps5-andn-{tests,clippy,build,startup}.log` and retain the first real dlsym
lookup. RORX is the next CPU gap. Bootstrap and PS5 gameplay remain incomplete;
this startup-only run skips Metal presentation.

## Direct RORX immediate rotation

Direct lowering now accepts VEX RORX32/64 from a register or guarded memory
source. The immediate count is masked to five/six bits and emitted as ARM64
ROR (EXTR alias). Dword results zero-extend, all guest flags remain unchanged,
and aliases are resolved before writing the destination. Memory sources are
read even for a masked-zero count; faults preserve destination and flags.

`rust/verification/direct_rorx_native.rs` uses repeated single-bit rotation
(arithmetic division and reinsertion of the low bit) as its independent oracle.
It checks all 256 immediate counts with six input patterns at both widths,
count boundaries across all GPR source/destination pairs, aliases including
address bases, high source bits, unaligned/short/unmapped reads, and address
overflow. All arithmetic flags start set so preservation is checked explicitly.
Expected state includes every GPR, flags, mapped bytes and fault/fuel/stop.
Reproduce with the standalone release rlib/dependency flags above using
`direct_rorx_native.rs`.

Validation passes: 137760 native RORX cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied. Configured `--direct-startup` passes the former block at 0xcd2
and exits 1 at image-relative 0x38ed with
`Direct lowering unsupported: VEX_Mulx_r64_r64_rm64`. Logs are
`/tmp/macps5-rorx-{tests,clippy,build,startup}.log` and retain the first real dlsym
lookup. MULX is the next CPU gap. Bootstrap and PS5 gameplay remain incomplete;
this startup-only run skips Metal presentation.

## Direct MULX full-width multiplication

Direct lowering now accepts VEX MULX32/64 with a register or guarded memory
explicit source and implicit EDX/RDX input. ARM64 UMULL or MUL/UMULH calculates
the full unsigned product before either destination changes. Both dword outputs
zero-extend. The low half is written first and the high half last, so the high
half remains if the destinations coincide. Inputs may alias either destination
or the memory address base. Flags are unchanged, and a memory fault preserves
both destinations. Operand order and the identical-destination rule follow
[Intel SDM volume 2B, MULX](https://cdrdv2-public.intel.com/868141/253667-089-sdm-vol-2b.pdf).

`rust/verification/direct_mulx_native.rs` passes 1474560 native cases. Its
independent product oracle sums 16-bit limb products into 128 bits. Cases cover
all high-destination/low-destination/source GPR triples, EDX/RDX aliases, both
widths, zero/one/sign-boundary/full-width operands, high-half carry propagation,
unaligned/short/unmapped reads, address overflow and flag preservation. The full
GPR bank, mapping and fault/fuel/stop state are compared. The strict-warning
standalone verifier, 687 library tests, three integration tests, strict Clippy
and release build pass. Reproduce with the release rlib/dependency flags above
using `direct_mulx_native.rs`.

Configured `--direct-startup` passes the former block at 0x38ed and exits 1 at
image-relative 0x3b9c with `Direct lowering unsupported: Neg_rm32`.
Logs are `/tmp/macps5-mulx-{tests,clippy,build,startup}.log` and retain the first
real dlsym lookup. NEG is the next CPU gap. Bootstrap and PS5 gameplay remain
incomplete; this startup-only run skips Metal presentation.

## Direct integer NEG

Direct lowering accepts unlocked NEG on 8/16/32/64-bit registers and memory.
Register forms emit ARM64 NEG after capturing the original operand, preserve
surrounding bits for narrow destinations and zero-extend dword writes. The
existing arithmetic flag helper computes zero minus the original operand.
Memory NEG uses operation 6 of the guarded RMW helper, calculating flags before
but committing them only after a successful store. Invalid reads/writes preserve
flags and bytes. LOCK forms remain explicitly unsupported by this non-atomic path.

`rust/verification/direct_neg_native.rs` uses modular subtraction and direct
carry/nibble/sign/zero/parity/overflow rules as an independent oracle. It checks
all byte values, signed-minimum overflow at all widths, all register/base
encodings, legacy high-byte aliases, partial writes, unaligned/final-byte/short/
unmapped operands, read-only mappings, prior faults and zero/one/two instruction
budgets. Expected state includes the full GPR bank, mapped bytes, flags, RIP and
fault/fuel/stop. NOT and memory arithmetic verifiers are rerun for shared paths;
the latter's invalid-operation case is now 7 because 6 implements NEG. Reproduce
with the release rlib/dependency flags above using `direct_neg_native.rs`,
`direct_not_native.rs`, and `direct_memory_arithmetic_native.rs`.

Validation passes: 255744 native NEG cases, 27648 NOT cases, 407205 memory-
arithmetic cases, 687 library tests, three integration tests, strict Clippy and
release build. Standalone verifiers compile with warnings denied. Configured
`--direct-startup` exits 1 in the same span at image-relative 0x3b9c, now with
`Direct lowering unsupported: Sbb_rm32_r32`. NEG is accepted during translation,
but this entire span has not executed. Logs are
`/tmp/macps5-neg-{tests,clippy,build,startup}.log` and retain the first real dlsym
lookup. Subtract-with-borrow is the next CPU gap. Bootstrap and PS5 gameplay
remain incomplete; this startup-only run skips Metal presentation.

## Direct ADC and SBB

Direct lowering accepts unlocked ADC/SBB at 8/16/32/64 bits with register,
immediate or guarded memory sources and register or memory destinations.
Register forms capture both operands and incoming guest CF before emitting
ARM64 addition/subtraction and calling the arithmetic flag helper. Narrow
register writes preserve surrounding bits, including legacy high-byte aliases;
dword writes zero-extend. Memory forms use operations 7/8 of the guarded RMW
helper, committing flags only after the store succeeds. LOCK remains unsupported.

`rust/verification/direct_carry_native.rs` passes 1116160 native state checks.
An independent widened-integer oracle computes carry, signed overflow, nibble
carry, sign, zero and parity. The encoding matrix covers all register/base
encodings, both carry inputs, all widths, register/memory/immediate forms,
immediate sign extension, partial-register aliases, unaligned and short reads,
and read-only stores. An additional exhaustive sweep covers every pair of byte
operands with both carry inputs for both operations. Checks compare the full
GPR bank, flags, mapped bytes where applicable, and fault/fuel/stop state.
The shared memory-arithmetic verifier passes 407205 cases; its invalid-operation
test now uses 9 because operations 7 and 8 implement ADC/SBB.

The release build, 687 library tests, three integration tests and strict Clippy
pass. Standalone verifiers compile with warnings denied. Reproduce using the
release rlib/dependency flags above with `direct_carry_native.rs` and
`direct_memory_arithmetic_native.rs`.

Configured `--direct-startup` passes the previous span at image-relative 0x3b9c
and exits 1 at span 0x501b with `Direct lowering unsupported: Cmp_rm64_imm8`.
Logs are `/tmp/macps5-carry-{tests,clippy,build,startup}.log`. This is evidence of
progress through homebrew startup, not successful bootstrap or PS5 gameplay.
The startup-only run skips Metal presentation. Memory/immediate comparison is
the next observed CPU gap.

## Direct memory comparison immediates

Memory-first CMP/TEST lowering now handles 16/32/64-bit operands with a
register or immediate second operand. CMP supports short sign-extended and
full-width encoded immediates; qword CMP/TEST sign-extends the encoded dword.
The load width comes from the decoded memory operand, including word loads.
Word register sources are captured with ARM64 LDRH. The existing guarded load
must succeed before arithmetic and flag updates; comparisons never store to
memory or alter GPRs. TEST retains the existing deterministic policy of clearing
undefined AF along with CF/OF.

The expanded `rust/verification/direct_memory_compare_native.rs` passes 307200
native state checks against an independent result/flag oracle. It covers each
width, short/full immediates at sign boundaries, all source/base register aliases,
read-only mappings, unaligned reads, short operands and overflowing addresses.
Checks compare the full GPR bank, flags, mapped bytes and fault/fuel/stop state.
The shared register/memory arithmetic verifier passes 196608 regression checks.
Reproduce with the release rlib/dependency flags above using
`direct_memory_compare_native.rs` and `direct_arithmetic_memory_native.rs`.

The release build, 687 library tests, three integration tests and strict Clippy
pass; both standalone verifiers compile with warnings denied. Configured
`--direct-startup` passes the previous span at image-relative 0x501b and exits 1
at span 0x4b1 with `Direct lowering unsupported: VEX_Vzeroupper`. Logs are
`/tmp/macps5-cmpimm-{tests,clippy,build,startup}.log`. VZEROUPPER is the next
observed CPU gap. Bootstrap and PS5 gameplay remain incomplete, and this
startup-only run skips Metal presentation.

## Direct vector state clearing

The direct ARM64 backend supports VEX VZEROUPPER and VZEROALL in the 64-bit
guest execution mode. VZEROUPPER clears bytes 16–31 of each of the sixteen
guest vector registers; VZEROALL clears all 32 bytes. Both emit bounded stores
into the CPU context without accessing guest memory or calling a helper.
Scalar registers, flags, MXCSR and x87 state are preserved. This follows the
AVX state-clearing semantics in the
[Intel instruction reference](https://cdrdv2-public.intel.com/774492/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_vector_zero_native.rs` checks both instructions with
two-byte and three-byte VEX encodings, both ignored W values, all vector bytes,
and varying nonzero patterns. It also checks zero/one/two instruction budgets,
pre-existing faults, null guest memory, preserved non-vector state and resuming
at RET without repeating the vector instruction. Reproduce with the release
rlib/dependency flags above using `direct_vector_zero_native.rs`.

Validation passes: 9216 native state cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier and updated
startup harness compile with warnings denied. Logs are
`/tmp/macps5-vzero-{tests,clippy,build,startup}.log`.

Configured direct startup now passes its pinned failure-boundary assertions:
2603840 BSS bytes cleared, ten named dependency providers accepted, and the
expected unresolved `fscanf` message followed by an owned normal exit request
with status -1. The direct backend reports 3863 translated instructions and
571 attempts. The harness now checks direct translation usage and absence of
dylib rebuilds in direct mode, retaining the old roots/rebuild assertions for
the dylib mode. The dependency trace, import bindings, guest log and exit
ownership assertions remain shared. The first run exposed this harness
backend mismatch as `Missing translation roots`; the corrected run exits 0
because it verifies the expected guest failure boundary, not successful startup.

This reaches the same scoped homebrew boundary as the older AOT path. It does
not prove CPU completeness: instruction families and execution scenarios not
covered by this path remain to be implemented and validated. Guest cleanup is
pending, game main has not executed, and PS5 gameplay remains unsupported.
The startup-only run skips Metal presentation. CPU completeness remains the
priority; reaching a library boundary does not justify calling the translator
production-ready.

## Direct CL-count register shifts

Direct SHL/SHR/SAR on 32/64-bit registers now accepts CL counts as well as
immediate counts. Both the original destination and ECX are captured before
the destination changes, so shifts of ECX/RCX itself use the original count.
ARM64 UBFX masks the count to five or six bits before the variable shift and
the existing flag helper. Counts of zero preserve flags; undefined AF and
multi-bit OF retain the existing deterministic policy of preserving old values.
Memory destinations and 8/16-bit shifts remain unsupported by this direct path.

The expanded `rust/verification/direct_shift_native.rs` compares both count
forms against iterative one-bit shifts and independent flag rules. It covers
all 256 CL values, every destination register including ECX/RCX, both widths,
sign boundaries and nonzero bits above CL that must not affect the count.
Immediate forms are checked alongside CL forms to protect the shared lowering;
the existing implicit-one, budget and pre-existing-fault checks remain.
Reproduce with the release rlib/dependency flags above using
`direct_shift_native.rs`.

Validation passes: 589830 native shift checks, 687 library tests, three
integration tests, strict Clippy and release build. The standalone verifier
compiles with warnings denied. Logs are
`/tmp/macps5-clshift-{tests,clippy,build}.log`. These checks broaden CPU coverage
beyond the pinned startup path; no new gameplay or startup claim follows from
them. Full CPU translation and PS5 game execution remain incomplete.

## Direct narrow register shifts

Direct SHL/SHR/SAR now supports 8/16-bit register operands with immediate,
implicit-one and CL counts. Byte/word loads and stores preserve the surrounding
register bits; legacy AH/CH/DH/BH and REX low-byte registers use their distinct
context offsets. SAR sign-extends the original narrow operand to 32 bits before
the ARM64 shift. Five-bit count masking permits counts at or above the narrow
width without wrapping at 8 or 16. The flag helper receives the original
operand width and retains the existing deterministic undefined-flag policy.

`rust/verification/direct_narrow_shift_native.rs` uses an iterative one-bit
oracle across all byte values, word sign boundaries, all 256 immediate/CL
counts, implicit-one encodings, and every register encoding with and without
REX. Cases include CL and CH destination overlap with the count register, and
check the full GPR bank, flags and fault/fuel/stop state. The existing wider
shift verifier covers the unchanged 32/64-bit forms on the same release build.
Reproduce using the release rlib/dependency flags above with
`direct_narrow_shift_native.rs` and `direct_shift_native.rs`.

Validation passes: 9714168 narrow native state checks, 589830 wider regression
checks, 687 library tests, three integration tests, strict Clippy and release
build. Both standalone verifiers compile with warnings denied. Logs are
`/tmp/macps5-narrowshift-{tests,clippy,build}.log`. Memory shifts remain an
explicit direct-backend gap. This adds CPU instruction coverage without changing
the previously observed homebrew library boundary or establishing gameplay.

## Direct guarded memory shifts

Direct SHL/SHR/SAR accepts 8/16/32/64-bit memory operands with immediate,
implicit-one and CL counts. Effective address and count are captured before
calling the shared guarded RMW helper. Helper operations 9/10/11 mask the count
to five/six bits and compute the shifted value and flags using the operand
width. SAR sign-extends the original value before the host shift. A successful
full-width store precedes the flag commit; read or write faults preserve flags
and bytes. Even zero-count operands follow the guarded RMW access path, matching
the existing AOT implementation. LOCK forms remain unsupported.

`rust/verification/direct_memory_shift_native.rs` uses an iterative one-bit
result/flag oracle for all widths, all 256 counts, the three count encodings,
all sixteen base registers, sign boundaries and zero/full-width values.
It includes RCX address/count overlap, unaligned/final-byte/short/unmapped reads,
overflowing addresses, read-only writes, zero counts, and full GPR/flags/memory/
fault/fuel/stop/RIP comparisons. The shared memory arithmetic verifier updates
its invalid-operation case to 12 and protects the existing RMW operations.
Reproduce with the release rlib/dependency flags above using
`direct_memory_shift_native.rs` and `direct_memory_arithmetic_native.rs`.

Validation passes: 4924800 native memory-shift checks, 407205 shared RMW
regression checks, 687 library tests, three integration tests, strict Clippy
and release build. Standalone verifiers compile with warnings denied. Logs are
`/tmp/macps5-memshift-{tests,clippy,build}.log`. These checks establish scoped
memory-shift behavior, not completed CPU translation, fault delivery or gameplay.

## Direct ROL and ROR

Direct ROL/ROR supports 8/16/32/64-bit registers and guarded memory with
immediate, implicit-one and CL counts. Register lowering uses ARM64 RORV;
ROL negates the count in a temporary without changing the count passed to flag
calculation. Narrow values are repeated across 32 bits before rotation, with
byte/word stores preserving the rest of the guest register. Memory operations
12/13 use width-reduced rotation while retaining the masked count for flags.
Memory flags commit only after the guarded store succeeds. Zero masked counts
preserve flags; nonzero full-width narrow rotations still update CF. Rotates
preserve SF/ZF/AF/PF, set OF for count one and preserve undefined multi-bit OF.
Rotate-through-carry RCL/RCR remains unsupported by this path.

The wider-register, narrow-register and memory shift verifiers now include
ROL/ROR with iterative one-bit result and flag oracles. They cover every raw
count, all registers and base aliases, partial-register state, byte inputs,
word/dword/qword sign boundaries, zero counts and full-width rotations. Memory
checks retain short/unaligned/read-only/overflowing accesses and preservation
on failure. The RMW helper invalid-operation regression now uses 14. Reproduce
with the existing release rlib/dependency flags using `direct_shift_native.rs`,
`direct_narrow_shift_native.rs`, `direct_memory_shift_native.rs` and
`direct_memory_arithmetic_native.rs`.

The expanded shift/rotate suites pass 983046 wider-register cases, 16190280
narrow-register cases, 8208000 memory cases and 407205 shared RMW regression
cases. The 687 library tests, three integration tests, strict Clippy and release
build pass; all four standalone verifiers compile with warnings denied. Logs
are `/tmp/macps5-rotate-{tests,clippy,build}.log`. These combined counts include
existing shifts, not just newly added rotations. Gameplay and full CPU
translation remain incomplete.

## Direct RCL and RCR

Direct RCL/RCR supports 8/16/32/64-bit registers and guarded memory with
immediate, implicit-one and CL counts. A Rust helper represents the operand
and incoming carry as a 9/17/33/65-bit ring in u128. The masked count is reduced
by the ring width for narrow operands; zero effective counts preserve flags.
One-bit masked counts set OF from the appropriate sign/carry relation, while
undefined multi-bit OF retains its prior value. SF/ZF/AF/PF remain unchanged.
These count and flag rules follow the
[Intel RCL/RCR reference, volume 2B](https://cdrdv2-public.intel.com/868141/253667-089-sdm-vol-2b.pdf).

Register lowering captures CL before the helper can change ECX/RCX. The helper
preserves partial byte/word destinations and zero-extends dword results.
Memory operations 14/15 of the shared RMW helper commit flags only after a
successful store. The invalid-operation regression is now 16. The helper is
compiled to native ARM64 with the Rust runtime; this path does not emit the
entire carry rotation inline in the JIT block.

`rust/verification/direct_rotate_carry_native.rs` passes 6894720 native cases
and 262144 pure byte reference cases (7156864 combined). The independent oracle
rotates one bit at a time. Native cases cover all four widths, every count,
both incoming carry states, implicit-one encodings, legacy high bytes, REX
registers, CL destination/base aliases, sign boundaries, read-only and invalid
memory operands, and complete GPR/flags/memory/fault/fuel/stop/RIP state.
The pure sweep exhausts all byte values, counts and carry inputs in both
directions. The shared memory-arithmetic verifier passes 407205 cases.

The 687 library tests, three integration tests, strict Clippy and release build
pass; standalone verifiers compile with warnings denied. Reproduce with the
release rlib/dependency flags above using `direct_rotate_carry_native.rs` and
`direct_memory_arithmetic_native.rs`. Logs are
`/tmp/macps5-rotatecarry-{tests,clippy,build}.log`. This adds instruction coverage;
it does not establish complete CPU translation or PS5 gameplay.

## Direct word arithmetic and comparisons

Direct register-destination ADD/SUB/CMP/AND/OR/XOR/TEST now accepts 16-bit
operands alongside the existing dword/qword forms. Word register loads capture
operands before aliasing writes, and word stores preserve the upper 48 bits.
Immediate16 and sign-extended Immediate8to16 inputs feed the same 16-bit flag
calculation. Guarded memory inputs load only two bytes and fault before any
register or flag update. CMP/TEST leave the destination unchanged.

`rust/verification/direct_word_arithmetic_native.rs` checks all destination/
source register combinations, short/full immediates, signed and nibble
boundaries, register aliases and surrounding register-bit preservation against
independent widened-integer/Boolean result and flag rules. The existing
`direct_arithmetic_memory_native.rs` now tests word inputs as well as dword and
qword inputs, preserving its guarded-read and alias checks. Reproduce both with
the release rlib/dependency flags above.

Validation passes: 1142784 native word-arithmetic cases and 294912 mixed-width
memory-input cases, plus 687 library tests, three integration tests, strict
Clippy and release build. The standalone verifiers compile with warnings denied.
Logs are `/tmp/macps5-wordarith-{tests,clippy,build}.log`. This expands CPU
coverage; the homebrew library boundary and lack of PS5 gameplay remain.

## Direct INC and DEC at every integer width

Direct INC/DEC now supports unlocked 8/16/32/64-bit registers and guarded
memory. Byte/word register loads and stores preserve the surrounding bits,
including legacy high-byte aliases. Dword results zero-extend. Memory operations
16/17 in the shared RMW helper add/subtract one and commit flags only after the
guarded store succeeds. The arithmetic flag helper preserves incoming CF while
updating the other defined arithmetic flags. LOCK memory forms remain rejected.

`rust/verification/direct_incdec_all_native.rs` compares against independent
modular increment/decrement, nibble, parity, sign and overflow rules. It covers
all register/base encodings, all byte inputs, sign-boundary values at every
width, both carry states, upper-bit preservation, unaligned/short/unmapped
operands, read-only stores, prior faults and zero/one/two instruction budgets.
Checks include the full GPR bank, mapped bytes, flags, RIP and fault/fuel/stop.
The shared memory-arithmetic verifier updates its invalid-operation case to 18.
Reproduce using the release rlib/dependency flags above with
`direct_incdec_all_native.rs` and `direct_memory_arithmetic_native.rs`.

Validation passes: 525312 native INC/DEC cases, 407205 shared RMW regression
cases, 687 library tests, three integration tests, strict Clippy and release
build. Both standalone verifiers compile with warnings denied. Logs are
`/tmp/macps5-incdecall-{tests,clippy,build}.log`. This closes the unlocked
INC/DEC width/addressing gap; it does not establish full CPU coverage or gameplay.

## Direct truncated IMUL

Direct two- and three-operand IMUL supports 16/32/64-bit register destinations,
register or guarded-memory sources, and short/full encoded immediates. Operands
are captured before aliasing destination writes. ARM64 MUL supplies the low
product; word writes preserve upper bits and dword writes zero-extend. The
existing signed-product flag helper sets CF/OF according to whether the full
product fits the signed destination width and preserves other undefined flags.
Guarded load faults leave the destination and flags unchanged. The one-operand
IMUL form with implicit double-width output remains unsupported by this path.

`rust/verification/direct_imul_native.rs` uses a signed-magnitude oracle that
sums shifted operands into a full i128 product. Cases cover every destination/
source or base register combination, both instruction forms, sign-extended
immediates, positive/negative boundaries, aliasing, partial-register state,
unaligned/short/unmapped and overflowing reads, and full GPR/flag/memory/RIP/
fault/fuel/stop comparisons. Reproduce using the release rlib/dependency flags
above with `direct_imul_native.rs`.

Validation passes: 470016 native IMUL cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied after correcting a spacing error in its initial source. Logs
are `/tmp/macps5-imul-{tests,clippy,build}.log`. This is instruction coverage,
not completed CPU translation or PS5 gameplay.

## Direct implicit full-product multiplication

Direct one-operand MUL/IMUL supports 8/16/32/64-bit register and guarded-memory
sources. The explicit source and implicit accumulator are captured before any
destination changes, including AH/AL/RAX/RDX and address-base aliases. ARM64 MUL
computes the low product; UMULH/SMULH computes the high half for qword operands.
Narrow signed inputs are sign-extended before multiplication. Byte forms write
AX, word forms DX:AX, dword forms EDX:EAX with zero extension, and qword forms
RDX:RAX. Unsigned CF/OF indicate a nonzero high half; signed CF/OF indicate that
the product does not fit the signed input width. Undefined flags are preserved.
Guarded-read faults preserve both destinations and flags.

`rust/verification/direct_full_multiply_native.rs` uses signed-magnitude
shift-and-add arithmetic as an independent full-product/overflow oracle.
Cases cover every register/base encoding, all widths, source/output aliases,
positive/negative sign boundaries, unaligned/short/unmapped/overflowing reads,
prior faults and instruction budgets. It compares the full GPR bank, memory,
flags, RIP and fault/fuel/stop state. A separate native byte sweep exhausts
all 65536 input pairs for each of MUL and IMUL. Reproduce with the release
rlib/dependency flags above using `direct_full_multiply_native.rs`.

Validation passes: 296960 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied. Logs are `/tmp/macps5-fullmul-{tests,clippy,build}.log`.
This closes the direct implicit-product multiplication gap; division and other
CPU families still need coverage, and PS5 gameplay remains unsupported.

## Direct DIV and IDIV

Direct DIV/IDIV supports 8/16/32/64-bit register and guarded-memory divisors.
The divisor is captured before calling the existing Rust division helper,
including AL/AH/RAX/RDX aliases. Guarded source failures stop before division.
The helper consumes AX or DX:AX/EDX:EAX/RDX:RAX, writes the quotient/remainder
with the required partial-register rules, and preserves undefined flags.
Zero divisors and out-of-range quotients produce modeled divide fault 13
without changing either output. The direct block stops on that fault; delivery
of an architectural exception to guest handlers is a separate incomplete task.

`rust/verification/direct_divide_native.rs` uses bit-by-bit unsigned long
division of signed magnitudes, then applies quotient/remainder signs and range
checks. The oracle does not use the runtime's division implementation. Cases
cover every register/base encoding, all widths, implicit-output aliases,
zero divisors, signed minimum divided by -1, quotient boundaries, remainder
sign, partial-register outputs, unaligned/short/unmapped/overflowing reads,
prior faults and zero/one/two instruction budgets. Full GPR/flags/memory/RIP/
fault/fuel/stop state is compared. Reproduce using the release rlib/dependency
flags above with `direct_divide_native.rs`.

Validation passes: 221184 native division cases, 687 library tests, three
integration tests, strict Clippy and release build. The standalone verifier
compiles with warnings denied. Logs are
`/tmp/macps5-divide-{tests,clippy,build}.log`. This establishes scoped division
behavior, not full CPU readiness or PS5 gameplay.

## Direct accumulator sign extension

Direct CBW/CWDE/CDQE sign-extends AL to AX, AX to EAX, or EAX to RAX.
CWD/CDQ/CQO replicates the accumulator sign into DX/EDX/RDX while preserving
the accumulator itself. ARM64 bitfield instructions extract and extend the
required sign bits. Word stores preserve surrounding bits, while dword outputs
zero-extend into the 64-bit guest register. All guest flags remain unchanged.

`rust/verification/direct_accumulator_extend_native.rs` checks all byte inputs
for CBW, all word inputs for CWDE/CWD, and boundary values for every form,
including nonzero upper bits that must be preserved or cleared. It checks full
GPR state, flags, prior faults, instruction budgets and RIP. Composed native
sign-extension/IDIV sequences exercise signed quotient/remainder behavior at
all four widths. Reproduce with the release rlib/dependency flags above using
`direct_accumulator_extend_native.rs`.

Validation passes: 525736 native state/composition checks, 687 library tests,
three integration tests, strict Clippy and release build. The standalone
verifier compiles with warnings denied. Logs are
`/tmp/macps5-accumext-{tests,clippy,build}.log`. These checks establish scoped
CPU behavior; full translator readiness and PS5 gameplay remain incomplete.

## Paired accumulator extension backend check

A source review initially suspected the older AOT CWDE path sign-extended into
all of RAX. Native verification disproved that suspicion: the source and emitted
assembly use `ldrsh w20`, followed by a 64-bit context store, so upper bits are
correctly zeroed. No runtime fix was necessary.

`rust/verification/aot_accumulator_extend_native.rs` now generates the actual AOT
assembly for all six accumulator-extension forms, assembles it with the Apple
ARM64 assembler, links it into a Rust driver, and executes it alongside the
direct JIT on identical CPU states. Both backends are checked against explicit
signed-width conversion rules. All 131406 paired cases pass, including every
byte input for CBW, every word input for CWDE/CWD, nonzero upper bits and sign
boundaries. Full GPR banks, flags and fault state are compared. Control-stop
states differ because AOT ends at a region boundary while the direct fixture
stops on its instruction budget, so this test makes no equivalence claim about
those control states.

Build the standalone verifier with the release rlib/dependency flags above,
then run it with the absolute release rlib path and an output directory:

```sh
/tmp/macps5-aot-accumext "$PWD/rust/target/release/libnative_ps5.rlib" /tmp/macps5-aot-accumext-paired
```

The verifier and generated Rust driver compile with warnings denied. The final
log is `/tmp/macps5-aot-accumext-paired.log`. The initial global-assembly attempt
was replaced with the repository's existing `xcrun clang` object-file workflow
to handle Mach-O local labels correctly. No runtime source changed, so this is
additional backend validation, not expanded instruction support or gameplay.

## Direct BSWAP

Direct BSWAP32/64 now emits ARM64 REV on the selected register. Dword results
zero-extend into the guest 64-bit register; qword results reverse all eight
bytes. Flags and other registers remain unchanged. The undefined 16-bit form
is not added to the supported instruction list.

`rust/verification/direct_bswap_native.rs` independently reconstructs the
reversed value byte by byte. It varies every byte position through all 256
values, for both widths and all sixteen registers, with nonzero surrounding
bytes. It checks full GPR state, flags, RIP, prior faults and zero/one/two
instruction budgets. Reproduce with the release rlib/dependency flags above
using `direct_bswap_native.rs`.

Validation passes: 262144 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied. Logs are `/tmp/macps5-bswap-{tests,clippy,build}.log`. This
extends CPU coverage without establishing complete translation or gameplay.

## Direct bit scans and counts

Direct BSF/BSR/LZCNT/TZCNT/POPCNT supports 16/32/64-bit register destinations
and register or guarded-memory sources. A Rust helper computes the result and
defined flags after the source has been captured. Word results preserve upper
bits and dword results zero-extend. For zero-input BSF/BSR, ZF is set and the
architecturally undefined destination is preserved deterministically, including
its upper bits. Other undefined flags are preserved. LZCNT/TZCNT set CF from
zero input and ZF from zero result; POPCNT clears the other arithmetic flags
and sets ZF for zero input. Guarded-read faults preserve destination and flags.

`rust/verification/direct_bit_scan_native.rs` independently enumerates set-bit
positions to derive indices and counts. It tests every destination/source or
base register pair, all widths and operations, zero, all-one, alternating and
every single-bit input, including bits outside a narrow operand. Memory checks
cover unaligned/short/unmapped/overflowing reads. Full GPR, flags, memory, RIP
and fault/fuel/stop state are compared; separate helper guards check invalid
widths, destinations, operations and pre-existing faults. Reproduce with the
release rlib/dependency flags above using `direct_bit_scan_native.rs`.

Validation passes: 1566720 native cases plus five helper guard cases, 687
library tests, three integration tests, strict Clippy and release build. The
standalone verifier compiles with warnings denied. Logs are
`/tmp/macps5-bitscan-{tests,clippy,build}.log`. This adds instruction coverage
through a native Rust helper, not a proof of full CPU readiness or gameplay.

## Direct BMI1 lowest-bit operations

Direct BLSI/BLSMSK/BLSR supports VEX-encoded 32/64-bit register destinations
and register or guarded-memory sources. BLSI isolates the lowest set bit,
BLSMSK fills bits through that bit, and BLSR clears that bit. Source capture
precedes destination updates, including register aliases. Dword results
zero-extend. Memory faults preserve destination and flags.

The Rust helper updates defined CF/ZF/SF, clears OF and preserves undefined
AF/PF. BLSI sets CF for nonzero input; BLSMSK and BLSR set CF for zero input.
BLSMSK clears ZF even when its source is zero (the result is then all ones).
These rules follow the Operation and Flags Affected sections of the
[Intel instruction reference](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf).
The BLSI description paragraph contradicts its operation/flags sections on
zero-input carry; implementation and verification follow the latter sections.

The expanded `rust/verification/direct_bit_scan_native.rs` derives expected
results by enumerating set-bit positions, independently of the helper's
arithmetic formulas. It covers all destination/source and destination/base
register pairs, aliases, both widths, zero/all-one/alternating/single-bit
inputs, and guarded unaligned, short, unmapped and overflowing reads. It
compares full GPR, flags, memory and execution-stop state. An additional helper
guard rejects a nonexistent 16-bit BMI1 operation.

Validation: 2193408 combined native cases (626688 added BMI1 cases) plus six
helper guards pass, as do 687 library tests, three integration tests, strict
Clippy and the release build. Reproduce with the existing release rlib and
dependency flags using `direct_bit_scan_native.rs`. Build/test logs are
`/tmp/macps5-bls-{tests,clippy,build}.log`. This is additional CPU instruction
coverage; complete translation, production readiness and gameplay remain
unverified.

## Direct scalar/vector transfers and compiled memory probe

Direct legacy/VEX MOVD/MOVQ supports 32/64-bit GPR-to-XMM and XMM-to-GPR
transfers, guarded memory loads/stores and the separate XMM qword move
encodings. XMM results clear unused low-128-bit space; legacy forms preserve
the upper YMM half while VEX forms clear it. Dword GPR writes zero-extend.
Memory stores capture their source before effective-address scratch use.
Faults preserve register and memory state; flags and MXCSR remain unchanged.

`rust/verification/direct_scalar_vector_native.rs` checks all register/base
pairs, both encoding families, both widths and directions, register aliases,
unaligned and short reads, overflow and read-only stores. It compares all
modeled vectors, GPRs, flags, MXCSR, memory, RIP and fault/fuel/stop state.
All 33792 native cases pass, alongside 687 library tests, three integration
tests, strict Clippy and release build. Logs are
`/tmp/macps5-scalar-vector-{tests,clippy,build}.log`.

The compiler/extraction utility was shared as `compiled_workload_support.rs`.
The integer workload was rerun and still passes all 120 results, warm-cache
and fuel checks with 54 instructions, seven attempts and 7328 JIT bytes.

The new `compiled_memory_guest.rs` performs an in-place u64 transform and a
checksum; `compiled_memory_native.rs` checks full buffers and returns across
aligned and unaligned pointers. This integration probe is **not passing yet**.
Its first missing form was `Movq_xmm_rm64`, now implemented and independently
tested. At that checkpoint it failed at a translation span starting at `0x10011f` with
`Direct lowering unsupported: Pshufd_xmm_xmmm128_imm8`. The probe deliberately
fails on that error; it does not count reaching the boundary as success.

Reproduce each standalone verifier with the release rlib/dependency flags
above. Run `compiled_memory_native` with the path to `compiled_memory_guest.rs`
to reproduce the pending integration gap. The compiled-memory fixture and
PSHUFD integration remain work in progress; no gameplay or full CPU-readiness
claim follows from the completed MOVD/MOVQ checks.

## Direct BZHI

Direct BZHI supports 32/64-bit register and guarded-memory sources with native
ARM64 masking and flag updates. Only the low eight index bits are used. An
index at or above operand width preserves the source and sets CF; a smaller
index clears bits starting at that index. Dword results zero-extend. SF/ZF
reflect the result, OF is cleared, and undefined AF/PF are preserved. Sources
are captured before destination writes, so aliases are supported. Read faults
leave the destination and flags unchanged. Semantics follow the Operation and
Flags Affected sections of the
[Intel BZHI reference](https://cdrdv2-public.intel.com/671110/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_bzhi_native.rs` uses independent per-bit selection
as its expected-result oracle. It covers every destination/source/index
register triple, including memory-base aliases; both widths; index boundaries;
zero and patterned values; and unaligned, short and overflowing memory reads.
A fixed-register sweep covers all 65536 low-word index values against six
source patterns, ensuring the high index byte is ignored. Initial arithmetic
flags vary between set and clear states. Full GPR, flags, RIP, memory and
fault/fuel/stop state are checked.

Validation passes: 1605632 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied; reproduce using the release rlib/dependency flags above and
`direct_bzhi_native.rs`. Logs are `/tmp/macps5-bzhi-{tests,clippy,build}.log`.
This extends the direct CPU backend; it does not establish complete instruction
coverage, production readiness, or PS5 gameplay.

## Direct PDEP and PEXT

Direct VEX PDEP/PEXT supports 32/64-bit operands, with a register value and a
register or guarded-memory mask. A native Rust helper scatters or gathers bits
by removing the mask's lowest set bit on each iteration, bounded by 64
iterations. Inputs are captured before destination writes; dword inputs and
outputs zero-extend. Guest flags remain unchanged, and memory-read faults
preserve destination state. Semantics and operand ordering follow the
[Intel instruction reference](https://cdrdv2-public.intel.com/868141/253667-089-sdm-vol-2b.pdf).

`rust/verification/direct_parallel_bits_native.rs` constructs an independent
list of selected bit positions and maps input bits to output positions. Native
execution covers every destination/value/mask register triple, both widths and
operations, register and memory aliases, zero/all-one/alternating/sparse masks,
and valid, unaligned, short and overflowing reads. Additional sweeps exhaust
all byte-sized value/mask pairs and every single-bit value/mask pair across
64 bits. Full GPR, flags, RIP, memory and fault/fuel/stop state are checked.

Validation passes: 1589248 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied; reproduce using the release rlib/dependency flags above and
`direct_parallel_bits_native.rs`. Logs are
`/tmp/macps5-parallel-bits-{tests,clippy,build}.log`. These are correctness
checks for this instruction family; helper performance, production translator
readiness and PS5 gameplay are not established.

## Direct bit tests and mutations

Direct legacy BT/BTS/BTR/BTC supports 16/32/64-bit register and guarded-memory
operands with register or immediate indices. A native Rust helper publishes
the old selected bit to CF, preserving other flags deterministically. Register
indices wrap within operand width; memory register indices are signed and
select an operand-sized word before selecting its bit. Immediate indices stay
within the addressed word. Word register writes preserve upper bits; dword
mutations zero-extend, while BT leaves the entire register unchanged. The
addressing rules follow the [AMD instruction reference](https://www.amd.com/content/dam/amd/en/documents/processor-tech-docs/programmer-references/24594.pdf).

Memory reads and writes use guarded mappings. A failed read or write preserves
CF; read-only BT works, while mutations require a writable full operand span.
LOCK-prefixed forms remain unsupported and explicitly rejected. This path
does not claim atomic read-modify-write behavior.

`rust/verification/direct_bit_test_native.rs` independently selects and changes
bits within byte arrays. Native checks cover all register/base/index pairs,
aliases, all widths and operations, signed index boundaries, out-of-mapping
addresses, read-only mappings, and every encoded immediate against each
single-bit input. Full GPR, flags, memory, RIP and fault/fuel/stop state are
checked, including destination/CF preservation on faults and read-only writes.

Validation passes: 823296 native cases plus explicit LOCK rejection, 687
library tests, three integration tests, strict Clippy and release build.
The standalone verifier compiles with warnings denied; reproduce with the
release rlib/dependency flags above using `direct_bit_test_native.rs`. Logs are
`/tmp/macps5-bittest-{tests,clippy,build}.log`. Full translator readiness,
atomic instruction support and PS5 gameplay remain incomplete.

## Direct flag control

Direct CLC/STC/CMC clears, sets or complements CF; CLD/STD clears or sets DF.
LAHF writes the five arithmetic status flags to AH with the prescribed fixed
bits, preserving AL, the rest of RAX and guest flags. SAHF copies AH's five
status bits into guest flags, preserving OF, DF and all other bits. All seven
instructions emit ARM64 code directly. They follow the normal prior-fault and
instruction-budget guards. Invalid LOCK-prefixed forms are rejected. LAHF's
fixed-bit behavior follows the
[Intel instruction reference](https://cdrdv2-public.intel.com/671110/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_flags_native.rs` independently updates flags bit by
bit and AH as a byte. It sweeps the low twelve flag bits with architectural
reserved-bit values, five AH patterns, zero/one/two instruction budgets and a
pre-existing fault. An additional SAHF sweep tests every AH byte against every
low flag byte. Full GPR, flags, RIP and fault/fuel/stop state are checked in the
main sweep; all seven LOCK forms are rejected.

Validation passes: 638976 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied; reproduce with the release rlib/dependency flags above using
`direct_flags_native.rs`. Logs are `/tmp/macps5-flags-{tests,clippy,build}.log`.
This establishes these flag operations within the direct backend, not full
CPU readiness, string-instruction support or PS5 gameplay.

## Direct double-width shifts

Direct SHLD/SHRD supports 16/32/64-bit register or guarded-memory destinations,
a register source and immediate or CL counts. The native Rust helper combines
the two operands using u128 arithmetic. Counts are masked to five bits (word
and dword) or six bits (qword). Defined CF/SF/ZF/PF and single-bit OF are
updated; AF and multi-bit OF are preserved deterministically. Zero counts
preserve flags. Word counts 17..31 have undefined result and flags; the helper
deterministically preserves the original word and flags. These boundaries
follow the [Intel instruction reference](https://cdrdv2-public.intel.com/671110/325383-sdm-vol-2abcd.pdf).

Sources and count are captured before destination writes. Word writes preserve
upper bits; dword writes zero-extend. Memory writes are guarded even for a
masked-zero count, and read/write faults preserve destination and flags.
LOCK-prefixed forms are rejected.

`rust/verification/direct_double_shift_native.rs` independently shifts arrays
of individual bits one step at a time. Checks cover every source/destination
or base register pair, CL aliases, both directions, all widths, count and
sign boundaries, every immediate count, single-bit inputs, zero/one/patterned
data, unaligned and short reads, overflowing addresses and read-only writes.
Full GPR, flags, memory, RIP and fault/fuel/stop state are compared.

Validation passes: 927744 native cases and explicit LOCK rejection, 687 library
tests, three integration tests, strict Clippy and release build. The standalone
verifier compiles with warnings denied; reproduce using the release rlib and
dependency flags above with `direct_double_shift_native.rs`. Logs are
`/tmp/macps5-double-shift-{tests,clippy,build}.log`. These scoped checks do not
establish production CPU readiness or PS5 gameplay.

## Direct counter branches

Direct LOOP/LOOPE/LOOPNE supports both RCX and address-override ECX counters.
It decrements the selected counter without changing guest flags, then tests
nonzero count and (where required) guest ZF. ECX writes zero-extend into RCX.
JRCXZ/JECXZ tests the selected counter without decrementing or writing it.
These operations emit ARM64 code directly and use the same internal labels,
external-target dispatch, instruction budgets and boundary rejection as Jcc.
Semantics follow the [Intel instruction reference](https://cdrdv2-public.intel.com/812383/253666-sdm-vol-2a.pdf).

`rust/verification/direct_counter_branch_native.rs` independently simulates
execution until RET, external dispatch, prior fault or budget exhaustion. It
covers all eight forms, both ZF states and other arithmetic flags, zero and
wrapping counters, high RCX bits ignored by ECX forms, forward branches and
backward loops, all three target layouts and six instruction budgets. Full
GPR, flags, RIP, target, fault, stop and remaining fuel are compared. Targets
inside an instruction are rejected. An actual NativeExecutor loop decrements
three times and crosses into a separately registered continuation returning 42.

Validation passes: 92160 new native state checks; the executor loop; 5120
existing Jcc truth-table/budget checks, three existing backward-loop checks
and executor branch result 7; 687 library tests, three integration tests,
strict Clippy and release build. Both standalone verifiers compile with
warnings denied. Reproduce using the release rlib/dependency flags above with
`direct_counter_branch_native.rs` and `direct_branch_native.rs`. Logs are
`/tmp/macps5-counter-{tests,clippy,build}.log`. The old LOOP rejection fixture
now specifically rejects a target inside LOOP's displacement byte.

The accompanying source audit found no implemented CPUID path or complete
versioned target-feature profile. This remains a release requirement; added
instruction support must not be treated as a complete advertised CPU contract.
Architectural fault delivery and PS5 gameplay also remain unverified.

## Direct packed vector logic

The direct backend supports AND/AND-NOT/OR/XOR in legacy SSE128 and VEX128/256
forms: ANDPS/ANDPD/PAND, ANDNPS/ANDNPD/PANDN, ORPS/ORPD/POR, XORPS/XORPD/PXOR
and their V-prefixed equivalents. MMX and EVEX forms remain outside this path.
Register operands emit ARM64 logical operations; memory operands use the
existing full-span guarded vector helper. A legacy wrapper enforces 16-byte
memory alignment and preserves the destination's upper YMM half. VEX128 clears
that half; VEX256 writes the full modeled vector. AND-NOT negates the first
source. Guest flags and MXCSR are unchanged, and faults preserve all vectors.
Upper-lane behavior follows the
[Intel instruction reference](https://cdrdv2-public.intel.com/868140/253666-089-sdm-vol-2a.pdf).

`rust/verification/direct_vector_logic_native.rs` computes expected values
with Boolean truth tables per bit. It covers every VEX destination/source
triple and legacy destination/source pair, both register and memory operands,
all four operations and three opcode families, both VEX widths, aliases and
nonzero upper lanes. Memory cases include aligned and unaligned addresses,
last-valid spans, short/unmapped reads and overflowing addresses. Checks
compare all modeled vectors and GPRs, flags, MXCSR, memory, RIP and
fault/fuel/stop state.

Validation passes: 811008 native cases plus all 98308 existing vector-XOR
regression cases (including prior-fault and zero-budget guards), 687 library
tests, three integration tests, strict Clippy and release build. Both standalone
verifiers compile with warnings denied. Reproduce using the release rlib and
dependency flags above with `direct_vector_logic_native.rs` and
`direct_vector_xor_native.rs`. Logs are
`/tmp/macps5-vector-logic-{tests,clippy,build}.log`. Alignment failures are modeled
guard faults, not delivered architectural exceptions. This adds CPU vector
coverage without establishing complete SIMD, GPU, or PS5 game support.

## Direct legacy packed moves

Direct MOVUPS/MOVUPD/MOVDQU and MOVAPS/MOVAPD/MOVDQA now support both opcode
directions for register copies and guarded loads/stores. Legacy register
writes preserve the destination's upper YMM half; VEX128 continues clearing it.
The aligned legacy forms require 16-byte alignment, while unaligned forms
accept any mapped full operand. Failed accesses leave vectors and memory
unchanged. Guest flags, MXCSR and GPRs are preserved. These semantics follow
the [Intel instruction reference](https://cdrdv2-public.intel.com/868137/325462-089-sdm-vol-1-2abcd-3abcd-4.pdf).

The expanded `rust/verification/direct_vector_move_native.rs` checks legacy
SSE128 alongside VEX128/256 for all six opcode families, both directions,
all register/base pairs, aliases, nonzero upper lanes and read-only versus
writable mappings. Addresses cover alignment, last-valid spans, short and
unmapped accesses, and overflow. It compares every modeled vector, GPR,
memory byte, flags, MXCSR, RIP and fault/fuel/stop state. Every encoding fixture
also executes with a prior fault and with zero instruction budget, verifying
that neither accesses memory nor changes architectural state.

Validation passes: 175104 combined native cases (138240 operand/state cases
and 36864 prior-fault/budget cases), 687 library tests, three integration tests,
strict Clippy and release build. The standalone verifier compiles with warnings
denied; reproduce using the release rlib/dependency flags above with
`direct_vector_move_native.rs`. Logs are
`/tmp/macps5-legacy-move-{tests,clippy,build}.log`. Alignment faults remain runtime
guard stops; architectural exception delivery, full SIMD readiness and PS5
gameplay are not established.

## Direct packed integer add and subtract

Direct PADDB/PADDW/PADDD/PADDQ and PSUBB/PSUBW/PSUBD/PSUBQ, plus their VEX
counterparts, support legacy SSE128 and VEX128/256. Register operands emit
ARM64 SIMD ADD/SUB with the selected element size; 256-bit operations use two
128-bit halves. Carries and borrows wrap within each element. Memory sources
use the existing guarded Rust helper, with a legacy wrapper enforcing 16-byte
alignment and preserving upper YMM bits. VEX128 clears upper YMM bits. Source
and destination aliases are supported; flags and MXCSR remain unchanged.
Semantics follow the [Intel instruction reference](https://cdrdv2-public.intel.com/868141/253667-089-sdm-vol-2b.pdf).

`rust/verification/direct_packed_addsub_native.rs` independently calculates
results using byte-by-byte carry/borrow propagation, reset at each element
boundary. It checks every VEX destination/source triple and legacy register
pair, all four element sizes, both operations, register and memory sources,
aliases, alignment, last-valid spans, short/unmapped reads and overflow.
Full modeled vectors, GPRs, memory, flags, MXCSR, RIP and fault/fuel/stop state
are compared; fault cases preserve destination vectors.

Validation passes: 540672 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied; reproduce with the release rlib/dependency flags above using
`direct_packed_addsub_native.rs`. Logs are
`/tmp/macps5-packed-addsub-{tests,clippy,build}.log`. These checks establish
scoped correctness, not measured game performance or complete CPU/GPU support.

## Compiled Rust integer workload

`rust/verification/compiled_integer_guest.rs` is a freestanding Rust mixing
workload with variable rotates, multiplication, shifts, XOR and loop control.
`compiled_integer_native.rs` invokes rustc for x86_64-apple-darwin at opt-level 2,
extracts its single relocation-free text section, places it in a synthetic ELF
image, and executes it through NativeExecutor's automatic direct translator.
The Mach-O extractor rejects text relocations and extra executable sections;
this harness does not claim general Mach-O loading or PS5 binary compatibility.

An independent u128 modular-arithmetic model checks the folded 32-bit result
for six seeds and ten round counts, repeated twice. The fold exercises both
halves of the 64-bit accumulator within the executor's 32-bit module-return
contract. A second pass must preserve translation-attempt counts and JIT byte
usage. A u64::MAX round count must terminate with an instruction-fuel error.
No AOT/dylib fallback is allowed for the registered direct source.

Measured validation passes: 120 result checks, stable warm cache, explicit
fuel stop and zero loaded translation dylibs. This compiler build emitted 198
guest text bytes; execution discovered 54 instructions in seven attempts and
used 7328 JIT bytes. These sizes are observations, not hard-coded assertions
or representative game memory/performance estimates. Only verification files
changed; the previously passing library/Clippy checks were not rerun.

Reproduce by compiling `compiled_integer_native.rs` with the release rlib and
dependency flags above, then running its executable with the path to
`rust/verification/compiled_integer_guest.rs`. Rust's x86_64-apple-darwin core
target must be installed. Both verifier and guest compile with warnings denied.
This adds evidence for mixed compiler-generated CPU execution and cache reuse;
full instruction coverage, architectural fault delivery and gameplay remain
unverified.

## Direct packed dword shuffle

Direct PSHUFD/VPSHUFD supports legacy SSE128 and VEX128/256 register and
guarded-memory sources. Register lowering captures all four source dwords
before writing a 128-bit half, preserving source/destination aliases. Each
256-bit half shuffles independently. Legacy forms preserve upper YMM bits;
VEX128 clears them. The legacy memory wrapper requires 16-byte alignment and
preserves upper bits; failed reads preserve all vectors. These rules follow
the [Intel instruction reference](https://cdrdv2-public.intel.com/774492/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_shufd_native.rs` independently decodes the immediate
as four base-four selectors and copies bytes from the selected dwords. It
covers all 256 controls, every source/destination or base pair, all three
encoding/width forms, aliases, independent halves and nonzero upper lanes.
Memory cases cover alignment, unaligned VEX reads, last-valid spans, short
reads and overflow. All modeled vectors, GPRs, flags, MXCSR, memory, RIP and
fault/fuel/stop state are compared.

Validation passes: 1376256 native cases, 687 library tests, three integration
tests, strict Clippy and release build. The standalone verifier compiles with
warnings denied; reproduce with the release rlib/dependency flags above using
`direct_shufd_native.rs`. Logs are `/tmp/macps5-shufd-{tests,clippy,build}.log`.

The compiled memory workload was rerun against the new release. Its next
explicit missing form is `Psrlq_xmm_imm8`, reported for the translation span
at `0x10011f`. At that checkpoint it remained a failing integration probe;
passing shuffle checks alone did not establish a compiled-memory result.

## Direct packed qword shift and compiled memory execution

Direct immediate PSRLQ/VPSRLQ supports legacy SSE128 and VEX128/256 register
forms. Each 64-bit lane lowers to an ARM64 logical shift; counts at least 64
produce zero, without scalar-style count masking. Legacy preserves upper YMM
bits and VEX128 clears them. Source/destination aliases are safe because each
lane is read before its corresponding write. Flags and MXCSR are unchanged.
Register/memory-supplied counts, MMX and EVEX forms remain unsupported by this
addition. Semantics follow the packed logical shift section of the
[Intel instruction reference](https://cdrdv2-public.intel.com/774492/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_psrlq_native.rs` uses an independent bit-by-bit
reference for all 256 immediate counts, every valid source/destination pair,
all three forms and four input patterns. It compares every vector and GPR,
flags, MXCSR, RIP and fault/fuel/stop state. All 540672 result cases and 270336
prior-fault/zero-fuel guards pass on the ARM64 host. The 687 library tests,
three integration tests, strict Clippy and release build also pass.
Logs: `/tmp/macps5-psrlq-{tests,clippy,build}.log`.

The existing optimized Rust memory guest now executes successfully through
the automatic direct translator. Its 72 checks cover 12 element counts,
three buffer alignments and two execution passes. Each compares the return
value and the entire output buffer, including surrounding sentinels, against
the independent u128 arithmetic model. The measured guest is 200 bytes;
translation uses 56 discovered instructions, eight attempts and 7144 JIT
bytes, with unchanged translation/arena usage on the warm pass and zero
translation dylibs. The compiled integer guest still passes all 120 cases
and its fuel-exhaustion check. These are compiler-generated workload results,
not proof of full instruction coverage, production readiness or gameplay.

Reproduce with the release rlib/dependency compiler flags above using
`direct_psrlq_native.rs`, `compiled_memory_native.rs` and
`compiled_integer_native.rs`. Pass the corresponding `compiled_*_guest.rs`
source path to each compiled workload verifier; the x86_64-apple-darwin Rust
target must be installed. All verifier builds use `-Dwarnings`.

## Direct scalar floating-point moves

Direct MOVSS/MOVSD now supports legacy register and memory forms, plus VEX
three-register merges, loads and stores, including both register opcodes.
Legacy register moves preserve the destination bits above the scalar; legacy
loads clear the rest of the low 128 bits and preserve the upper YMM half.
VEX register forms merge the non-scalar low-128 bits from their first source
and clear upper YMM bits; VEX loads clear every bit above the scalar. Stores
write only the scalar bytes. The scalar source is captured before merge
writes so every source/destination alias remains valid. No host floating-point
arithmetic is used for these bit-preserving moves. Semantics follow the MOVSS
and MOVSD sections of the
[Intel reference](https://cdrdv2-public.intel.com/789581/325383-sdm-vol-2abcd.pdf).

`rust/verification/direct_float_move_native.rs` passes 41984 native state
checks, covering every register pair/triple, both opcodes and both scalar
widths. Memory cases include unaligned accesses, final valid spans, short
spans, address overflow and read-only stores. The verifier compares all
vectors, GPRs, flags, MXCSR, memory bytes and RIP/fault/fuel/stop state. The
33792 existing MOVD/MOVQ checks also pass after extending their shared lowering.
All 687 library tests, three integration tests, strict Clippy and release
build pass; logs are `/tmp/macps5-float-move-{tests,clippy,build}.log`.

The new `compiled_float_guest.rs` is a no_std optimized Rust recurrence that
loads and multiplies f64 elements, writes each cumulative product, and returns
a fold of the final result's bits. `compiled_float_native.rs` supplies exact
powers of two and alternating signs, and constructs expected IEEE binary64
bits using integer exponent/sign arithmetic. Its intended 72 cases cover
12 lengths, three alignments and cold/warm passes, checking every buffer byte
and the return value. This test targets compiled control/data flow; it does
not exercise rounding, NaNs, subnormals or floating-point exceptions.

The probe initially stopped at `Movsd_xmm_xmmm64`. With these moves it reaches
the explicit `Mulsd_xmm_xmmm64` gap, reported for the translation span starting
at `0x10016e`. At that checkpoint it failed and was not a passing workload. Run it
using the release rlib/dependency compiler flags above, passing the path to
`rust/verification/compiled_float_guest.rs`. The new native move verifier uses
the same compiler flags. EVEX moves and full CPU/game readiness remain outside
the proven scope.

## Direct scalar binary64 multiplication

Direct MULSD and VEX VMULSD now support register and guarded-memory second
operands. Lowering calls the existing integer-only Rust
`native_ps5_multiply_binary64_v1` model, checks its fault state before committing
the result, and applies the legacy/VEX upper-bit rules. Legacy preserves all
destination bits above the scalar; VEX copies bits 127:64 from its first source
and clears upper YMM bits. All source/destination aliases are safe. Guarded
memory failures occur before arithmetic or MXCSR updates. This is native Rust
helper execution, not a new host floating-point fast path.

The existing model handles rounding modes, DAZ/FTZ, signed zeros, infinities,
NaNs and sticky exception status. Unmasked exceptions update MXCSR but leave
the destination unchanged and stop with diagnostic fault 15. Reserved controls
use diagnostic fault 14. These stops do not constitute complete architectural
guest exception delivery. This change reuses the model without changing its
arithmetic; its primary references and limitations remain documented in
`rust/src/scalar_fp/arithmetic.rs` and earlier readiness checkpoints.

`rust/verification/direct_mulsd_native.rs` passes 379168 native state checks and
17408 prior-fault/zero-fuel guards. It checks every legacy register pair and
VEX register triple, memory bases, source aliases, both forms' upper bits,
short/overflowing reads, all four rounding controls, DAZ/FTZ, signaling/quiet
NaN priority, invalid/denormal/overflow/precision exceptions and reserved
controls. Expected arithmetic bits and status are specified explicitly rather
than calculated with the helper under test. Every vector, GPR, flags, MXCSR,
x87 controls, memory bytes and RIP/fault/fuel/stop state is checked. No hardware
x86 differential comparison is claimed for this verifier.

The compiled Rust floating-point recurrence now passes all 72 return and
full-buffer checks. This build is 162 guest bytes and uses 42 discovered
instructions, seven translation attempts and 6224 JIT bytes; warm-pass usage
is unchanged and no translation dylibs are loaded. Its exact-power inputs
validate compiled execution and memory integration, not broad numerical edge
coverage; those edge cases are checked separately above.

All 687 library tests, three integration tests, strict Clippy and the release
build pass. Logs are `/tmp/macps5-mulsd-{tests,clippy,build}.log`. Reproduce using
the release rlib/dependency flags above with `direct_mulsd_native.rs` and
`compiled_float_native.rs`; pass `compiled_float_guest.rs` to the latter.
Both verifier compilations use `-Dwarnings`. Full floating-point instruction
coverage, production CPU readiness and PS5 gameplay remain unproven.
