# Guest sscanf

`Sscanf` shares the existing bounded C-locale scanner with `Vsscanf`. The direct
SysV call supplies input and format in RDI/RSI, then four destination pointers
in RDX/RCX/R8/R9 and additional pointers from the stack after the return address.
Floating-point destinations are pointers too; this call does not consume XMM
arguments. No guest va_list or scratch register-save area is fabricated.

Only consumed stack argument slots are read and protected. Suppressed fields
consume no destination. All planned assignments and any errno write are guarded
before committing output or floating-point flags. Output aliases with source,
format or consumed stack argument slots are rejected explicitly. These snapshot
and atomic-fault policies are runtime limitations, not partial-fault fidelity.

Existing limits remain: 65,536-byte input/format bounds, 256 arguments, C-locale
conversions, and unsupported wide, positional and long-double assignments.

The matching build passed 569 library tests, three integration tests, strict
all-target Clippy and release compilation (79245 exit0,
`work/sscanf-{tests,clippy,release}.log`). Native verification passed (93464 exit0,
`work/sscanf-native/native.log`): four register and two stack destinations,
suppression, count/string conversions, EOF/mismatch, full-width pointers and
memory/return-frame guards. It verifies the named ELF import at GOT `0x26fb78`
before binding and executes the complete pinned M_StrToInt caller. Existing
SDL_vsscanf native regression also passes, including floating-point rounding,
errno, cursor updates and guarded outputs.

Startup-only verification passed (52702 exit0,
`work/arm64-prototype/sscanf-entry-verified.log`): sscanf is bound, all ten
named dependencies are accepted, and startup now stops at missing `opendir`.
It retains 20,096 owned dependency bytes. Cleanup remains pending and game
main has not executed. These checks do not establish retail-game compatibility,
CPU completion or gameplay.
