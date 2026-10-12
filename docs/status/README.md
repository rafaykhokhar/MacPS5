# Progress definitions

This map deliberately tracks a small, explicit checkpoint list. It does **not** measure how much engineering remains. Equal segment sizes do not mean equal effort, and completed fixtures do not establish general compatibility.

| Track | Checkpoint 1 | Checkpoint 2 | Checkpoint 3 |
| --- | --- | --- | --- |
| CPU | Selected instruction decoding: verified | Selected generated ARM64 execution: verified | Complete pinned tiler worker: unverified |
| Runtime | Owned memory/module models: verified | Selected translated service calls: verified | Guest pthread scheduler: unverified |
| Apple GPU | Metal synthetic rendering: verified | Synthetic VideoOut presentation: verified | Frame produced by real guest game loop: unverified |
| PS5 games | Retail game boots: unverified | Retail gameplay: unverified | Tekken 8 playable: unverified |

The CPU and runtime checkpoints cover subsets, not complete instruction sets or system libraries. The 16 GB budget and native-level performance goals remain unverified in games. Badges are checked-in status assets, not live CI results. Update them only alongside supporting evidence and this table.

See ../../rust/README.md for the development record. Historical logs referenced there exist in the original development workspace; the repository contains source/verifiers, not an independently reproduced copy of every log.
