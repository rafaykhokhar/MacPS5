# Guest remove

The matching build passes 558 library tests, three integration tests, strict
all-target Clippy and release compilation (`work/remove-{tests,clippy,release}.log`,
72505 exit0). Namespace and kernel-bridge tests cover unlink/recreate identity,
open-descriptor reads, zero link count, quota retention/release, directory link
counts, permission/error precedence and stale-plan rejection.

Matching native checks pass the pinned `M_remove` wrapper at `0x118480`, which
tail-jumps through GOT `0x26fb50`, plus direct service calls. They cover full-width
path pointers, return/errno behavior, old descriptors after unlink/recreate,
directory removal, parent-versus-target permission policy, invalid pointers and
errno/path aliases. The existing M_MakeDirectory/stat/create/read regression also
passes (`work/remove-native/native.log`,56187 exit0).

Startup-only import/ownership assertions pass after binding remove, at missing
`rename`, with 20,000 retained dependency bytes
(`work/arm64-prototype/remove-entry-verified.log`,78606 exit0). Discovery26435
first found the new boundary while using the preceding checkpoint's assertions.
This verifies binding, not that startup called remove. Game main and exit
cleanup remain unexecuted.

`Remove` operates only on the owned guest filesystem. The kernel snapshots the
guest pathname, applies the configured guest working directory, and removes a
file name or empty directory. It never forwards a path to the host filesystem.
Success returns zero without writing errno; failure returns low32 minus one and
publishes guest errno through the normal memory/return-frame guards.

Open descriptors keep their backing, old inode, data, and quota charge after
unlink. Their stat link count becomes zero. Recreating the same pathname creates
a distinct inode; old content is released when its final owner disappears.
Directory removal updates parent link counts and invalidates stale namespace
plans. Read-only file contents do not prevent deletion from a writable parent.

Policy follows FreeBSD11.4 libc `remove.c` (lstat before choosing rmdir/unlink)
and DELETE lookup in `vfs_lookup.c`. Final dot/dotdot spellings are preserved
for rejection rather than accidentally deleting their normalized directory.
Missing intermediate nodes, file trailing slashes, parent permissions and
nonempty directories remain distinct errors.

The existing virtual profile excludes symlinks, hardlinks, mounts, sticky
directories, ACLs and inode flags. Working directories are pathnames, not retained
directory inodes; behavior after removing the cwd by its absolute path remains
incomplete. Persistent host save storage and general filesystem compatibility
are not established by this implementation.
