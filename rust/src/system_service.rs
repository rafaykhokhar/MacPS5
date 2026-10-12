//! Process-owned SystemService state for the two imports used by the homebrew.
//!
//! ABI evidence: the PS5 SDL header declares `int(void)` for hiding the splash:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/video/ps5/SDL_ps5video.h
//! The SDK browser sample declares `int(const char *, void *)` and passes NULL
//! for its second argument:
//! https://github.com/ps5-payload-dev/sdk/blob/master/samples/browser/main.c
//! Neither source defines the browser options layout or its error codes. The
//! generated SDK export stubs establish names only, not those missing semantics.
//!
//! Splash visibility is a virtual presentation property for a process. Hiding
//! it does not establish that guest display buffers have reached a real screen.
//! Browser calls stop explicitly until a guest browser backend and its ABI are
//! implemented; they never invoke a host application or claim launch success.

use crate::Result;

#[derive(Debug)]
pub struct SystemService {
    splash_visible: bool,
}

impl Default for SystemService {
    fn default() -> Self {
        Self {
            splash_visible: true,
        }
    }
}

impl SystemService {
    pub fn splash_visible(&self) -> bool {
        self.splash_visible
    }

    /// Hide this process's virtual splash. Repeated hides are idempotent.
    pub fn hide_splash_screen(&mut self) -> i32 {
        self.splash_visible = false;
        0
    }

    /// Guest ABI arguments remain integer guest addresses. Without a backend,
    /// no pointed-to bytes are read and no guest state or memory is modified.
    /// Errors are runtime diagnostics, not invented console return codes.
    pub fn launch_web_browser(&self, _uri: u64, options: u64) -> Result<i32> {
        if options != 0 {
            return Err("Unsupported sceSystemServiceLaunchWebBrowser non-null options ABI".into());
        }
        Err(
            "Unsupported sceSystemServiceLaunchWebBrowser: guest browser backend unavailable"
                .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splash_state_is_owned_by_each_process_and_hiding_is_idempotent() {
        let mut first = SystemService::default();
        let second = SystemService::default();
        assert!(first.splash_visible());
        assert_eq!(first.hide_splash_screen(), 0);
        assert!(!first.splash_visible());
        assert!(second.splash_visible());
        assert_eq!(first.hide_splash_screen(), 0);
        assert!(!first.splash_visible());
    }

    #[test]
    fn browser_without_backend_never_succeeds_or_changes_splash_state() {
        let mut service = SystemService::default();
        for visible in [true, false] {
            if !visible {
                service.hide_splash_screen();
            }
            for uri in [0, 1, 0x1000, u64::MAX] {
                let error = service.launch_web_browser(uri, 0).unwrap_err();
                assert!(error.contains("backend unavailable"));
                assert_eq!(service.splash_visible(), visible);
                for options in [1, 0x1000, u64::MAX] {
                    let error = service.launch_web_browser(uri, options).unwrap_err();
                    assert!(error.contains("non-null options ABI"));
                    assert_eq!(service.splash_visible(), visible);
                }
            }
        }
    }
}
