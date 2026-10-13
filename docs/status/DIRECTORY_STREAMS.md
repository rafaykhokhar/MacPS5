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
Remaining requirements include bounded cursor/read plans, guarded guest copies,
DIR allocation/lifecycle, errno behavior and native
caller verification before adding the service exports. Existing namespace
limits, virtual uid/gid policy and lack of host mounting remain in force.

Retained identity is now implemented separately as `DirectoryToken`. It keeps
namespace and node identity across rename without sharing mutable metadata
between prepared namespace plans. Enumeration after opening uses that identity
and does not repeat path permissions. Removed or replaced directories remain
identifiable but reading their detached contents is explicitly unsupported;
recreating the old pathname cannot redirect an existing token.

Internal directory endpoints now use the existing descriptor table and shared
capacity. Their tokens validate table owner and generation, preventing reuse
or another table from reviving an old handle. Live directory endpoints also
retain namespace binding. Close retires the endpoint normally. This is not yet
connected to guest open/opendir: raw directory read/write/stat/seek/truncate
syscalls report explicit runtime limitations before guest side effects. Buffer,
cursor, detached-directory metadata and DIR lifecycle integration remain open.

Verification: 572 library tests, three integration tests and strict all-target
Clippy pass (25001 exit0; `work/directory-enumeration-{tests,clippy}.log`). New
tests cover immediate children, inode identity, root dot entries, permissions,
path errors, owned snapshot behavior and every supported name length's wire
encoding. No new native startup or gameplay claim is made by this checkpoint.

The retained-identity/descriptor checkpoint passes 579 library tests, three
integration tests and strict all-target Clippy (20526 exit0;
`work/directory-handles-{tests,clippy}.log`). Added tests cover discarded/stale
rename plans, permission changes after opening, replacement/recreation,
descriptor reuse/capacity/namespace isolation, and unsupported syscall guards
with normal close. The release archive and native startup have not been rebuilt
for this internal-only checkpoint.
