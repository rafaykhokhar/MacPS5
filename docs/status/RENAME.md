# Guest rename

The matching build passes 565 library tests, three integration tests, strict
all-target Clippy and release compilation (`work/rename-{tests,clippy,release}.log`,
25616 exit0). Model tests cover discarded/stale/foreign plans, replacement
backing lifetime, same/cross-parent link counts, error/no-op lookup ordering and
descendant names growing beyond PATH_MAX after a valid ancestor rename. Kernel
tests cover cwd relocation with relative reopening and atomic rejection of a
replacement that would unlink the cwd identity.

Native verification passes the pinned `M_rename` wrapper at `0x118490` through
GOT `0x26fb58`, source/victim open-descriptor contents and link counts, replacement
inode identity, subtree/cwd movement with relative reopening, errno/path aliases,
and type/permission/nonempty/self-descendant failures. The remove regression also
passes (`work/rename-native/native.log`,94749 exit0).

Startup-only import/ownership assertions pass with rename bound and missing
`strncpy`, retaining 20,016 dependency bytes
(`work/arm64-prototype/rename-entry-verified.log`,30682 exit0). Discovery48377
first exposed the new boundary using the previous checkpoint's expectations.
This proves import binding, not a rename call during startup. Game main and
exit cleanup remain unexecuted.

The guest namespace prepares a complete rename before publishing it. Source
file backing and inode identity survive the move. Replacing a destination
unlinks its old backing while existing descriptors retain its data and quota
charge. Directory moves rewrite descendant names and adjust parent link counts.
Owner/revision checks reject stale plans before mutation.

The kernel snapshots the source pathname and performs source lookup before
reading the destination pathname. Successful rename preserves errno; guest
failures publish errno only through validated memory. No pathname is forwarded
to the host filesystem.

The current working directory is stored as a pathname. A directory rename
containing it prepares a replacement cwd before namespace commit and publishes
both together. Replacing the cwd's directory inode with another inode is an
explicit unsupported diagnostic; retained directory handles are not implemented.
A resulting cwd beyond the current pathname profile is also a runtime limit,
not an invented filesystem errno.

The virtual profile excludes symlinks, hard-link creation, mounts, sticky
directories, ACLs, inode flags, and persistent host saves. This implementation
does not establish general filesystem or game compatibility.
Directory `read_only` is the existing per-node mutation policy, not a mount
model. It also prevents changing a source directory's parent, a stronger rule
than UFS's source-directory write-permission check alone.
