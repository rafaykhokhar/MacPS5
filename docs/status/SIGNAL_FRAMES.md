# Signal frame ABI audit

Research for the open SA_SIGINFO, SA_ONSTACK and context-restoration gates in [ABORT.md](ABORT.md). This document does not implement or verify guest signal-frame delivery. The public payload SDK is pinned at `10b539de8f2b6773a9a78caa7576a97dbd99b29a`; its FreeBSD-derived declarations are evidence of that SDK's interface, not measurements of Sony's kernel/libc on a specified firmware. FreeBSD 11.4 kernel source supplies a reference behavior, not proof that PS5 uses an identical trampoline, frame placement, selectors, XSAVE feature set or return validation.

## Declaration-derived layout

Offsets below are decimal bytes, derived from the pinned amd64 LP64 headers: 64-bit pointers, long, size_t and register_t; 32-bit int/pid_t/uid_t; explicit 16-byte FP alignment. No compiler layout probe or console capture was run in this audit. Implementation must independently assert these layouts against the pinned SDK before accepting the serialization.

Sources: [machine scalar types](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/_types.h), [system types](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_types.h), [signal mask](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_sigset.h).

### siginfo_t and stack_t

`siginfo_t` is 80 bytes, alignment 8. Its reason union is 40 bytes; do not substitute Linux's 128-byte siginfo representation. `stack_t` is 24 bytes, alignment 8. Signal masks occupy four u32 words (16 bytes).

| siginfo field | Offset | Bytes |
| --- | ---: | ---: |
| si_signo / si_errno / si_code | 0 / 4 / 8 | 4 each |
| si_pid / si_uid / si_status | 12 / 16 / 20 | 4 each |
| si_addr | 24 | 8 |
| si_value, pointer-or-int union | 32 | 8 |
| _reason union | 40 | 40 |
| _reason._fault._trapno | 40 | 4 |
| _reason._timer._timerid / _overrun | 40 / 44 | 4 each |
| _reason._mesgq._mqd | 40 | 4 |
| _reason._poll._band | 40 | 8 |
| _reason.__spare__.__spare1__ / __spare2__[7] | 40 / 48 | 8 / 28 |

| stack field | Offset | Bytes |
| --- | ---: | ---: |
| ss_sp | 0 | 8 |
| ss_size | 8 | 8 |
| ss_flags | 16 | 4 |
| tail padding | 20 | 4 |

SA_ONSTACK=1, SA_SIGINFO=0x40, SS_ONSTACK=1 and SS_DISABLE=4. The pinned amd64 minimum is 2048 bytes; the recommended SIGSTKSZ is 34816 bytes. These constants do not establish the actual console kernel's effective minimum once extended CPU state is included. [Signal declarations](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/signal.h), [machine limits](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/_limits.h).

### mcontext_t

Size 800, alignment 16. Offsets are relative to the mcontext itself.

| Field | Offset | Bytes |
| --- | ---: | ---: |
| mc_onstack | 0 | 8 |
| mc_rdi, rsi, rdx, rcx, r8, r9 | 8, 16, 24, 32, 40, 48 | 8 each |
| mc_rax, rbx, rbp, r10, r11 | 56, 64, 72, 80, 88 | 8 each |
| mc_r12, r13, r14, r15 | 96, 104, 112, 120 | 8 each |
| mc_trapno / mc_fs / mc_gs | 128 / 132 / 134 | 4 / 2 / 2 |
| mc_addr | 136 | 8 |
| mc_flags / mc_es / mc_ds | 144 / 148 / 150 | 4 / 2 / 2 |
| mc_err, rip, cs, rflags, rsp, ss | 152, 160, 168, 176, 184, 192 | 8 each |
| mc_len / mc_fpformat / mc_ownedfp | 200 / 208 / 216 | 8 each |
| mc_fpstate[64] | 224 | 512 |
| mc_fsbase / mc_gsbase | 736 / 744 | 8 each |
| mc_xfpustate / mc_xfpustate_len | 752 / 760 | 8 each |
| mc_spare[4] | 768 | 32 |

Flags HASSEGS=1, HASBASES=2, HASFPXSTATE=4. FP format NODEV=0x10000 or XMM=0x10002; ownership NONE=0x20000, FPU=0x20001, PCB=0x20002. [Pinned mcontext declaration](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/ucontext.h).

### ucontext_t and declared sigframe

`ucontext_t`: size 880, alignment 16. uc_sigmask@0 (16 bytes), uc_mcontext@16 (800), uc_link@816 (8), uc_stack@824 (24), uc_flags@848 (4), __spare__[4]@852 (16), tail padding@868 (12). `uc_link` is not the ordinary signal-return target.

Declared amd64 `sigframe`: size 976, alignment 16. Handler/action pointer union@0 (8), padding@8 (8), sf_uc@16 (880), sf_si@896 (80). These are SDK-declaration-derived offsets, not an observed console stack frame. [ucontext declaration](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/_ucontext.h), [sigframe declaration](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/sigframe.h).

## FP and extended state

The 512-byte legacy FP area is an amd64 FXSAVE-shaped `savefpu`. Relative offsets: CW@0/u16, SW@2/u16, abridged tag@4/u8, zero@5, opcode@6/u16, FP instruction pointer@8/u64, operand pointer@16/u64, MXCSR@24/u32, MXCSR mask@28/u32; eight 16-byte x87 slots@32 (10 payload bytes plus 6 padding each), sixteen 16-byte XMM slots@160, padding@416 (96 bytes). The 64-byte XSAVE header starts the separate extended area: xstate_bv@0, xcomp_bv@8, reserved@16. The declared YMM layout then has sixteen upper 128-bit halves@64, totaling 320 extended bytes; this is not an exhaustive PS5 XSAVE profile. [Pinned FP declarations](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/x86/fpu.h).

FreeBSD's `fpusetxstate` requires XSAVE availability, at least the 64-byte header, a runtime-dependent maximum length and xstate_bv contained in xsave_mask. It copies the state bitmap and extended payload, not a blindly trusted whole supplied header. `fpusetregs` masks MXCSR with cpu_mxcsr_mask. Do not replace those rules with arbitrary host ARM64 control bits, hardcode the maximum to 320, or infer enabled components from the SDK declaration alone. [FreeBSD FP restore](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/amd64/amd64/fpu.c).

Current `legacy_cpu::Cpu` has GPRs, flags, MXCSR, x87 CW/SW and sixteen 256-bit vector values. It lacks the eight x87 data registers, tag word, FP instruction/operand pointers and opcode, and does not itself provide all segment/trap fields. A 720-byte internal CPU snapshot is not an 800-byte guest mcontext. Full signal serialization requires a defined guest CPU/XSAVE profile and state extension; zero-filled missing live architectural state is not faithful preservation.

## Reference kernel delivery and return

FreeBSD `sendsig` snapshots mask/stack/trapframe/FP state, then selects alternate-stack top only when enabled, requested and not already on it; otherwise it reserves the 128-byte red zone. Extended state is placed below that top with 64-byte alignment, followed by a 16-byte-aligned sigframe. RDI receives signal number; RSI receives siginfo for SA_SIGINFO, otherwise the legacy code; RDX receives ucontext and RCX the fault address. Execution enters the ABI's signal trampoline with DF/TF cleared. `sys_sigreturn` copies user ucontext, rejects unknown mc_flags and disallowed RFLAGS changes, checks user CS privilege, validates/copies extended state, restores FP/trapframe/bases and mask, and returns EJUSTRETURN. Bad CS queues SIGBUS and returns EINVAL. The reference sigreturn path does not validate mc_len; do not confuse it with set_mcontext's rules. Invalid addresses/selectors can subsequently fault in hardware, which our translator must model or reject explicitly. [FreeBSD amd64 delivery/return](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/amd64/amd64/machdep.c).

The reference trampoline calls the stored handler, passes sf_uc to SYS_sigreturn, and has a faulting fallback if the syscall returns. A private sentinel can authenticate internal continuations but cannot substitute for reading valid guest modifications to ucontext once context-changing returns are supported. [Trampoline](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/amd64/amd64/sigtramp.S).

The RFLAGS comparison preserves all bits outside PSL_USERCHANGE; that set includes CF/PF/AF/ZF/SF/TF/DF/OF/NT/RF/AC/ID. Do not accidentally allow privilege bits such as IOPL/IF. The separate CS check requires user privilege. Firmware selector values remain unverified. [Reference flag definitions](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/x86/include/psl.h).

For abort through the audited FreeBSD raise path, `raise` calls thr_self then thr_kill. The kernel supplies SI_LWP=0x10007, sending process PID and real UID. These require explicit guest identity, not host process identifiers; fault addresses or hardware trap codes must not be invented for a software raise. [raise](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/gen/raise.c), [thr_kill](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/kern_thr.c).

Libthr also interposes delivery: it can pass a copied/adjusted context to sigreturn after the user handler, remove SIGCANCEL from its mask and restore thread cancellation bookkeeping. Direct kernel semantics and imported pthread/libc semantics must remain distinct. [Libthr signal path](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libthr/thread/thr_sig.c).

## Alternate stack ownership and verification gates

FreeBSD's normal on-stack test derives state from the current SP in the enabled half-open registered range. Query reports SS_ONSTACK/SS_DISABLE accordingly. Changing stack registration while on it fails EPERM; unsupported input flags fail EINVAL; enabled size below the ABI minimum fails ENOMEM. Registration stores the guest range rather than allocating its memory. Nested SA_ONSTACK delivery remains on the current stack instead of restarting at its top. sigreturn does not generally replace the registered stack from uc_stack; compatibility mc_onstack handling is conditional. [Reference stack handling](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/sys/kern/kern_sig.c).

Required runtime design and tests:

- Explicit per-thread alternate-stack registration, checked range arithmetic, defined minimum/profile and bounds. Acquire backing leases for active frame/context/extended state and handler code; preserve them across scheduling, waits and nested callbacks. Any stronger protection against unmapping or freeing registered memory is a documented runtime safety policy, not a claimed FreeBSD registration check.
- Serialize a real guest frame using independently checked offsets; snapshot inputs before output writes, validate all affected memory, reserve continuation/lease capacity and publish mask/disposition/frame state consistently. Retain instruction consumption across returns.
- Validate and restore guest-modified RIP/RSP/GPRs/flags/TLS bases/mask/FP state according to the supported profile. Do not expose host pointers or restore internal fuel, backing handles and dispatcher state from guest data. Unsupported segment/XSAVE/context changes must fail explicitly.
- Exercise ordinary and SA_SIGINFO argument values, mask edits, RIP/RSP changes, x87/XMM/YMM/MXCSR preservation, invalid flags/CS/FP formats/extended-state lengths, unaligned or short contexts, red-zone preservation, alternate-stack boundaries, recursive delivery, switching/disable errors, thread isolation and lifecycle retention.
- Obtain console or otherwise independent target evidence for frame placement, trampoline/syscall entry, selectors, CPU feature mask, FP reset at handler entry and return-fault behavior. Header agreement and a passing self-generated fixture do not establish those facts.

Only this document was added for the audit; no runtime code or build was produced. Retrieved reference files reside under the development workspace's `work/arm64-prototype/abort-audit`.
