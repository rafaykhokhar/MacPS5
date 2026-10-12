# Guest comparator execution

Status: sort/search models and focused native callback execution pass. The matching full startup regression passes: `qsort` and `bsearch` bind, and the next unresolved export is `__stdinp` (`callback-entry.log`). Game main has not executed. The acceptance matrix below is broader than the current native fixtures and must not be read as a list of completed checks.

The isolated model tests can be reproduced from the repository root without a native runtime archive:

```sh
rustc --edition=2024 --test -D warnings rust/verification/compare_models.rs -o /tmp/macps5-compare-models
/tmp/macps5-compare-models
```

These six tests verify model behavior only; they do not establish callback execution or safe runtime memory ownership.

Three independent memory-boundary tests pass with warnings denied in an isolated source snapshot (`callback-memory-review.log`). They cover overlapping writes against two return slots, scalar/copy/atomic write paths, slot retirement and owned-backing identity across mapping rebuilds. The integrated runtime must still demonstrate that it installs and retains these guards on every callback path.

The matching callback snapshot passes 323 unit tests, three integration tests and strict all-target Clippy (`callback-snapshot-tests.log`, `callback-snapshot-clippy.log`). Focused native execution passes the actual SDL wrappers and display-mode comparator, signed integer sorting/search, nested callbacks and a rejected active-array free. Exact comparison logs match uninterrupted, forced-quantum and blocking-sleep runs (`sort-search-native.log`). This does not yet cover every mutex/semaphore tail-return combination or second-worker invalidation listed below.

The startup fixture currently stops at `qsort`. Implementing it requires executing the guest's comparator through the ARM64 translator. `bsearch` requires the same mechanism. A guest address must never be invoked as a Rust or host function pointer.

## Execution contract

Each active operation owns its algorithm state and outstanding request. A comparison result is the signed low 32 bits of the guest return value. Sorting compares addresses of actual array elements; searching passes the key first and the current element second. Sorting uses bounded heapsort; comparator order is not claimed to match FreeBSD quicksort.

Comparator execution uses the existing guest thread, TLS, floating-point state, instruction budget and dispatch budget. Suspending for a quantum, sleep, mutex, semaphore or another service must retain the entire nested callback stack. Returning from a comparator must not reset budgets or replay its side effects. Comparator changes to memory, errno and floating-point state remain visible.

A callback return is valid only for its owning active frame at the expected guest stack position. Ordinary executable returns, process completion and callback completion must remain distinct. Tail calls into host services and later service resumes must validate the correct completion target before acquiring a resource or publishing output.

The outer service's return frame remains live throughout all comparisons. Nested callback frames also remain live. Stack placement must obey the guest ABI's alignment and red-zone rules. Service output and swap guards must protect every live return slot, not just the most recent one.

## Storage and mutation

Check array multiplication and end-address arithmetic before nontrivial execution. Retain owned array backing, the key's containing backing and comparator code while callbacks are active. Free, realloc, unmap and managed module retirement must reject releases overlapping retained storage, including attempts by another worker. The key object need not be as large as an array element.

No borrowed `Memory` object may survive a callback. Reconstruct mappings on resume. Standalone hosts remain responsible for retaining the exact image backing supplied to the executor; equal bytes at a reused address are insufficient. Arbitrary externally borrowed data is not safe to retain across callback suspension.

A swap is committed only after its request identity, both complete destination ranges, all live return slots and bounded scratch allocation have been checked. Read the records after the preceding comparator finishes. A guard failure must retain the pending swap without rerunning that comparison. A completed request cannot be accepted twice.

The current runtime treats a callback guard failure as a process fault: it retains active frame ownership and rejects further execution. It does not expose a repair-and-retry API for the failed swap. Normal quantum and service suspensions remain resumable; the model's request-retention tests alone do not prove runtime fault recovery.

Trivial sorting of zero or one element, and searching zero elements, requires no array or comparator access. Nontrivial zero-size elements and configured capacity limits currently have explicit unsupported boundaries. Those limits must not be reported as full libc compatibility.

## Required verification

| Case | Evidence required before acceptance |
| --- | --- |
| Ordering and search | Independent ordering/permutation and membership checks, duplicates, odd/wide records, signed comparison extremes |
| Actual guest callbacks | Translated comparators receive the correct guest addresses and produce correct output through actual SDL wrappers |
| Suspension | Forced quanta and blocking services preserve state and comparison side-effect counts |
| Nesting | A comparator calls another sort/search and resumes its parent correctly |
| Tail completion | Comparator tail calls into immediate and suspended services reach only the authenticated parent frame |
| Memory ownership | Comparator and second-worker free/realloc/unmap/module-retirement attempts fail before release |
| Guard failures | Read-only/invalid records and return-frame overlap fail before swap mutation; comparison side effects are not replayed |
| Faults and exit | Consuming resume errors and exit retain required backing; they do not manufacture a successful sort or release live stack storage |
| Budgets | Nested callbacks share limits; inconsistent comparators cannot produce unbounded host work |
| Integration | Full native regression and the pinned startup run pass against the exact release archive |

Passing these checks establishes the stated comparator-service scope. It does not establish game boot, complete CPU behavior, asynchronous signal callbacks or process cleanup.
