# C/POSIX collation

`guest_strcoll::compare_c` implements the current runtime's C/POSIX profile. FreeBSD 11.4 `strcoll_l` falls back to `strcmp` when the collate object's `__collate_load_error` is set; the C collate object sets this flag. The implementation preserves unsigned-byte differences as a low-32-bit C int, reads only through the first difference or NUL, and uses the existing 65,536-byte runtime limit. Invalid memory and exhausted runtime limits remain explicit faults.

Source references: [strcoll.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strcoll.c), [collate.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/locale/collate.c). This is source-derived behavior, not a claim about arbitrary console locale tables.

Three model tests cover all 65,536 unsigned-byte pairs, aliases, read ordering, address overflow and the scan limit. The matching integrated build passes 441 library tests, three integration tests and strict Clippy (`work/arm64-prototype/wide-strcoll-build.log`).

The native verifier executes the pinned executable's actual `strcoll_l` tail wrapper at `0x1161b0`, using its import slot at `0x26fad0`. Checks pass for C/POSIX selection, unsigned bytes, prefixes, full-width addresses, unchanged source/errno/heap state, early mismatch before inaccessible memory, and faults for equal inaccessible suffixes (`work/strcoll-native/native.log`, session 99268). These checks establish the stated wrapper behavior; they do not establish game execution or non-C collation support.

If the runtime later accepts other collation locales, dispatch must choose their corresponding implementation rather than silently routing them through `compare_c`.

Updated integrated startup passes the binding/state assertions and reaches missing `strftime`, then requests normal exit -1 with 19,296 dependency bytes retained (`work/arm64-prototype/strcoll-entry-integrated.log`, 39042). The full run returns failure because all three Metal presentations are dropped. Game main has not executed.
