# Directory stream prerequisites

The current startup blocker is `opendir`. There is no registered opendir,
readdir or closedir service yet; startup has not advanced past this import.

`Files::directory_entries` now enumerates the owned namespace, returning dot
and dot-dot entries followed by byte-sorted immediate children. Inodes match
the existing namespace identities. Path traversal uses existing search checks;
the final directory requires read access. A readable directory need not be
writable or searchable to enumerate its own names. Missing paths, regular
files and denied traversal/read access remain errors.

`DirectoryEntry::record` serializes the guest layout explicitly, including
four-byte padding and NUL termination. The layout is verified against the
[pinned payload SDK header](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/dirent.h)
and [FreeBSD 11.4](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/sys/dirent.h).
It does not use macOS's host dirent layout.

This is an owned enumeration snapshot, not an open directory handle. It does
not establish stream behavior across rename/unlink or concurrent mutations.
Next requirements are descriptor ownership and retirement, bounded cursor/read
plans, guarded guest copies, DIR allocation/lifecycle, errno behavior and native
caller verification before adding the service exports. Existing namespace
limits, virtual uid/gid policy and lack of host mounting remain in force.

Verification: 572 library tests, three integration tests and strict all-target
Clippy pass (25001 exit0; `work/directory-enumeration-{tests,clippy}.log`). New
tests cover immediate children, inode identity, root dot entries, permissions,
path errors, owned snapshot behavior and every supported name length's wire
encoding. No new native startup or gameplay claim is made by this checkpoint.
