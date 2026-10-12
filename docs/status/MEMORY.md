# Memory capacity and the 16 GB target

The 16 GB unified-memory design target is not yet verified for a game. A guest-visible physical-memory value, an allocation quota, and available host memory are different quantities. System-configuration queries must use an explicit guest hardware profile; they must not infer physical RAM from one allocation pool or claim that the reported capacity is guaranteed allocatable.

## Current accounting domains

| Domain | Current accounting and integration requirement |
| --- | --- |
| Heap, anonymous and mapped direct memory | `guest_memory.rs` applies a combined 256 MiB owned-allocation cap. This is a prototype pool limit, not physical RAM. |
| Direct-memory reservations | `direct_memory.rs` separately limits reserved ranges to 256 MiB. A future shared ledger must distinguish address reservations from backing and avoid charging the same mapped storage twice. |
| Images and translated code | Module image budgets and the executor's 256 MiB retained translation-source cap are separate. Generated code, linker/compiler peaks and loaded code also require accounting. |
| Thread and runtime state | Active, suspended and fault-retained stacks, TLS, service arenas and environment storage have separate lifetimes. Charges must survive suspension and rejected resumes. |
| Files | The shared in-memory content quota is 64 MiB. Live handles retain backing across namespace replacement; temporary read/write plans and replacement copies can add peak memory. Large mounted files should charge resident buffers/cache, not their logical file length. |
| Apple GPU | Metal buffers, textures, drawable/readback resources and duplicate staging allocations contribute to unified-memory use. Aliases sharing backing count once; actual copies count separately. |

## Required shared accounting

Reserve capacity before allocation and attach a charge token to the backing's owner. Release the charge when the last owner releases that backing, including error and cancellation paths. Reserve temporary peaks before copying or reallocating; do not charge only the final retained size. Exhaustion must leave guest-visible state unchanged according to each operation's contract.

The aggregate allocation ceiling must leave measured headroom for the operating system, native runtime, generated code and GPU resources within the target machine's total memory. Guest-reported physical capacity remains a stable hardware-profile value, not a live free-memory counter. Existing per-domain caps remain until replaced by verified accounting; changing a reported value does not remove them.

Completion requires tests across ownership transfers, aliases, failed allocations and cleanup, followed by end-to-end workload measurements on the target Mac. The current implementation has no comprehensive shared ledger and no verified game-memory fit.
