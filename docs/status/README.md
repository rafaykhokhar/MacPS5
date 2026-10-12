# Progress definitions

This map deliberately tracks a small, explicit checkpoint list. It does **not** measure how much engineering remains. Equal segment sizes do not mean equal effort, and completed fixtures do not establish general compatibility.

| Track | Checkpoint 1 | Checkpoint 2 | Checkpoint 3 |
| --- | --- | --- | --- |
| CPU | Selected instruction decoding: verified | Selected generated ARM64 execution: verified | Complete pinned tiler worker, selected inputs: verified |
| Runtime | Owned memory/module models: verified | Selected translated service calls: verified | Guest pthread scheduler: partial; background-worker module lifetimes pending |
| Apple GPU | Metal synthetic rendering: verified | Synthetic VideoOut presentation: verified | Frame produced by real guest game loop: unverified |
| PS5 games | Retail game boots: unverified | Retail gameplay: unverified | Tekken 8 playable: unverified |

The CPU and runtime checkpoints cover subsets, not complete instruction sets or system libraries. The 16 GB budget and native-level performance goals remain unverified in games. Badges are checked-in status assets, not live CI results. Update them only alongside supporting evidence and this table.

The actual twelve-worker tiler's deterministic test buffer also passes CPU untile and reaches a presented Metal drawable on the M5 Pro (`tiler-gpu-native.log`). All 82,560 pixels supplied to Metal match the source pattern. This is CPU-to-GPU presentation evidence, without GPU readback; the guest-game-frame checkpoint remains unverified.

The tiler checkpoint executes the pinned homebrew's 48-instruction worker on ARM64. Direct checks compare 96 written pixels across three widths and tile boundaries against an independent untile reference, check unchanged pixels, and exercise empty ranges and memory/division failures. A separate check executes the real 242-instruction wrapper: it creates and joins 12 guest workers, and all 82,560 output pixels match the reference. The game loop has not run. Guest create/join/detach and joined-worker module-startup fixtures also pass. The runtime's third checkpoint remains pending because handing module execution back while background workers remain active requires process-wide image lifetime support.

See ../../rust/README.md for the development record. Historical logs referenced there exist in the original development workspace; the repository contains source/verifiers, not an independently reproduced copy of every log.
