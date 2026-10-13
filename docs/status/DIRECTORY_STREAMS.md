# Directory streams

`Opendir`, `Readdir` and `Closedir` are now implemented and registered in the
startup provider. Startup-only verification passes (64758 exit0,
`work/arm64-prototype/directory-entry-verified.log`): all three named imports
are bound, all ten named dependencies are accepted, and the next missing import
is `putenv`. It retains 20,208 owned dependency bytes; cleanup remains pending
and game main has not executed.

The owned stream profile allocates a protected 64-byte opaque handle and
1024-byte batch buffer from guest heap storage. These sizes are prototype
choices, not a claim about the console's private DIR layout. Refills advance
the descriptor cursor only after validating and copying a whole batch. Cached
records remain readable after an external fd close; the next refill reports
EBADF. EOF preserves errno. Close frees stream storage before closing its fd,
and generation checks protect a replacement descriptor when the old one was
closed externally. Errno output is guarded before teardown when needed.

The integrated build passes 589 library tests, three integration tests, strict
Clippy and release compilation (13051 exit0;
`work/directory-streams-{tests,clippy,release}.log`). Native verification passes
(12407 exit0, `work/directory-native/native.log`): 102 records over repeated
batch refills, exact names and boundaries, EOF/errno, runtime-heap cleanup,
open errors, cached data after external fd close and stale-close protection.
The verifier confirms import names/addresses before testing direct translated
calls; it does not claim to execute a retail-game directory caller.

Raw directory syscalls, fdopendir, rewinddir/telldir/seekdir services, locking,
detached-directory refills, and mutation-tolerant directory cookies remain
incomplete. The following sections record the prerequisites and their tests.

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
At that checkpoint DIR lifecycle and native verification were still pending. Existing namespace
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
syscalls report explicit runtime limitations before guest side effects.
Guest-visible buffers and DIR lifecycle were pending at that checkpoint;
detached-directory metadata remains incomplete.

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

The internal read layer now uses bounded batches of complete dirent records with an internal
entry-index cursor. This is a virtual filesystem policy, not a disk offset or
public telldir cookie. A read plan must validate namespace ownership/revision,
descriptor generation and cursor revision before commit. Namespace mutation
during an active iteration remains an explicit unsupported case until rewind;
this avoids silently reinterpreting an index in a changed sorted listing.

The pinned SDK exposes DIR as opaque in its
[public directory API](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/dirent.h).
FreeBSD's [readdir implementation](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/readdir.c)
returns a pointer within a retained batch and refills it as needed. The stream
layer now connects these batches to guest-visible pointers; an internal batch
alone was not a working readdir service.

Read plans are capped at 1 MiB and never split a record. A too-small buffer for
the next record gives an explicit runtime diagnostic. Copy validates plan and
guest memory before advancing; failed output access leaves the plan available
for retry. Successful commits, including empty EOF, reject later replay.
Rewind invalidates pending plans and permits a fresh namespace revision. These
APIs do not publish errno or public directory cookies; a future guest bridge
must also protect its live return frame and any companion output fields.

Verification: 584 library tests, three integration tests and strict all-target
Clippy pass (33806 exit0; `work/directory-batches-{tests,clippy}.log`). Coverage
includes record boundaries, namespace mutation, competing plans, rewind,
descriptor reuse, repeated EOF and failed guest copies. No service export or
new native startup claim is included.
