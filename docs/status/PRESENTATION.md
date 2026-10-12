# Current presentation failure

The mkdir integration runs repeatedly failed to confirm drawable presentation. Startup independently reaches missing `pthread_cond_init`; that does not turn the integrated run into a pass.

Focused native diagnostics on Apple M5 Pro reproduced three dropped frames in 0.353 seconds: command status 4 (completed), presentedTime 0, and matching presented callbacks for drawable IDs 0, 1 and 2. This instance exhausted the three-attempt limit before the three-second deadline. The visible, nonminiaturized window reported occlusion state 0x2000 and a screen; layer bounds and drawable size were 640×129, opacity 1, with a superlayer. App active was false, hidden false. A separate main-display probe reported active, awake and online. These observations do not identify the cause.

Controlled work-directory-only experiments with additional startup pumping, modern activation/event pacing, transaction presentation and disabled display sync did not produce a positive presentation timestamp. No experimental behavior change was adopted. Logs and harnesses reside under `work/presentation-diagnostics` in the development workspace.

Repository changes retain per-attempt diagnostics and keep the existing success requirement: completed command plus finite positive presentedTime. Isolated ARM64 strict compilation passed; the focused native baseline failed as described. Rendering command completion alone is insufficient proof of display presentation.

References: [MTLDrawable.presentedTime](https://developer.apple.com/documentation/metal/mtldrawable/presentedtime), [NSApplication.activate](https://developer.apple.com/documentation/appkit/nsapplication/activate()), [CAMetalLayer.presentsWithTransaction](https://developer.apple.com/documentation/quartzcore/cametallayer/presentswithtransaction).
