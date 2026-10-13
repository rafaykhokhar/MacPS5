# C/POSIX wide collation transform

`Service::Wcsxfrm` implements the C/POSIX copy fallback with unsigned 16-bit
guest wide characters. It returns the full source length in units, writes at
most capacity minus one units plus NUL, and does not pad unused capacity.
Capacity zero ignores the destination but still validates the source.

The implementation follows FreeBSD 11.4's `wcsxfrm` fallback: first `wcslen`,
then `wcscpy` if the source fits, otherwise `wcsncpy(n-1)` and a final NUL.
The guest character width is a pinned SDK profile choice, not the host's wchar
width or proof of Sony's binary implementation. Non-C locales are unsupported.
Sources are linked in `rust/src/guest_wcsxfrm.rs`.

The scan is bounded to 65,536 guest units. The dispatcher validates the exact
write span against protected storage and its live frame before committing.
Malformed input and restrict-overlap failures are atomic. Huge capacities do
not allocate capacity-sized buffers. No guest heap allocation or errno update
is performed.

The matching release passes 140 native direct-service and actual pinned
`wcsxfrm_l` wrapper cases at offset `0x116230`, GOT `0x26fb00`
(`work/wcsxfrm-native/current-release.log`, session88974 exit0). Cases include
surrogates/unsigned units, truncation, no padding, zero capacity with invalid
destination, exact spans, source/write/frame/overlap guards and unchanged
errno/heap. Four helper tests cover bounds and planning. The native verifier
is registered in the full entry runner. Resolving an import does not show that
startup called the service or that game main executed.

Startup-only verification binds the actual `wcsxfrm` GOT slot and stops at
missing `mbrlen`, retaining 19,520 owned dependency bytes. All startup ownership
assertions pass (`work/arm64-prototype/wcsxfrm-final.log`, session72544 exit0).
Component and Metal checks are explicitly skipped in that run; the component
evidence above comes from separate native runs. Game main has not executed.

Both component runs used release archive SHA-256
`7325ba36c3250acbe28abeb4d89f9f74dc3e69fe7fa33436bceae22b819aebed`.
The helper source SHA-256 was
`7267495ec8db58fcbfbd310bc5016b3bc97e815c8b51d3d535d2d389794c30fa`;
the AOT source SHA-256 was
`e9eb00d914aaa66be39cd6e6eb87141d51695717f1725ed44d7853d68a6354c7`.
