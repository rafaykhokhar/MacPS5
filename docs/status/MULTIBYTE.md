# C-locale multibyte conversion

## Restartable byte length

`Service::Mbrlen` delegates to the existing C/POSIX `mbrtowc` plan with no
output pointer. This follows FreeBSD 11.4
[mbrlen.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/locale/mbrlen.c)
and the stateless `none` backend. NULL source returns zero before testing
length; non-NULL source with zero length returns the full64 incomplete sentinel
without a read. Otherwise exactly one byte is read: NUL returns zero and any
other value returns one. No state, errno or guest allocation is changed.
Future stateful locales require separate implicit mbrlen state.

The matching release passes 4,238 direct and actual pinned `mbrlen_l` wrapper
checks (offset `0x1162e0`, GOT `0x26fb08`), covering all byte values, huge counts,
NULL/zero-length precedence, exact one-byte bounds, ignored invalid/read-only
state, unchanged errno/heap and operation without a kernel errno context
(`work/mbrlen-native/native.log`, session25397 exit0). The verifier is registered
in the full entry runner. This is C/POSIX profile evidence, not UTF-8 support or
Sony binary equivalence.

The build passes 507 library tests, three integration tests, strict all-target
Clippy and release compilation (`work/mbrlen-build.log`, session76550 exit0).
Release archive SHA-256:
`6d8ed05768346ccfc27cc4184fa0b4e3a153fbeaaf780db5cb4a13314a8f0bf7`.

Startup-only verification checks the actual bound `mbrlen` GOT slot, reaches
missing `mbtowc`, and passes ownership assertions with 19,568 retained dependency
bytes (`work/arm64-prototype/mbrlen-verified.log`, session60640 exit0).
The run explicitly skips component/Metal checks. Game main has not executed.

## Earlier conversion checkpoint

The local Rust implementation covers mbrtowc, wcrtomb and mbsrtowcs under the existing C/POSIX-only locale policy. The matching build passes 363 unit tests, three integration tests and strict Clippy. Native verification passes all 256 byte values, wchar16 narrowing, full-width counts/results, reset/state behavior, source-pointer updates and memory/errno guards through the three actual pinned forwarding wrappers (`multibyte-native-run.log`, release 4611). Startup binds mbrtowc and reaches missing abort with 19,200 owned dependency bytes (multibyte-entry.log). The overall run fails because Metal presentation is not confirmed; cleanup and game main remain unverified. This is not UTF-8 support or measured Sony libc behavior.

The pinned SDK uses unsigned 16-bit wchar_t and a 128-byte, eight-byte-aligned mbstate_t. FreeBSD's C-locale backend is stateless and does not access the state pointer at all. The implementation therefore leaves state untouched, including on reset and errors.

- mbrtowc maps one unsigned byte to a wide unit. NULL source resets with return zero; otherwise zero capacity returns the full 64-bit incomplete sentinel without accessing input or output.
- wcrtomb maps a low-16-bit unit through 255 to one byte. NULL destination returns one; an unrepresentable unit returns the full 64-bit error sentinel and EILSEQ 86 without accessing destination.
- mbsrtowcs supports sizing, bounded conversion and source-pointer updates. Sizing ignores output capacity and preserves the source-pointer slot. Consuming NUL writes a wide terminator and clears the source pointer. Capacity exhaustion advances it by consumed bytes.

Actual output ranges and any source-pointer update are preflighted against mapping permissions and live return frames. Success does not require errno storage. Guard failures and the 65,536-unit scan budget reject transactionally. Restrict-violating overlaps are explicitly unsupported; this is not a claim to reproduce partial native faults or undefined overlapping-buffer behavior.

Policy and license provenance are recorded in [guest_multibyte.rs](../../rust/src/guest_multibyte.rs) and [NOTICE.md](../../NOTICE.md). Native acceptance must cover the ELF's actual forwarding wrappers and exact ABI outputs before startup compatibility is claimed.
