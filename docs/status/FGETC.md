# Unbuffered byte input

`fgetc` now reads one byte through the owned FILE/descriptor model. It returns
unsigned bytes (including 255), preserves errno on success and ordinary EOF,
and distinguishes EOF from descriptor errors. FILE and failure-errno guards
run before committing cursor or metadata changes.

The exposed one-byte FILE buffer retains the last byte and advances its pointer
after a successful read. A failed refill resets the pointer while retaining the
byte. Sticky EOF does not refill. The existing fread/fgets paths share this
metadata behavior; seeking resets the pointer and a read-to-write transition
preserves the buffer byte. This remains an explicitly unbuffered profile;
general buffering, pushback and concurrent FILE operations remain incomplete.

Reference behavior comes from FreeBSD 11.4
[refill.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/refill.c)
and [rget.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/rget.c).

The matching build passes 598 library tests, three integration tests, strict
Clippy and release compilation (`work/fgetc-{tests,clippy,release}.log`,
process 41398 exit 0). Native ARM64 byte-input checks pass for regular files
and redirected stdin, exact cursor positions, EOF/error distinctions,
seek/clearerr, modified FILE rejection and errno guards
(`work/fgetc-native/native.log`, process 25141 exit 0).
Existing native FILE and fgets checks also pass
(`work/fgetc-native/regression.log`, process 6085 exit 0).

The native verifier independently checks that relocation 0x26fbd0 names fgetc.
Its generated call fixture verifies service behavior; it does not claim an
original game callsite executed. Startup discovery binds fgetc and stops at
missing fscanf with 20,288 retained dependency bytes. Game main has not run,
and dependency cleanup remains pending.

Updated startup-only binding and ownership assertions pass
(`work/arm64-prototype/fgetc-entry-verified.log`, process 55446 exit 0).
