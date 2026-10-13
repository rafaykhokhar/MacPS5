# Binary32 string conversion

`Service::Strtof` uses the existing source-based `float_parse::strtof_effects` parser directly. It does not round through binary64. The dispatcher shares strtod's guarded input/control validation, preflights errno and end-pointer writes, preserves existing FP status and writes the binary32 result into XMM0's low32 bits. Other XMM0 bytes are preserved as a runtime policy, not an ABI guarantee.

The accepted control environment requires masked exceptions, gradual underflow, valid control words and matching SSE/x87 rounding directions. Unsupported controls and bounded-parser failures remain explicit runtime faults. C-locale parsing, source NaN payloads, hexadecimal conversion quirks and source-ordered decimal effects are covered; this is not arbitrary locale or complete floating-point environment support.

The matching release build is recorded in `work/arm64-prototype/strtof-release.log` (63152). The actual pinned `strtof_l` wrapper at `0x1161e0`, through GOT `0x26fae0`, passes direct binary32 cases, end-pointer/errno aliases, full-width input addresses, output guards and control rejection (`work/strtof-review/native.log`, 42205). The current model also passes 80,104 upstream source comparisons of result bits, consumed input, errno and MXCSR effects (`work/strtof-review/source-compare.log`, 70031). Neither result establishes console compiler equivalence or gameplay.

The combined pre-release tree passed 474 library tests and three integration tests; strict Clippy passed after the concurrent ownership API cleanup. Startup beyond this import is verified separately.
