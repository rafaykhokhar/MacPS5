# Stdout integration gaps

## Rendering target and fixed temporary buffer

The existing printf renderer now submits borrowed batches through a generic
target. Its allocating Buffer adapter and public vasprintf entry points retain
their previous behavior. Batch submission preserves deferred guest reads,
eight-vector flushing, padding boundaries, logical `%n` counts and poisoning
after failure. Hexadecimal fields use the same batch path. Private-range
snapshots preserve allocation-padding guards without adding host allocations
to the Buffer adapter.

The registered `printf_stream_buffer` model implements the 1,024-byte temporary
buffer used by the source's unbuffered formatted-output path. An exact fill of
a partially filled buffer remains pending; exceeding the remaining space
flushes it. An empty buffer sends full-size chunks directly. Short writes
advance only by accepted bytes; zero/error results stop later writes. Render
failure discards pending bytes while retaining already emitted prefixes.
It does not allocate guest output memory or flush on Drop.

The combined build passes 539 library tests, three integration tests, strict
Clippy and release compilation (`work/printf-target-build.log`, 34567 exit0).
Matching native integer/text/wide and hexadecimal asprintf-wrapper regressions
pass (`work/printf-target-native/native.log`, 6839 exit0), including cursor,
allocation/cleanup, `%n` alias, FP register/stack argument and rounding effects.
The fixed buffer has boundary/chunk-trace and short-write/failure-injection
tests. The stream target and guest printf services are still being connected;
this checkpoint does not claim that printf works in guest startup.

## Owned byte-output checkpoint

The Rust executor now owns a writable stdout FILE at `0x7fff00001400` and
writable pointer cell at `0x7fff00001600`, plus a separate state model. This is
an explicit unbuffered fd1 capture profile, not a claim to reproduce default
terminal-dependent buffering. Redirected streams, buffered/callback state,
wide orientation and general FILE locking remain unsupported.

`Fputc`, `Putchar` and `Swbuf` services produce unsigned-byte return values and
share capture ordering with Puts. Swbuf accepts the legitimate inline `_w=-1`
entry state. Byte output updates orientation and the one-byte buffer; bulk Puts
preserves buffer content. Flush validates the quiescent unbuffered state, and
abort includes stdout cleanup validation when capture is configured.

An exclusive validated `Memory::borrow_write` lends the actual guest FILE bytes
to an owner/revision-checked commit permit. Captured output either fails without
changing bytes or succeeds before an infallible metadata/model commit. Ordinary
write protection and runtime-owner barriers apply. Capture-capacity/allocation
failures remain diagnostics; backend errno is not invented from policy limits.

Matching-release native checks pass direct byte calls, the pinned banner's
actual inline decrement/Swbuf byte sequence, Puts ordering, metadata, flush,
writable-pointer persistence and explicit redirected-stream rejection, corrupt
metadata guards and capture-capacity atomicity
(`work/stdout-byte-native/native.log`, session48634 exit0). Existing native Puts
and abort regressions pass (`work/stdout-byte-native/regressions.log`, 55154
exit0). The build passes 527 library tests, three integration tests, strict
Clippy and release compilation (`work/stdout-byte-build-fixed.log`, 20946 exit0).
An initial Clippy failure used an API newer than the declared MSRV; it was
replaced before this passing build. The compiler used is still Rust1.99, not
an actual Rust1.85 build.

The full entry runner includes the byte verifier. The native startup export
table now advertises `__stdoutp` as a writable pointer object and putc, fputc,
putchar and __swbuf as services. Startup still stops earlier at missing `printf`,
so this does not prove those later imports were resolved or executed. Updated
startup-only ownership assertions pass with 19,856 dependency bytes
(`work/arm64-prototype/printf-target-verified.log`, 52403 exit0). No gameplay
or new Metal presentation result is established by this checkpoint.

## Remaining formatted-stream integration

Startup currently encounters the missing `printf` import. Existing captured
stdout and stderr-only formatting do not yet provide the complete stdout FILE
ABI needed by the pinned executable.

The `__stdoutp` import at GOT `0x26fb30` is a pointer cell containing a guest
FILE address. `I_PrintBanner` performs the extra dereference at `0x1174b1`, then
decrements FILE `_w` (offset12) at `0x1174b7–0x1174bd` before calling `__swbuf`
at `0x1174dd` through GOT `0x26fb40`. An unbuffered FILE therefore legitimately
arrives with `_w=-1`. A pristine-metadata-only validator would reject real code.

Required implementation order:

1. Own the stdout FILE and pointer cell, sharing the existing capture ordering.
   Preserve the audited 312-byte FILE layout and valid inline state changes.
2. Implement byte-output transitions for putc/fputc/putchar, plus the distinct
   `__swbuf` entry transition. Include stream orientation, direction errors,
   sticky error, errno, buffer/pointer updates and flush behavior.
3. Integrate stdout into Puts, fflush(NULL) and abort cleanup.
4. Generalize formatted output from the typed argument/rendering machinery to
   stream sinks. Avoid substituting allocating vasprintf followed by a write:
   allocation failures, output timing and alias behavior differ.

`printf` has five available GP variadic argument registers after its format;
`fprintf` has four after stream and format. FP and overflow-stack arguments
also need their proper ABI treatment. Pinned `DEH_printf` calls `vprintf` at
`0x11c46d` with GP offset8, while `DEH_fprintf` calls `vfprintf` at `0x11c670`
with GP offset16; both initialize FP offset48.

Primary behavior references: FreeBSD11.4
[wbuf.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/wbuf.c),
[fputc.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/fputc.c),
[printf.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/printf.c)
and [vprintf.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/vprintf.c).
The unbuffered temporary-buffer formatting path and error propagation need
explicit verification. Decimal/extended formatting, positional arguments,
locale grouping, buffered/callback streams and general FILE locking remain gaps.
This is a read-only source/pinned-code audit, not implemented or verified output.
