# Abort and synchronous SIGABRT

The Rust executor implements synchronous abort with ordinary translated handlers and a distinct `SignalTerminated` outcome. Matching release build 92846 passes seven native cases in `abort-native.log` (runner 55763): default, ignored, returning handler, NODEFER, RESETHAND with nested abort, two nested caught handlers, and handler-requested normal exit. Every case initially blocks SIGABRT and reaches the actual pinned homebrew abort call followed by UD2; execution terminates without executing that UD2. Handler cases execute real translated instructions with quantum 3. FreeBSD source policy is not measured Sony libc behavior.

The integrated library suite passes 375 tests and three video model integration tests. Unit checks cover saved modeled CPU/FP/vector restoration and exact stack retention when a suspended handler is dropped; those checks alone do not establish native FP restoration. Scheduler and module regressions cover termination latching, blocked worker admission, retained pending initializers and rejection of further execution. Process teardown, general asynchronous signal delivery, SA_SIGINFO, alternate stacks and context-changing returns remain unfinished. The current CPU model also lacks complete architectural x87 state; see [signal frame requirements](SIGNAL_FRAMES.md).

## Sequencing and ownership

`guest_abort` models cleanup pending → first raise → caught handler running or forced default → second raise → signal termination. Default disposition terminates on the first raise; ignored disposition is reset to default and raised again. Initially blocked SIGABRT is explicitly unblocked. An ordinary caught handler must execute actual translated instructions before its return triggers default disposition and the second raise. No `exit(-1)`, normal exit code, invented handler success, shell status134, or invented core dump substitutes for SIGABRT6.

`guest_signal_action::Snapshot` captures process identity, revision and disposition. Its exclusive commit permit validates a snapshot and optional replacement before publishing any action change. `guest_abort` exposes exclusive phase permits and process-local/thread-bound handler-return tickets. These authenticate model sequencing only: the executor must separately authenticate guest frames, validate the owning signal Store snapshot, reserve continuation capacity, preserve thread identity, and publish TLS masks/dispositions/phase together.

The abort mask blocks every maskable signal except SIGABRT. Existing libthr-visible filtering removes SIGKILL9, SIGSTOP17 and SIGCANCEL32. Handler delivery adds its action mask and SIGABRT unless SA_NODEFER; SA_RESETHAND resets disposition before handler entry. `Snapshot::reset_for_delivery` represents the libthr/kernel automatic reset: default handler, original flags minus NODEFER/SIGINFO, and kernel all-signal catchmask with unmaskable bits removed. This differs from abort’s later explicit flags-zero reset. A returned handler restores the **abort-adjusted mask**, not the original pre-abort mask, before forced default/re-raise. Explicit SIGABRT in the action mask still blocks it even with SA_NODEFER.

The model supports ordinary-handler flag decisions for SA_RESTART, SA_RESETHAND and SA_NODEFER. It rejects other caught-handler flags; it does not silently discard SA_SIGINFO or SA_ONSTACK.

## Stdio cleanup preflight

FreeBSD abort invokes `__cleanup` before its first raise. `findfp.c::_cleanup` walks `__sflush`, rather than invoking fclose or atexit. Current accepted writes to owned stderr and ordinary FILE streams commit immediately; no buffered output is waiting to drain. A cleanup helper must validate every open owned FILE's supported metadata, configured stderr, and **configured stdin separately**: existing `FileStreams::flush(0)` only traverses ordinary streams. Modified or unsupported metadata is a boundary, not evidence of a successful flush. Do not require an unconfigured stderr FILE. Captured puts/stdout output has no modeled stdout FILE to flush. Keep captures, descriptors and files open; this preflight does not prove general libc finalization or host teardown.

## Implementation and remaining gates

1. Implemented: distinct signal termination retains the no-return call context and stops scheduler/module execution without executing the caller's following UD2.
2. Implemented for modeled state: owned abort frames save the current Cpu structure, TLS/thread identity, signal masks and code/stack ownership. This is broader than comparator GPR snapshots but is not complete architectural x87 state.
3. Implemented: handler entry, stack space, return sentinel, backing leases and bounded nesting are checked before delivery commits. Live instruction accounting is retained when saved CPU state is restored.
4. Ordinary translated handler returns, quantum yields, nested abort and handler exit pass native checks. General service waits and callback combinations require further native coverage. Returned handlers restore saved state before default disposition and the second raise; genuine exit requests supersede that path.
5. Implement SDK siginfo/ucontext/mcontext layouts, FP serialization and validated context restoration for SA_SIGINFO. Implement registered alternate-stack ownership, bounds and SS_ONSTACK state for SA_ONSTACK. Context-changing signal returns/longjmp must either execute faithfully or remain explicit unsupported boundaries; never fabricate these effects with a plain Rust callback.
6. Termination retains worker/initializer ownership, and abandoned handler continuations retain exact stack backing and fault frames. Real process teardown remains unfinished. No normal atexit execution or fabricated zero-resource cleanup is reported.

## Verification gates

Expanded native runner 32743 passes nine cases against the same release (`abort-native-waits.log`), adding explicit SIGABRT action-mask handling with NODEFER and a scheduler-driven nanosleep inside the handler. Real guest instructions seed and observe a GPR, XMM0, MXCSR and x87 control word across that wait. This proves preservation across suspension, not native final-handler-return restoration of the interrupted state. An earlier expanded run stopped on unsupported legacy MOVDQU; the wait fixture uses supported VEX VMOVDQU and does not prove legacy instruction support.

The exploratory full entry run 25072 binds abort and reaches missing vasprintf with 19,232 retained dependency bytes but fails the previous expected-import assertion. Updated integrated run 10250 passes all nine abort cases and the startup boundary assertions (`abort-entry-integrated.log`). It still exits unsuccessfully because Metal presentation fails: three dropped attempts in 0.354 seconds. No game main or gameplay is established.

Model checks cover default/ignored decisions, blocked-mask transformation, caught-handler return ticket replay/foreign/thread rejection, reset-before-entry and NODEFER masks, dropped/stale signal transactions, unsupported flags and phase ordering. These do not execute guest handlers.

Required native coverage: default, ignored, initially blocked SIGABRT, handler entry with argument6, handler return → second raise, handler disposition changes, SA_RESETHAND, NODEFER/action-mask interaction, mask/FP restoration, quantum/service suspension, nested abort, handler exit, return-frame and code lifetime failures, configured stdin cleanup validation, scheduler-wide stop and retained ownership. SA_SIGINFO/alternate stack/context-changing return gates remain open.

## Pinned homebrew evidence

`crispy-doom.elf` has36 calls through abort GOT0x26fab0. Convenient no-return branches are wchar_to_loop_convert0x1027cf and wchar_from_loop_convert0x102baf, each immediately followed by UD2. Other callsites include __assert0x1172e4/0x117301 and png_longjmp0x145d4d. These branches are native test candidates, not proof that any game executes.

## Sources

- [FreeBSD11.4 abort.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdlib/abort.c)
- [Current-thread raise.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/raise.c)
- [Stdio findfp.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdio/findfp.c)
- [Libc sigprocmask interposition](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/sys/sigprocmask.c)
- [Kernel signal reset implementation](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/kern_sig.c)
- [Libthr signal implementation](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libthr/thread/thr_sig.c)
- [Pinned SDK signal ABI](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/signal.h)
- [Pinned amd64 mcontext ABI](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/ucontext.h)
- [Pinned ucontext ABI](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_ucontext.h)
