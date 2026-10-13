# Stdout integration gaps

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
