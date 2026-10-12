# Acknowledgments and provenance

MacPS5 is a separate experimental project inspired by AnyPS5:
https://github.com/boykopovar/AnyPS5

The README structure and progress-map presentation follow that project's style. Its progress numbers, compatibility claims and badges are not reused. The inspected upstream snapshot was f145139; the upstream project is GPL-2.0-only.

Runtime ABI research references the open ps5-payload-dev SDK, FreeBSD 11.4 sources, and SDL's PS5 port. Source comments record relevant paths and revisions. Those projects' copyrights and licenses are not replaced by this project's license.

The semaphore model follows FreeBSD's `sem_new.c` and `kern_umtx.c` behavior. Its source retains the applicable David Xu, Jeffrey Roberson and FreeBSD Foundation notices; see [the semaphore notices](rust/src/guest_semaphore/LICENSE).

Rust dependencies and exact versions are recorded in rust/Cargo.lock and retain their own licenses. Apple SDKs and platform frameworks are not redistributed. Retained legacy implementation files are migration references, not the direction for new work.

The Rust binary-search callback state machine adapts the traversal in FreeBSD 11.4's `lib/libc/stdlib/bsearch.c`. The University of California Regents' 1990/1993 BSD three-clause notice is retained in [guest_search.rs](rust/src/guest_search.rs). The separate heapsort state machine does not reproduce FreeBSD's quicksort implementation or comparator order.
