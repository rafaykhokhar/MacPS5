# Condition variable implementation plan

Not implemented or verified. Source-derived FreeBSD policy is not console-measured behavior.

Use an owned state machine: Waiting → Reacquiring(result) → Returned. Signal or timeout selects a result; the call returns only after mutex reacquisition. Signals are not banked for future waiters.

The pinned SDK represents condition and attribute slots as eight-byte opaque pointers; NULL is the static condition initializer. Calls return int status directly, without errno writes. Default clock is realtime. FreeBSD validates timed-wait timespec before condition/mutex handling, lazily initializes static conditions even for signal/broadcast, and fully releases recursive mutex depth during wait, restoring it on reacquisition.

## Implementation boundaries

- Add guest_cond.rs with bounded objects, waiters, owner-bound single-use tickets and prevalidated commit plans. Existing condition.rs concerns CPU flags.
- Extend guest_mutex.rs with dedicated full-depth detach/reacquire and retained-waiter destruction guards. Ordinary unlock is insufficient for recursive mutexes.
- Reserve and validate continuation storage, live return frames, code and backing ownership before atomically enqueueing and releasing ownership. No fallible step between those mutations.
- Extend executor retention guards for both mutex and condition backing across free/realloc/unmap/image retirement. Reject unsupported borrowed suspension storage.
- Extend scheduler eligibility and deadlines for both waiting and reacquisition phases. Recheck realtime after monotonic wake hints.
- Cancellation requests/delivery remain unsupported. Future cancellation must reacquire before cleanup/unwind; dropping a released wait cannot silently cancel it.

## Required verification

Signal-before-wait, one-waiter signal, broadcast, signal while mutex remains locked, timeout with blocked reacquisition, recursive-depth restoration, transactional invalid-input failures, foreign/replayed identities, backing reuse, two native translated workers, quantum slicing, callback waits and full startup binding.

## Sources

- [Pinned SDK pthread header](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/pthread.h)
- [SDK pthread types](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_pthreadtypes.h)
- [FreeBSD condition implementation](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libthr/thread/thr_cond.c)
- [FreeBSD mutex implementation](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libthr/thread/thr_mutex.c)
- [FreeBSD condition attributes](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libthr/thread/thr_condattr.c)
