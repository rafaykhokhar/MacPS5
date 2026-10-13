# strftime implementation boundary

The registered Rust service in `rust/src/guest_strftime.rs` implements the C-locale formatting surface described below. This does not establish complete time-library compatibility.

The integrated build passes 465 library tests, three integration tests and strict Clippy (`strftime-build.log`, 90645). Native verification against that archive passes the actual `strftime_l` wrapper, C names/composites/ISO weeks/modifiers, lazy tm reads and NULL format, full-width arguments, ignored locale argument, exact-capacity prefixes, timezone offsets/strings, output canaries and readonly/alias guards. An unsupported `%s` retains its earlier output prefix before the runtime fault (`work/strftime-native/native.log`, 9434, exit zero). Startup beyond this import is a separate check.

## ABI and source evidence

The pinned crispy-doom ELF has `strftime_l` at `0x1161c0`, a tail jump through GOT `0x26fad8`. The fifth locale argument is forwarded but the imported target is ordinary `strftime`; the intended service consumes `(destination, capacity, format, tm)`.

[SDK time.h](https://raw.githubusercontent.com/ps5-payload-dev/sdk/10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/time.h) declares nine signed int32 fields: sec/min/hour/mday/mon/year/wday/yday/isdst at offsets 0/4/8/12/16/20/24/28/32. LP64 alignment puts signed long gmtoff at 40 and the zone pointer at 48; total size is 56. Model reads are lazy: a literal format does not read `tm`, and a date-only conversion does not read timezone fields. A NULL format selects `%c`.

Reference behavior comes from [FreeBSD 11.4 strftime.c](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdtime/strftime.c) and its [C-locale table](https://raw.githubusercontent.com/freebsd/freebsd-src/releng/11.4/lib/libc/stdtime/timelocal.c). Local copies and the standalone test harness are under `work/arm64-prototype/strftime-audit`. These are source-derived policies, not measurements of console libc.

## Implemented behavior

Names, date/time numerics and composites cover `%a/%A/%b/%B/%h/%c/%C/%d/%D/%e/%F/%H/%I/%j/%k/%l/%m/%M/%n/%p/%r/%R/%S/%t/%T/%u/%U/%v/%V/%w/%W/%x/%X/%y/%Y/%g/%G/%z/%Z/%+` and `%%`. C-locale E/O modifiers and source padding flags `-`, `_`, `0` are handled, including repeated-modifier/unknown-character behavior. Source year formatting ignores padding modifiers and preserves its negative/large-year century convention. Invalid name indexes produce `?`; ISO weeks require normalized weekday/year-day inputs.

The model preserves full-capacity prefixes and returns zero without forcing NUL. Successful output includes NUL; empty success also returns zero. It tracks transient terminators written by source conversion helpers, so a later unsupported conversion retains the earlier prefix including an existing NUL when appropriate. Ordinary literals do not manufacture such a terminator.

`%z` uses guest gmtoff when isdst is nonnegative. `%Z` reads a supplied guest zone string; otherwise nonnegative isdst requires explicit configured UTC and produces `UTC`. Negative isdst with no zone produces nothing. Host locale/timezone APIs are never consulted.

## API and guards

`plan(memory, destination, capacity, format, tm, Policy { timezone }) -> Result<Plan, Error>` returns destination, exact final bytes and `Completion::{Returned(size), Unsupported(reason)}`. The executor must validate those bytes against its live return frame, commit, then either return the size or fault on Unsupported. Unsupported is never a synthetic libc zero return. Memory, alias and budget errors reject the plan atomically, matching the runtime's guarded-access boundary.

The maximum destination capacity is 65,536 bytes, with bounded source reads and composite recursion. Destination overlap with any consumed format/tm/zone source is explicitly rejected, consistent with the source's restrict-qualified contract. No errno access is needed for supported conversions.

Ten standalone tests pass, with strict standalone Clippy: C-locale names/composites, exact-capacity truncation, modifiers/trailing-percent handling, negative/large years, ISO boundaries, invalid names/week numbering, timezone fields and fallback policy, lazy tm access/guards, and partial output on unsupported conversion. These are model tests, not native-wrapper evidence.

## Remaining requirements

- `%s` requires a source-faithful guest mktime implementation: UTC field normalization, overflow, leap/calendar handling, and tm_isdst semantics. A simple epoch calculation or host mktime is not an accepted substitute.
- Source strftime calls tzset even for formats without timezone fields. Guest timezone initialization, TZ environment effects and timezone database support are not modeled here. Reuse the runtime's existing configured UTC state for the limited fallback policy; do not invent a second implicit timezone.
- The source's optional CHECK_STRFTIME_FORMATS_FOR_TWO_DIGIT_YEARS stderr warnings are not implemented.
- Arbitrary locales and alternative calendar/numeral representations remain unsupported; C-locale E/O behavior is not general locale support.
- Extreme invalid numeric inputs that would overflow source int arithmetic, and unnormalized ISO week inputs, remain guarded boundaries.
- Native checks must exercise the actual `strftime_l` tail wrapper, output canaries, exact-capacity failure prefixes, tm offsets, ignored locale argument, timezone strings, and source/output guards on a matching archive.

## Native wrapper checkpoint

After root registration and matching release build `90645`, isolated native verifier session `9434` completed with exit status 0. `rust/verification/strftime_native.rs` calls the actual pinned tail wrapper and checks the C-locale formats, ISO weeks/modifiers, NULL format, unused invalid tm for literals, zero/full capacities, destination canaries, ignored invalid locale, guest gmtoff/zone fields, UTC fallback, read-only and alias guards, and retained prefix before unsupported `%s`. Log: `work/strftime-native/native.log`. No host locale formatting is used as a substitute. The remaining requirements above still apply.
