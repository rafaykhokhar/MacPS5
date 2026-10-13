# C/POSIX wide collation audit

The source supports reusing the runtime's existing `wide_string::wcscmp` for C/POSIX `wcscoll`. FreeBSD 11.4 `wcscoll_l` first computes `wcscmp(ws1, ws2)` and returns that result immediately when the locale's `__collate_load_error` is set. The C collation object has that flag set; both C and POSIX select that object. No collation-table pass, allocation, encoding conversion or errno write is reached in this branch. [wcscoll.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/wcscoll.c), [collate.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/locale/collate.c).

The guest ABI must stay unsigned 16-bit. Pinned SDK revision `10b539de8f2b6773a9a78caa7576a97dbd99b29a` selects `unsigned short` for `___wchar_t` under `__SCE__`, with range 0 through 0xffff. Existing SDL wide-string code corroborates two-byte stride. These are raw code units; surrogate values and noncharacters are not encoding errors. [SDK x86 scalar types](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/_types.h).

There is a specific source limitation: upstream FreeBSD `wcscmp.c` explicitly assumes `wchar_t = int` and casts the mismatching pointers to `unsigned int *`. Those four-byte loads cannot be reused for this guest ABI. The existing runtime comparator instead subtracts two unsigned16 units promoted to i32, yielding −65535 through 65535. Keeping that exact difference is our declared guest comparator policy and preserves the required ordering; the public source and SDK alone do not establish Sony libc's exact return magnitude. The observed executable wrapper forwards an imported function rather than providing its implementation. [wcscmp.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/wcscmp.c).

Integration can add a distinct `Service::Wcscoll` and route its C/POSIX branch to `wide_string::wcscmp(&memory, rdi, rsi)`, returning the result through low32 EAX as existing `Wcscmp` does. No kernel errno context should be required. Reads stop at the first difference or equal NUL; equal pointer values must still read memory. Unaligned full-width guest pointers are handled by existing guarded two-byte loads. A partial unit, inaccessible needed suffix, address overflow or the existing 65,536-unit scan bound produces an explicit runtime fault, never a fabricated comparison result or errno assignment. Input buffers are read-only and may alias.

The actual pinned forwarding wrapper was located by the native-verifier agent at offset `0x116220` with `ff 25 d2 98 15 00`, targeting import slot `0x26faf8`. Native verification should cover that wrapper, C/POSIX selection, high-unit signedness, prefix ordering, no source/errno/heap changes, early mismatch before inaccessible memory, exact aliases, and partial-word/overflow/scan-bound faults. This audit does not itself report those native checks as passed.

Other locales remain unsupported. If `GuestLocale` later admits non-C collation, dispatch must use the selected table and its associated error behavior instead of continuing to route all locales through this comparator. This is a source-backed profile decision, not console-libc binary equivalence or gameplay proof.

## Implemented verification

`Service::Wcscoll` now shares the guarded unsigned16 comparator. Its matching
release passes 32 native direct/pinned-wrapper cases in C/POSIX locales, including
high/unaligned read-only pointers, signed differences, NUL/surrogate values,
identical inputs, early mismatch, partial words and scan-limit guards. Errno,
source bytes and guest heap remain unchanged. Evidence:
`work/wcscoll-native/native.log` (12966, exit0),
[native verifier](../../rust/verification/wcscoll_native.rs). The integrated
tree passes 502 library tests, three integration tests and strict Clippy.

Startup discovery binds `wcscoll` and reaches missing `wcsxfrm`, retaining 19,488
dependency bytes before normal failure exit (`wcscoll-startup.log`, 81115).
That discovery run failed the previous boundary assertion; the updated startup
assertions are being checked separately. Game main has not executed.
