# Presentation verification

On 2026-10-13, after the user confirmed the desktop was unlocked, the unchanged
standalone presentation path passed on Apple M5 Pro (session70872 exit0).
It presented a host-generated 640×129 RGBA image on attempt1 with finite positive
`presentedTime=62229.494376625014`; the success path also requires completed
command status4. Logs: `work/presentation-diagnostics/unlocked-presentation.log`
and `unlocked-session.log`. The session probe reported onConsole/loginDone true
and no screenLockedUndocumented key. The success log does not expose callback
counts. This resolves the focused presentation check, not guest GPU execution,
gameplay, or the full integrated runner. The lock state is a plausible explanation
for the earlier failures, not a proven causal diagnosis.

## Earlier failed checks

The mkdir integration runs repeatedly failed to confirm drawable presentation. Startup independently reaches missing `pthread_cond_init`; that does not turn the integrated run into a pass.

Focused native diagnostics on Apple M5 Pro reproduced three dropped frames in 0.353 seconds: command status 4 (completed), presentedTime 0, and matching presented callbacks for drawable IDs 0, 1 and 2. This instance exhausted the three-attempt limit before the three-second deadline. The visible, nonminiaturized window reported occlusion state 0x2000 and a screen; layer bounds and drawable size were 640×129, opacity 1, with a superlayer. App active was false, hidden false. A separate main-display probe reported active, awake and online. These observations do not identify the cause.

Controlled work-directory-only experiments with additional startup pumping, modern activation/event pacing, transaction presentation and disabled display sync did not produce a positive presentation timestamp. No experimental behavior change was adopted. Logs and harnesses reside under `work/presentation-diagnostics` in the development workspace.

Repository changes retain per-attempt diagnostics and keep the existing success requirement: completed command plus finite positive presentedTime. Isolated ARM64 strict compilation passed; the focused native baseline failed as described. Rendering command completion alone is insufficient proof of display presentation.

References: [MTLDrawable.presentedTime](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime), [NSApplication.activate](https://developer.apple.com/documentation/appkit/nsapplication/activate()), [CAMetalLayer.presentsWithTransaction](https://developer.apple.com/documentation/quartzcore/cametallayer/presentswithtransaction).

## LaunchServices comparison and constructor correction

A minimal regular `.app` bundle launched through LaunchServices initially failed before creating Metal objects: `setActivationPolicy(.regular)` returned false even though the existing activation policy was already regular. The constructor now skips redundant policy requests and reads the effective policy afterward. A nonregular effective policy remains an error regardless of the setter's return value. Regular policy permits normal app UI; it does not prove foreground activation or presentation.

The unchanged presentation path was then compared through LaunchServices and direct execution of the same bundled executable. Both reached the Apple M5 Pro renderer and failed with three completed, callback-confirmed drawables whose presentation timestamps remained zero. App active remained false. The constructor correction therefore fixes an independently observed `.app` initialization failure; it does not resolve dropped frames or establish a LaunchServices workaround.

The adopted source passed isolated ARM64 compilation with warnings denied. Final focused runs are recorded in `work/presentation-diagnostics/app-result-policy-verified-launchservices.log` and `app-policy-verified-direct.log`. LaunchServices `open -W` returned zero, but the application's explicit terminal result recorded failure; launcher exit status is not presentation evidence. The direct executable returned one. No release runtime archive was rebuilt for these checks.

Apple documents that [`setActivationPolicy`](https://developer.apple.com/documentation/appkit/nsapplication/setactivationpolicy(_:)) attempts a policy change and reports its success. [`run`](https://developer.apple.com/documentation/appkit/nsapplication/run()) normally maintains the event loop, invoking [`finishLaunching`](https://developer.apple.com/documentation/appkit/nsapplication/finishlaunching()) before dispatch. Repeated brief run/stop cycles remain a lifecycle difference to investigate; previous event-pacing experiments did not establish a remedy.

## Locked-session lead (2026-10-13)

A subsequent read-only CoreGraphics session probe returned `onConsole=true`, `loginDone=true`, and `screenLockedUndocumented=true`. The first two values use public keys declared in the installed macOS SDK's `CGSession.h`; the last queries the undocumented `CGSSessionScreenIsLocked` dictionary entry and must be treated as a diagnostic hint, not a supported API contract. Only these Boolean fields were inspected. The concurrent main-display probe still reported active, awake and online, so those display properties do not rule out a locked session.

This is a plausible environmental explanation for callback-confirmed dropped frames despite a window's visible flag. It is not yet a demonstrated cause: no controlled unlocked-versus-locked comparison has been performed. The probe neither creates a window nor activates or unlocks an application. Its source and output are retained in development workspace `work/presentation-diagnostics/session.rs` and `session.log` (strict standalone Rust compilation and execution returned zero). No presenter behavior changed, and the positive `presentedTime` success criterion remains intact. The next discriminating check is the same focused binary in an interactively unlocked session, with lock-state observations and actual presentation timestamps recorded together.
