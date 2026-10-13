# Allocating printf implementation audit

## Wide conversion increment

The registered service now supports `%lc` and `%ls` under the audited C-byte/wchar16 policy. Encoding failures publish NULL and EILSEQ after preserving already consumed arguments. Source-required temporary allocations are owned and explicitly freed; a scratch cleanup failure no longer skips cleanup of the independent output allocation. Successful large-precision sizing can set errno as the source does, with guarded writes.

The matching release passes 441 library tests, three integration tests and strict Clippy (`wide-strcoll-build.log`). The actual `asprintf_l` wrapper passes low16 character narrowing, high bytes, embedded NUL, EILSEQ, string precision 0/2/127/128, repeated temporary cleanup, source sizing beyond NUL, successful sizing errno side effects, and readonly/unmapped errno rejection (`work/vasprintf-wide-native/native.log`, 62755). Existing integer cases also pass. A subsequent dispatcher regression verifies NULL/EILSEQ publication, consumed arguments and released output ownership (`wide-service-model.log`). Native allocator exhaustion and floating conversion remain unverified.

The decimal FP-effects module is registered for independent testing, not enabled in guest formatting. Three focused checks and integrated strict Clippy pass (`printf-effects-integrated.log`); the source-operation model matched 98,304 upstream x86 oracle cases. Internal dtoa allocation behavior, runtime effect publication and source-piece emission still require integration. This evidence does not establish console compiler equivalence.

The separate `printf_float_field` adapter now combines source-format pieces with modeled FP effects, sign/prefix handling and ordered output batching. Four integrated tests and strict Clippy pass (`printf-field-integrated.log`): hexadecimal sign/prefix/zero ordering, tiny-source output with retained FP effects on sink failure, NaN sign suppression, and INT_MAX field rejection without flushing an earlier queued literal. This is a component API; the executor does not yet publish its effects or call it. Source gdtoa pool/freelist ownership and allocation ordering remain required before enabling it in the guest service.

## Earlier integer/text checkpoint

Status: integer/text service registered and independently exercised through the pinned `asprintf_l` wrapper. Matching release: 432 library tests, three integration tests and strict Clippy (`vasprintf-build.log`). Native cases cover GP spill transition, empty and embedded-NUL output, allocation growth/free, integer modifiers, `%n` widths and aliases, and guarded publication (`work/vasprintf-native/native.log`). Native allocation-failure and floating paths are not verified. Startup binds this service and stops at missing `strcoll` with 19,280 dependency bytes (`vasprintf-entry-integrated.log`); its overall result still fails Metal presentation. Wide, floating, positional and extended80 output remain outside this registered checkpoint. Source policy comes from FreeBSD 11.4 and the pinned executable, not measurements of console libc. This is not complete printf compatibility.

The output queue keeps guest spans deferred until flush, batches eight vectors and splits padding into sixteen-byte chunks. A failed sink call permanently prevents replay. The buffer distinguishes logical capacity from allocator rounding, frees old storage on growth allocation failure, validates private-storage aliases, publishes NUL before the final pointer and does not touch errno on success. Its poisoned state prevents swallowed append errors from turning into success.

The combined checkpoint passes all 417 library tests, three integration tests and strict all-target Clippy (`printf-checkpoint.log`, 95754). Those foundation checks preceded the integer/text native checkpoint above.

Binary64 f/e/g numerical verification additionally includes 25,032 nearest-even comparisons with Darwin snprintf and 200,064 independent exact-rational e/g cases across four rounding modes (`work/printf-float-audit/`). Those comparisons do not establish console libc fidelity or FP exception effects. Hexadecimal a/A has separate source-grounded unit checks; extended80 formatting and guest FP effects remain open.

## Executable evidence

Pinned `crispy-doom.elf` SHA256: `308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567`.

The audit disassembly has one direct reference to GOT `0x26fac0`: `asprintf_l`, entry `0x115fd0`, calls it at `0x11604d`. The wrapper's ABI is `(strp, locale, format, ...)`. It discards the locale, moves format into RSI and the address of its local va_list into RDX. Thus the imported service is ordinary `vasprintf(strp, format, ap)`.

The wrapper builds the 24-byte SysV argument descriptor at RBP-32: GP offset 24, FP offset 48, overflow pointer RBP+16, register-save pointer RBP-0xd0. RCX, R8 and R9 are saved before formatting. With nonzero AL it also saves XMM0 through XMM7. Native coverage must include this real wrapper, not only an adapter that constructs its own descriptor.

## Source policy and allocation order

[FreeBSD vasprintf.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vasprintf.c) allocates 128 bytes before calling the formatter, reserving one byte for NUL. Success terminates the buffer, publishes its pointer through `strp`, and returns the signed int byte count excluding NUL. Initial allocation failure sets `*strp=NULL`, then errno=ENOMEM, then returns -1. Formatter failure frees its allocation and performs the same failure publication. It does not shrink successful output to its exact length.

[FreeBSD fvwrite.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/fvwrite.c) grows allocating string streams when the next output chunk exceeds remaining capacity. With `blen` bytes already written and incoming chunk length `len`, its requested allocation becomes `blen + len + 129`. It uses reallocf, so allocation failure releases the old block. This is a chunk-sensitive policy, not geometric growth or a single exact-size final allocation.

Existing `libc_memory::realloc` preserves the old allocation on failure and may temporarily need both old and new blocks under the owned-memory quota. A vasprintf adapter must explicitly release old private storage on reallocf failure. Its allocation limitations must remain documented rather than represented as console allocator fidelity.

## Argument consumption and formatter refactor

[FreeBSD vfprintf.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vfprintf.c) saves a separate original argument-list copy for positional lookup, but ordinary sequential GETARG uses `va_arg` on the supplied list. On the target array-based SysV ABI this advances the caller-visible descriptor. Reading a detached snapshot and leaving every cursor unchanged does not reproduce that path. Positional formatting uses an argument table and requires a separate cursor-policy audit; do not extrapolate sequential advancement to positional conversions.

Current `format::formatted(mode=2)` reads the 24-byte descriptor, validates GP offset 0..48 in multiples of eight, and reads GP slots before overflow slots. It never publishes cursor changes, ignores FP offset and cannot select argument classes. Reuse its layout knowledge, not its whole output operation.

Refactor into a typed argument cursor and an event-producing formatter. The cursor should carry observed descriptor bytes, checked GP/FP/overflow advances, and source-ordered writes. Integer arguments consume eight-byte GP or overflow slots. Floating arguments require the SysV FP-save area and its 16-byte register slots. Long-double memory alignment and payload must be verified against the target ABI before implementation.

Output events must preserve literals, signs/prefixes, padding and conversion chunks sufficiently to reproduce allocation growth and argument-consumption timing. A completed Vec from `format::render` loses that event ordering. Do not implement vasprintf with two vsnprintf calls: that rereads sources and arguments and changes allocation/consumption order.

## Output, failure and alias guards

Before mutation, validate live return frames, output-slot width and relevant writable cursor fields. Keep the result allocation private until final pointer publication. On unsupported format, malformed descriptors, guarded-memory failure or runtime-budget exhaustion, release unpublished storage and produce an explicit runtime diagnostic; do not manufacture ENOMEM for an unimplemented conversion.

Guest allocator exhaustion is a real ENOMEM path. Validate both `strp` and errno destinations before publishing failure; perform the pointer-zero write before the errno write. In particular, errno overlapping `strp` must follow that order rather than assuming the two outputs are disjoint. Success must not require writable errno or overwrite it.

Format strings, string arguments and the va_list may overlap the pointer-output slot. Final pointer publication follows their consumption. Cursor changes may also affect later reads when buffers overlap. `%n` adds ordered guest writes during formatting, so a blanket final snapshot commit is not sufficient for the full formatter. Initially rejecting unresolved overlap cases is an explicit compatibility boundary, not equivalent behavior. Distinguish malformed/unmapped inputs from allocator failure.

## Existing surface and missing prerequisites

`format.rs` currently supports bounded `%d/%i/%u/%o/%x/%X/%p/%s/%c/%%`, integer length modifiers, flags, numeric/star width and precision. Its shared limit is 65,536 bytes; unsupported syntax and budget exhaustion currently share `Fault::Limit`. Split those error categories before exposing this service, otherwise unsupported formats could be mistaken for libc failure.

There is no reusable printf float-output implementation in the inspected Rust source or dependencies. `float_parse.rs` and its submodules implement text-to-binary parsing and source-derived parsing effects; they are not dtoa. Scalar FP modules provide arithmetic/rounding building blocks, not printf text generation. Rust host Display alone is not a verified replacement for target libc formatting.

Remaining formatter work includes:

- Binary-to-decimal conversion for `%f/%e/%g` and uppercase variants, exact precision/exponent selection, rounding modes, signed zero, infinities and NaNs.
- Hexadecimal `%a/%A`, typed FP argument fetching, and audited long-double ABI/output.
- `%lc/%ls` with the guest wchar16/C-byte conversion policy and conversion-failure ordering.
- Positional arguments and positional star widths/precisions, with argument-table type consistency.
- Ordered `%n` writes for all supported length modifiers and alias/return-frame protection.
- Source-audited extension specifiers and locale/grouping behavior as required by real callers; current C-locale policy cannot stand in for arbitrary locales.
- Independent formatting oracles, bounded working-memory accounting and correctly classified limits.

## Proposed work split and acceptance

1. Formatter agent: typed va_list cursor, typed errors and output events; preserve existing sprintf/snprintf tests while adding sequential cursor and register/overflow tests.
2. Allocator/service agent: private allocation guard, source growth policy, pointer publication, allocator-failure cleanup and executor return guards. Depends on stable event/cursor interfaces.
3. FP agent: independent dtoa/hex-output implementation and target rounding/ABI audit. Can proceed in parallel; integrate only when typed argument/events interfaces stabilize.
4. Verification agent: independent integer/FP references, allocation-failure injection, alias matrices and native wrapper fixtures. Keep native runs on a matching frozen release archive.

Acceptance must cover empty output, embedded NUL from `%c`, register-to-overflow transitions, long outputs across growth boundaries, exact cursor updates, free/realloc ownership, initial and growth allocation failure, ignored errno on success, failure output ordering, guarded sources/outputs and actual `asprintf_l` execution. Unsupported surface must remain listed until exercised; initialization progress is not game compatibility.

## Concrete Rust design after runtime inspection

This is an implementation proposal, not registered runtime functionality. No existing runtime file was changed for this planning increment.

### Interfaces and borrowing

Use one synchronous `Session<'m, 'a>` holding the exclusive `&'m mut guest_memory::Memory<'a>`, an output-write guard closure, the descriptor address, parser state, output allocation state and operation budget. Do not give independent long-lived mutable Memory borrows to the cursor and sink: fetching arguments, `%n`, deferred source reads and heap growth must share one ordered operation stream. Helpers take a short Memory borrow per operation.

Proposed value types are `ArgClass::{Gp, Binary64, Extended80}`, `ArgValue::{Integer(u64), Binary64(u64), Extended80([u8; 10])}`, and `FormatError::{Unsupported, Malformed, Read, Write, Budget, HostAllocation}`. Guest libc failures are a separate `LibcFailure(errno)` result, never inferred from `Unsupported` or `Budget`. Store floating values as guest bits, not host floats. Exact extended80 layout and padding must pass the target ABI fixture before enabling that class.

`Cursor::fetch(memory, class, guard)` loads the currently relevant descriptor fields, prevalidates the actual field update and source bytes, reads the value, and commits that cursor advance before subsequent formatter work. GP slots advance by eight bytes. FP slots advance by sixteen bytes and supply binary64 bits from their low eight bytes; the pinned wrapper saves eight XMM registers. Register exhaustion selects the overflow area. Width and precision stars consume GP arguments independently of the conversion class. Validate range, alignment and checked pointer arithmetic; do not globally require the unused register-save region to be mapped. Descriptor stores must run through the same live-return-frame guard as `%n` and final result publication.

The caller-visible descriptor is not a once-only cached snapshot. Earlier `%n` or permitted aliasing can change later descriptor fields. Source-generated instruction order for descriptor/argument overlap needs an ABI fixture; until resolved, reject that overlap explicitly before the affected fetch. This restriction cannot be hidden as ENOMEM. Positional mode needs a distinct argument-table path using the original copied cursor; do not make sequential cursor behavior the positional contract.

### Streaming output and private allocation

Use output events `GuestSpan(address, length)`, `OwnedSpan(bytes)`, and `Pad(byte, count)`, plus an explicit flush operation. Guest spans retain addresses, not references into Memory: malloc/realloc changes the region vector. Generated digits own their bounded scratch bytes until flush. Keep padding segmented according to the source helper, rather than expanding an arbitrarily large temporary Vec.

The upstream [printfcommon.h](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/printfcommon.h) output-vector helpers must define batching and flush boundaries. Immediate byte-at-a-time emission changes allocation timing; an eagerly copied `%s` changes its interaction with later `%n`. Audit and reproduce vector limits, padding chunk size, conversion flushes and deferred guest span reads before claiming alias equivalence. Single-pass means arguments and conversions execute once; positional discovery and source-required wide-string sizing are separate source-defined work.

Allocation state is `{address, requested_capacity, used, published}`. Start with requested capacity 128 and 127 usable output bytes. Track this logical capacity independently of `heap_allocation_size`: the latter returns the allocator's rounded capacity, which must not postpone libc growth. At flush, each source output vector that exceeds remaining logical capacity requests `used + vector_length + 129`, then updates logical capacity to that exact request. Leave one byte for NUL and never shrink on success.

`guest_memory::Memory` is the active native allocator, not the separate FFI `heap::Heap`. Its `malloc` supplies owned non-executable heap mappings, `heap_allocation_size` accepts exact heap bases, and `free` removes only owned heap allocations. `libc_memory::realloc` already copies, allocates and frees transactionally on success, preserving the old block on allocation failure. Wrap that helper with explicit reallocf semantics: on allocator failure, free the old private block and clear allocation ownership before reporting failure. Preserve its documented temporary old-plus-new quota limitation. Avoid carrying borrowed source slices across any allocation call.

Use an outer cleanup wrapper around the fallible formatting body: every error releases an unpublished allocation through the same live Memory. If cleanup itself fails, return a combined runtime fault and retain ownership information; never silently report a clean ENOMEM while leaving an uncertain mapping. Final success validates the result slot, writes NUL, publishes the pointer, then marks ownership transferred. Failure publication validates both destinations first, frees private storage, writes `*strp = 0`, then writes errno. Read-only/unmapped errno is irrelevant to success.

This is not a whole-call rollback transaction: argument cursor updates and `%n` already executed remain observable if later allocation fails. Runtime guard faults likewise must not claim guest-call completion; preserve executor fault semantics after earlier mutations. All output destinations must receive return-frame checks before their own first mutation, and preflight any known fixed destinations before allocating.

### Completing numeric and extended formats

Integer digit generation can reuse the existing division/sign/precision algorithms after extracting them from the monolithic `render`. Preserve the current renderer for existing services until event-output regressions agree. Add C-byte wide conversion using the already audited wchar16 policy, `%n` stores at the specified integer width, positional typed argument discovery, and explicit locale/extension handling. A full-format parser should identify missing conversions as unsupported rather than route them through integer logic.

There is no reusable dtoa implementation. `float_parse::big::Big` has multiply-add, shift, comparison, subtraction and division with a quotient restricted to fewer than 64 bits. It is private to the parser and uses infallible Vec growth. It is a useful algorithm reference, not a ready formatter dependency. A shared replacement would require bounded/fallible allocation, decimal digit extraction and regression coverage before changing the existing parser.

For new binary64 decimal output, decode the exact magnitude as significand times a power of two. Generate decimal digits with bounded integer arithmetic, round using quotient/remainder and the guest rounding mode, then assemble `%f`, `%e` and `%g` fields. `%g` exponent selection must use the rounded result, including carry into a new exponent; default precision, precision-zero behavior, alternate form and zero stripping are conversion-specific. Hex output can round extracted binary significand nibbles directly. Signed zero, subnormals, tie cases, infinities, NaNs, exponent widths and uppercase variants need independent expected results. Do not use Rust Display as the oracle. Audit source gdtoa/hdtoa and guest floating-exception behavior separately from mathematical correctness; extend to the verified long-double representation only after binary64 is validated.

### Next implementable checkpoints

1. New unregistered typed cursor module with an in-memory trace harness: mixed GP/FP, register exhaustion, stack arguments, descriptor updates, stars, invalid offsets, guarded writes and alias rejection. No service exposed yet.
2. Event parser/output module preserving literal and source vector boundaries. Tests compare byte output and an ordered trace of argument reads, cursor writes, `%n`, allocation requests and flushes. Existing integer tests become regression inputs, not a completion denominator.
3. Independent float-output module and exact rounding oracles. Keep all unsupported conversions visible in a capability matrix until each family passes.
4. Session allocator adapter with deterministic injected initial/growth exhaustion, ownership accounting, logical-versus-rounded capacity checks, ordered failure publication and errno-free success.
5. Register the complete agreed supported surface, then exercise the actual `asprintf_l` wrapper, including its AL/XMM save path and GP spill transition. Add native cursor and allocation observations, not only final text comparisons.

The first four checkpoints can be tested without changing executor registration. Registration remains a separate reviewed checkpoint; neither a typed cursor nor integer-only output warrants claiming vasprintf complete.

## Typed cursor checkpoint

`rust/src/printf_args.rs` implements the sequential cursor and is registered with a guarded guest-memory adapter. Eight standalone cursor tests and two real-memory adapter tests pass. The generic Memory adapter requires whole-span atomic writes and accepts the runtime's return-frame guard per field update. The integer/text service now uses this cursor.

The cursor supports six GP slots, eight binary64 FP slots, shared overflow slots, and raw x87 payload retrieval. It reads only descriptor fields needed by the requested class, reloads them on each fetch, and commits one caller-visible field per successful argument. Budget, malformed ABI data, unsupported descriptor aliases, read faults, write faults and protection faults have distinct errors. No heap allocation or host FP operations occur in argument fetching.

Local audit fixture `work/arm64-prototype/printf-audit/cursor.c` was compiled with Apple Clang 21 using `-target x86_64-unknown-freebsd11.4 -O1 -S`. Its output confirms GP stride8, FP stride16/end176, and long-double overflow alignment16/advance16 with ten-byte payload reads. This is compiler ABI evidence, not execution of the console libc. The compiler advances the descriptor before loading payload; the guarded cursor validates and loads first, then commits. Descriptor/payload overlap is explicitly rejected, making the accepted successful path equivalent while leaving rejected memory faults atomic.

Tests exercise all register slots, mixed classes and overflow, extended payload padding exclusion, unused inaccessible descriptor fields, external cursor mutation between fetches, no mutation on rejected writes/reads/return guards, malformed offsets/alignment/overflow, alias rejection, and retention of earlier successful advances after a later failure. Long-double formatting and positional argument tables remain outstanding.
