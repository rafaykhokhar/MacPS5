# Guest strncpy

`Strncpy` preserves full-width pointers and size_t, returns the destination, and
does not touch errno. Zero count accesses neither pointer. For nonzero count it
reads at most that many source bytes, stops at the first NUL, and pads the output
to exactly count bytes. Truncated non-NUL input receives no extra terminator.

The model prepares output before writing, validates the entire destination
including padding, and the executor protects its live return frame before
commit. Invalid-access failures leave output unchanged. This atomic fault policy
is a runtime boundary, not emulation of libc's partial stores on an invalid
address. Overlap between actual source reads and output is rejected explicitly;
operations above 64 KiB are runtime limit errors, with the full count checked
before narrowing. Neither limitation is converted into a fabricated libc errno.

The matching build passes 567 library tests, three integration tests, strict
Clippy and release compilation (`work/strncpy-{tests,clippy,release}.log`,42555
exit0). Initial native verification passed direct cases and guards, but its
payload call-site attribution was wrong: `0x170cd3` targets ftell at GOT
`0x26fb60`. That run (52207 exit0) is not valid evidence of a payload strncpy
call. The invalid-provenance log is retained separately.

Corrected native verification (12554 exit0, `work/strncpy-native/native.log`)
asserts ELF relocation names before patching the GOT: strncpy is `0x26fb70`
and ftell is `0x26fb60`. It decodes the unmodified TXT_StringCopy tail jump at
`0x1c80ad` and checks its target. All 90 direct/tail-jump cases, complete
TXT_StringCopy wrapper cases, atomic guards and narrow-string regression pass.

Startup-only verification (46090 exit0,
`work/arm64-prototype/strncpy-entry-verified.log`) confirms the strncpy import
is bound and all ten named dependencies are accepted. Startup now stops at
missing `sscanf`, retaining 20,064 owned dependency bytes. Cleanup remains
pending; game main has not executed. This is not retail-game compatibility
or CPU completion evidence.
