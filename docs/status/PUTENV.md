# Caller-backed environment entries

`Putenv` retains the guest's name=value pointer. Later name/value edits remain
visible to lookup. Replacing a borrowed entry preserves its lookup position;
new entries and replacements of owned entries take priority. `Setenv` never
reuses caller-backed bytes: overwrite zero preserves them and overwrite one
creates owned storage. Inputs are scanned only as needed: putenv stops at the
first equals sign, and getenv returns a matching value pointer without reading
the value. NULL, missing equals and empty names produce EINVAL for putenv.

The source reference is [FreeBSD 11.4 getenv.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdlib/getenv.c).
The model retains its bounded arena and active-entry profile; historical reuse
of inactive owned entries, unsetenv and direct replacement of environ remain
incomplete. Caller storage must remain valid while referenced.

Snapshots capture metadata before guest memory mappings are borrowed. Prepared
actions commit after those mappings are released, with namespace owner,
revision, table and owned-name validation. Failed commits publish no mutation.
Successful setenv/putenv no longer require writable errno storage; failures
still validate errno and return-frame protection before publishing an error.

Integrated verification: 593 library tests, three integration tests, strict
Clippy and release build pass (64416 exit0; `work/putenv-{tests,clippy,release}.log`).
A final test-only budget refinement passes 11 environment tests
(`work/putenv-final-environment-tests.log`). Both native environment verifiers
pass (99753 exit0, `work/putenv-native/native.log`), including borrowed pointer
identity, edits, duplicate lookup ordering, setenv interaction, mapped-end
prefix access and errno guards. The old native guard fixture was corrected to
trigger ENOMEM: successful calls correctly leave protected errno untouched.

Startup-only verification passes (25984 exit0,
`work/arm64-prototype/putenv-entry-verified.log`): named putenv import bound,
all ten dependencies accepted, missing `fgetc` next, 20,240 retained dependency
bytes. Cleanup remains pending and game main has not executed.
