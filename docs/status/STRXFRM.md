# C/POSIX strxfrm

The Rust service implements FreeBSD 11.4's C/POSIX collation fallback to
`strlcpy`. It returns the complete source byte length, and for nonzero capacity
copies at most capacity minus one bytes followed by NUL. A zero-capacity query
ignores the destination but still reads the complete source through NUL.

Primary source: [strxfrm.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strxfrm.c),
[strlcpy.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strlcpy.c).

Input scanning is bounded to 65,536 bytes. The service guards the actual output
extent rather than requiring the whole capacity to be mapped, supports full64
pointers/capacities/results, and leaves errno and guest allocation unchanged.
Overlapping source/output is outside the restrict contract and explicitly
rejected. Malformed input, output permissions and live return-frame conflicts
stop before publishing output. Non-C collation weights remain unsupported.

Four focused model tests and the integrated 501-library/three-integration test
suite pass, together with strict all-target Clippy. The native verifier uses
the pinned `strxfrm_l` wrapper at `0x116210`, GOT `0x26faf0`. Its matching-release
run passes 140 cases for direct and actual-wrapper calls in C/POSIX locales,
including truncation, high bytes, zero capacity, full returned lengths and
guarded outputs (`work/strxfrm-native/native.log`, 23957 exit0).
The service and verifier are registered for the next startup probe; that probe
has not yet established the next unresolved import.
