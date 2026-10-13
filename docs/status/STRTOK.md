# Reentrant byte tokenization

`Service::StrtokR` uses an owned Rust plan for FreeBSD 11.4
[`strtok_r` semantics](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/string/strtok.c).
It skips leading separators, returns a pointer into the original guest buffer,
replaces a token-ending separator with NUL, and saves the following address.
At source NUL the saved pointer becomes NULL. Delimiters may change between
calls. A fresh non-NULL input does not read the old saved pointer.

NULL input with a NULL saved pointer returns NULL without touching delimiters
or requiring writable saved-pointer storage. A final token without a separator
does not write its source bytes. Bytes are unsigned; no locale decoding, errno
update or guest allocation occurs.

The helper bounds source and delimiter scans to 65,536 bytes each. It lazily
caches delimiter membership, so total scan work is linear and it does not read
beyond the delimiter prefix needed for a match. Both output ranges are checked
against mapping permissions and live return frames before either write commits.
Guard/budget failures leave guest bytes unchanged. Actual restrict-qualified
write/read overlaps are explicit unsupported cases.

Six focused helper tests cover token chains, changed/empty delimiters, high
bytes, NULL continuation, read-only final tokens, bounds and atomic failures.
The integrated build passes 517 library tests, three integration tests, strict
Clippy and release compilation (`work/strtok-build.log`, session71413 exit0).
These helper checks alone do not establish guest wrapper execution or gameplay.

Matching-release native verification also passes direct calls and the actual
`SDL_strtokr_REAL` wrapper at `0x1166e0`, GOT `0x26fb20`
(`work/strtok-native/native.log`, session86020 exit0). It checks continued token
chains with changing delimiters, high bytes, empty sets, saved NULL exhaustion,
exact writes, read-only final tokens and transactional source/delimiter/save-slot,
frame, overlap and scan-bound failures. No kernel/errno context is required.
The verifier is registered in the full entry runner.

Startup-only assertions verify the actual import slot and retained ownership at
missing `printf`, with 19,664 dependency bytes
(`work/arm64-prototype/strtok-verified.log`, session38168 exit0). Component and
Metal checks are skipped in that run; game main has not executed.
