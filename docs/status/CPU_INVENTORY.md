# Initial CPU form inventory

**This is an audited starting inventory, not the target ISA specification or a CPU completion percentage.** Rows group concrete forms found in the current lowering source; grouped rows must eventually be split into individual encodings, operand variants, control states and defined faults before an exhaustive acceptance claim. Decoder acceptance, successful lowering and independently checked native execution are separate states.

## Scope and reproducibility

Manual source audit: 2026-10-13, repository HEAD `1e7b5a148524733ea2d566b1941930923c84f814`, with concurrent non-CPU work present. Audited `rust/src/aot.rs` SHA-256 `44c8b9c4f436142fa5230dd46335384127be4ecdc10fd46a85962345348b7d29`.

A read-only Rust discovery harness used the existing release archive, SHA-256 `26bc9c3e792f5599220f17a175045e3b9ea3e0bd0ffaafe05e1427647017f7f1`. The archive hash identifies the exact artifact; this audit did not rebuild it or assert byte-for-byte reproduction from HEAD. It decoded and tested lowering acceptance without executing guest ARM64 or opening a window. Strict harness compilation and execution passed (terminal command `364746`, exit 0). Development-workspace evidence: `work/cpu-inventory/discover.rs`, `discovery.log` and `discover`.

Input: pinned Crispy Doom ELF SHA-256 `308fbb17946974f0c2e888988d4f16f98681ea91a8a957a314b3518e5e45a567`; entry `0x40`; discovery budget 65,536 instructions. Result: **53,508 supported discovered instructions and 222 unsupported boundaries**, grouped into 34 form/reason categories. These are static reachability counts, not observed execution counts. Indirect targets are not exhaustively followed, and unsupported boundaries prevent exploration beyond them. Increasing support can expose more gaps.

## State interpretation

- **Lowered**: a code path exists in [aot.rs](../../rust/src/aot.rs), subject to its operand/address/prefix guards. This does not assert correct semantics for every accepted encoding.
- **Focused native evidence**: a specific executable verifier/reference and earlier recorded passing evidence exist. This audit inspects those sources and records, but does not rerun native suites or upgrade their scope.
- **Decode-only boundary**: the decoder identifies an instruction in the pinned ELF, but lowering rejects it. No CPU execution claim follows.

The [decoder](../../rust/src/decode.rs) uses iced-x86 in 64-bit mode and rejects invalid/truncated instructions; it is substantially broader than the implemented translator. `[r/m]` below means the legal register/memory alternatives for the instruction, not every x86 address mode.

## Implemented form families and verification anchors

All rows below are **lowered**, with only the stated focused evidence. [CPU.md](CPU.md) remains the completion-gate document. Historical test totals in [rust/README.md](../../rust/README.md) are checkpoint evidence, not current target coverage.

| Form / operand boundary | Native verifier or reference anchor | Coverage limit / next audit |
| --- | --- | --- |
| `MOV`, `MOVZX`, `MOVSX`, `MOVSXD`; legal scalar 8/16/32/64-bit forms, high-byte registers | [alu_native.rs](../../rust/verification/alu_native.rs), [checks.rs](../../rust/src/checks.rs), ordinary translated callers | Split extension source/destination widths, immediate sign extension, high-byte/REX constraints and fault ordering. Broad mnemonic acceptance alone is not an encoding audit. |
| `MOVBE` memory/register 16/32/64; `LEA`; `NOT` | `checks.rs`; `alu_native.rs` selected word cases | Enumerate each legal direction/address variant and preserved flags; no claim for excluded address modes. |
| `ADD/SUB/CMP/TEST/AND/OR/XOR/INC/DEC/NEG/SBB` scalar 8/16/32/64 | `alu_native.rs` independent NEG/SBB/DEC and word references; `checks.rs` older native fixture generation | Undefined flags must be masked. Not every memory/register/immediate encoding has an individually linked row yet. |
| `ADC` 8/16/32/64, selected register/memory/immediate/alias forms | [adc_native.rs](../../rust/verification/adc_native.rs); `allocator-cpu-native.log`; widened-integer reference | 58,752 recorded cases include carry chains and actual allocator CMP/ADC. Independent software oracle, not x86 hardware differential. |
| `IMUL r/m8/16/32/64` implicit accumulator/full-product | [imul_implicit_native.rs](../../rust/verification/imul_implicit_native.rs) | Full-product register pairing, defined CF/OF and failure preservation; other arithmetic flags undefined. Unsigned implicit `MUL` has no corresponding lowering arm. |
| Two-/three-operand `IMUL` 16/32/64, register destination | `alu_native.rs`, signed widened-product reference | Selected immediate extension, aliases, partial registers and guarded loads. No 8-bit two-/three-operand form claim. |
| `DIV/IDIV r/m16/32/64` | `alu_native.rs`, independent bit-serial divide reference | Recorded 175,008 cases include zero/overflow/no-commit faults. Byte division and architectural guest exception delivery remain gaps. |
| `SHL/SHR/SAR/ROL/ROR` 32/64, legal count operands | [shift_flags.rs](../../rust/src/shift_flags.rs), `checks.rs`, module fixtures | Count masking and zero-count flags tested; 8/16-bit lowering explicitly rejected. Undefined AF/multi-bit OF policy is not architectural defined-state proof. |
| `SHLX/SHRX/SARX/RORX/MULX/ANDN/BEXTR` 32/64 | `checks.rs` generated native fixtures, including BEXTR aliases/memory | `BEXTR` requires VEX; XOP immediate form rejected. Split each encoding and alias combination; legacy fixture harness provenance must remain explicit. |
| `CBW/CWDE/CDQE/CWD/CDQ/CQO` | `alu_native.rs` sign-extension/partial-register reference | Recorded 537,672 cases; unchanged flags and vector state. No implication of general x87 support. |
| All 16 ordinary `Jcc` short/near predicates | [condition.rs](../../rust/src/condition.rs), `checks.rs`, [modules_native.rs](../../rust/verification/modules_native.rs) | Truth tables and selected native branch paths; not LOOP/JCXZ-family or far control transfer. |
| All 16 `CMOVcc` 32/64 and `SETcc` byte forms | `condition.rs`, `checks.rs` | CMOV16 rejected. Need per-form conditional memory-fault/access-order matrix rather than predicate-only coverage. |
| Near direct `JMP`; near indirect `JMP r/m64` | `alu_native.rs` 162 recorded indirect-jump cases; actual `FF 27` and RIP-relative memory | No far jumps; indirect discovery cannot supply all targets. |
| Near `CALL`, `RET` exact `Retnq` without immediate; `PUSH` 64-bit; `POP r64` | Module/executor/native service fixtures; `checks.rs` | These involve dispatcher frame ownership as well as lowering. PUSH16, POP memory/16, RET immediate/far are rejected. |
| `NOP` | Lowering no-op plus ordinary fixtures | Per-encoding prefix/fuel behavior not independently inventoried. |
| `LOCK XADD m32,r32` only | [atomic_xadd_native.rs](../../rust/verification/atomic_xadd_native.rs), allocator/semaphore native evidence | Aligned sequentially consistent helper; host contention test is not concurrent guest execution. Other LOCK forms and misalignment rejected. |
| Legacy `MOVDQU xmm,xmm/m128` and `MOVDQU xmm/m128,xmm` | [movdqu_native.rs](../../rust/verification/movdqu_native.rs); `work/movdqu-review/release-native.log` | Independent byte copies, upper-YMM preservation, both register encodings, unaligned/fault guards. Exact synthetic abort instruction tested; not a pinned-game discovery. |
| VEX128/256 `VMOVAPS/VMOVUPS/VMOVDQA/VMOVDQU` register and memory moves | `checks.rs`; `alu_native.rs` selected DQA/DQU native checks | Alignment for aligned forms; VEX128 zero-upper. This does **not** include `VMOVUPD` despite similar bit-copy behavior. EVEX rejected. |
| VEX128/256 `VXORPS/VXORPD`, register or memory final source | `checks.rs` native vector fixtures | Split full alias/fault/upper-lane matrix. `VPXOR` is a distinct unimplemented mnemonic. |
| `VZEROUPPER/VZEROALL` | `alu_native.rs` vector-state checks | Sixteen modeled YMM registers; not proof of an audited target extended-state profile. |
| Exact `VEX_Vmovsd_xmm_m64` | [vmovsd_native.rs](../../rust/verification/vmovsd_native.rs) | Bit-preserving memory load, upper 192 bits zero, no FP exceptions. Register merge/store forms rejected. |
| Exact `VEX_Vcvtsi2sd_xmm_xmm_rm32` | Lowering source and exact-form rejection unit tests; dedicated native verifier not identified in this audit | Signed32 to binary64 is exact; source1 high-lane merge and upper zeroing. Signed64 form is a concrete pinned-ELF gap below. |
| Exact `VEX_Vcvttsd2si_r32_xmmm64` | `truncate_native.rs` | Selected rounding-independent truncation, exception effects and guarded inputs; r64/legacy/other conversions rejected. |
| Exact `VEX_Vmulsd_xmm_xmm_xmmm64` | [vmulsd_native.rs](../../rust/verification/vmulsd_native.rs) | Selected binary64/control/alias/fault comparisons; not packed or single-precision multiply. |
| Exact `VEX_Vfmadd213sd_xmm_xmm_xmmm64` | [fma213_native.rs](../../rust/verification/fma213_native.rs) | Fused rounding, old-destination upper-lane preservation; other operand-order variants such as 231 remain unimplemented. |
| `LDMXCSR/STMXCSR`, VEX equivalents, 32-bit memory | `truncate_native.rs`, FP verifiers, `checks.rs` | Validate control/reserved-bit and fault behavior; not full architectural exception delivery. |
| `FLDCW m16`, `FNSTCW m16`, `WAIT` | [x87_control_native.rs](../../rust/verification/x87_control_native.rs) | Selected x87 control/pending-state behavior. No x87 register stack arithmetic, full environment/XSAVE or signal-frame restoration coverage. |

Common addressing limits from `Emitter::address`: 64-bit GPR base/index, scale/displacement and RIP-relative forms are emitted. FS/GS, 32-bit base/index and EIP-relative addressing reject explicitly. Guarded runtime memory faults are prototype stop codes, not a complete model of x86 page-fault delivery/restart. LOCK/REP/REPNE reject except the exact LOCK XADD subset; mandatory instruction prefixes decoded as part of an opcode are distinct from repeat semantics.

## Concrete decode-only boundaries in the pinned ELF

These examples were regenerated by the read-only discovery harness, not inferred from mnemonic names. Counts are **static boundary sites**, and bytes/offsets identify one representative of each group. A selected site can be dormant, unreachable at runtime, an intentional failure trap, or misidentified as code by conservative discovery. Prioritize actual execution failures over these counts.

| Boundary forms | Sites | Representative offset / bytes | Missing behavior |
| --- | ---: | --- | --- |
| `BT r/m32,r32`; `BT r/m64,r64`; `BTR/BTS r/m32,r32` | 5 / 2 / 1 / 2 | BT32 `0x11a734`: `44 0f a3 f8`; BTR `0x11d7e6`: `0f b3 c1` | Bit-index/result/CF semantics; memory indices need their own addressing/fault audit. |
| `LZCNT r32,r/m32` | 1 | `0x1b58c3`: `f3 0f bd f2` | Leading-zero count including zero and defined flags; target feature profile still required. |
| `ROL r/m8,imm8` | 3 | `0x1b8adf`: `c0 c0 05` | Byte result/count masking and defined flags. |
| `VEX VCVTSI2SD xmm,xmm,r/m64` | 10 | `0x1772da`: `c4 e1 83 2a c0` | Signed64 rounding/inexact behavior, not the exact signed32 conversion shortcut. |
| `VEX VMOVD xmm,r/m32` | 24 | `0x13094d`: `c5 f9 6e c1` | Scalar bit-copy/upper-lane clearing; separate encoding from VMOVSD. |
| `VEX VMOVQ xmm,r/m64`; `VMOVQ xmm,xmm/m64` | 4 / 4 | `0x171001`: `c4 e1 f9 6e c1`; `0x11dbcd`: `c5 fa 7e 0d 9b 21 17 00` | Distinct source classes and lane clearing. |
| `VEX VMOVSS xmm,m32`; `VMOVLHPS xmm,xmm,xmm` | 4 / 1 | `0x11d84c`: `c5 fa 10 0d 38 ca 47 00`; `0x19a907`: `c5 f0 16 c0` | Scalar/merge bit-copy and upper-lane semantics. |
| `VEX VMOVUPD xmm,xmm/m128`; `xmm/m128,xmm`; `ymm/m256,ymm` | 1 / 1 / 1 | `0x130151`: `c4 a1 79 10 84 2d 80 fd ff ff` | Unaligned packed-double moves; existing UPS path does not accept UPD. |
| `VEX VPBROADCASTD xmm/ymm,xmm/m32`; `VPBROADCASTW ymm,xmm/m16`; `VBROADCASTSS xmm,m32` | 6 / 6 / 1 / 3 | D128 `0x13936b`: `c4 e2 79 58 0d 50 80 15 00` | Width-specific lane replication and guarded memory. |
| `VEX VPXOR xmm,xmm,xmm/m128` | 6 | `0x11b711`: `c5 f9 ef c0` | Integer XOR encoding/lane semantics, despite existing XORPS/PD lowering. |
| `VEX VPCMPEQD`; `VPINSRD imm8`; `VPMOVZXWD`; `VPSHUFD imm8`; `VPTEST` (128-bit forms) | 1 / 1 / 3 / 1 / 2 | PCMPEQD `0x17ccc1`: `c5 e1 76 db`; PTEST `0x11b77c`: `c4 e2 79 17 c0` | Distinct packed comparisons/insertion/extension/shuffle/flag behavior. |
| `VEX VCVTSI2SS xmm,xmm,r/m32` or `r/m64` | 3 / 2 | r/m32 `0x11da1d`: `c5 82 2a 00`; r/m64 `0x1a8d44`: `c4 e1 82 2a c2` | Binary32 rounding and exception semantics. |
| VEX scalar64 `VADDSD/VDIVSD/VSUBSD/VSQRTSD/VUCOMISD/VFMADD231SD` | 1 / 2 / 1 / 1 / 1 / 1 | ADD `0x181d1d`: `c5 fb 58 05 33 b8 4a 00`; FMA231 `0x1a6268`: `c4 e2 f9 b9 c8` | Separate FP operations, rounding/exceptions/NaNs and defined flags/merge behavior. |
| `UD2` | 116 | `0x373`: `0f 0b` | Intentional invalid-opcode trap semantics and delivery, not an ordinary successful arithmetic instruction. Keeping an explicit failure boundary is preferable to silently skipping it. |

The table accounts for all 222 boundary sites in this specific discovery snapshot; the retained log distinguishes all 34 individual form/reason groups. It does **not** account for every unsupported instruction in the executable or target ISA.

## Next acceptance work

1. Pin the actual target CPU feature/control-state profile and establish the complete required encoding list independently of this implementation.
2. Split grouped implemented rows into `iced_x86::Code` plus operand/address/prefix variants; link each to its exact native test revision, independent reference, seed, host and retained result. Accepted but unverified forms must remain visibly unverified.
3. Triage newly executed failures first. For static candidates, small bit-copy families can be implemented and independently checked before FP algorithms, but each still needs upper-state, aliases and fault tests.
4. Track architectural exception delivery, x87/extended state, atomics/memory ordering, threading and address modes separately from instruction-result checks. A syscall/library service is not an instruction-coverage row merely because translated code invokes it.
