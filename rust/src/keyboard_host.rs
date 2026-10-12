//! Focus-gated physical keyboard polling, not text input or a global monitor.
//! Apple GCKeyboard.h coalesces all keyboards; GCPhysicalInputProfile.capture
//! provides immutable state. GCKeyCodes.h exported constants are used by name.
//! SDL scancodes/modifier bits follow pinned SDL_ps5keyboard.c and SDL_scancode.h.
//! The guest keyboard owner handles its16 delivery-aware slots separately.
#![allow(unsafe_code)]
use crate::Result;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub focused: bool,
    /// None when unfocused: even keyboard enumeration is skipped.
    pub connected: Option<bool>,
    pub pressed: Vec<u16>,
    pub modifiers: u32,
}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn poll(presenter: &crate::metal_present::Presenter) -> Result<Snapshot> {
    if !presenter.keyboard_focus()? {
        return Ok(Snapshot {
            focused: false,
            connected: None,
            pressed: Vec::new(),
            modifiers: 0,
        });
    }
    let observed = apple::poll()?;
    // Focus can change while querying the snapshot. Never publish key state
    // if this owned window has ceased to be the active key window.
    if !presenter.keyboard_focus()? {
        return Ok(Snapshot {
            focused: false,
            connected: None,
            pressed: Vec::new(),
            modifiers: 0,
        });
    }
    Ok(observed)
}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use super::*;
    use std::ffi::{c_char, c_void};
    type Obj = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Obj;
        fn sel_registerName(name: *const c_char) -> Obj;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> Obj;
        fn objc_autoreleasePoolPop(pool: Obj);
    }
    #[link(name = "GameController", kind = "framework")]
    unsafe extern "C" {}
    struct Pool(Obj);
    impl Drop for Pool {
        fn drop(&mut self) {
            unsafe { objc_autoreleasePoolPop(self.0) }
        }
    }
    unsafe fn get(object: Obj, selector: &std::ffi::CStr) -> Obj {
        unsafe {
            let f: unsafe extern "C" fn(Obj, Obj) -> Obj =
                std::mem::transmute(objc_msgSend as *const ());
            f(object, sel_registerName(selector.as_ptr()))
        }
    }
    unsafe fn pressed(input: Obj, code: isize) -> bool {
        unsafe {
            let f: unsafe extern "C" fn(Obj, Obj, isize) -> Obj =
                std::mem::transmute(objc_msgSend as *const ());
            let button = f(input, sel_registerName(c"buttonForKeyCode:".as_ptr()), code);
            if button.is_null() {
                return false;
            }
            let f: unsafe extern "C" fn(Obj, Obj) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            f(button, sel_registerName(c"isPressed".as_ptr()))
        }
    }
    macro_rules! keys {($($name:ident=>$scan:expr),* $(,)?)=>{{
        unsafe extern "C"{$(static $name:isize;)*}
        [$(($name,$scan)),*]
    }}}
    pub fn poll() -> Result<Snapshot> {
        unsafe {
            let _pool = Pool(objc_autoreleasePoolPush());
            let class = objc_getClass(c"GCKeyboard".as_ptr());
            if class.is_null() {
                return Err("GCKeyboard unavailable".into());
            }
            let keyboard = get(class, c"coalescedKeyboard");
            let mut result = Snapshot {
                focused: true,
                connected: Some(!keyboard.is_null()),
                pressed: Vec::new(),
                modifiers: 0,
            };
            if keyboard.is_null() {
                return Ok(result);
            }
            let input = get(keyboard, c"keyboardInput");
            if input.is_null() {
                return Err("Keyboard lacks input profile".into());
            }
            let input = get(input, c"capture");
            if input.is_null() {
                return Err("Keyboard snapshot failed".into());
            }
            // Explicit common gaming subset; international/media/keypad keys are
            // not mapped yet. Numeric Apple enum values are never assumed.
            for (code, scan) in keys! {
                GCKeyCodeKeyA=>4,GCKeyCodeKeyB=>5,GCKeyCodeKeyC=>6,GCKeyCodeKeyD=>7,
                GCKeyCodeKeyE=>8,GCKeyCodeKeyF=>9,GCKeyCodeKeyG=>10,GCKeyCodeKeyH=>11,
                GCKeyCodeKeyI=>12,GCKeyCodeKeyJ=>13,GCKeyCodeKeyK=>14,GCKeyCodeKeyL=>15,
                GCKeyCodeKeyM=>16,GCKeyCodeKeyN=>17,GCKeyCodeKeyO=>18,GCKeyCodeKeyP=>19,
                GCKeyCodeKeyQ=>20,GCKeyCodeKeyR=>21,GCKeyCodeKeyS=>22,GCKeyCodeKeyT=>23,
                GCKeyCodeKeyU=>24,GCKeyCodeKeyV=>25,GCKeyCodeKeyW=>26,GCKeyCodeKeyX=>27,
                GCKeyCodeKeyY=>28,GCKeyCodeKeyZ=>29,GCKeyCodeOne=>30,GCKeyCodeTwo=>31,
                GCKeyCodeThree=>32,GCKeyCodeFour=>33,GCKeyCodeFive=>34,GCKeyCodeSix=>35,
                GCKeyCodeSeven=>36,GCKeyCodeEight=>37,GCKeyCodeNine=>38,GCKeyCodeZero=>39,
                GCKeyCodeReturnOrEnter=>40,GCKeyCodeEscape=>41,GCKeyCodeDeleteOrBackspace=>42,
                GCKeyCodeTab=>43,GCKeyCodeSpacebar=>44,GCKeyCodeRightArrow=>79,
                GCKeyCodeLeftArrow=>80,GCKeyCodeDownArrow=>81,GCKeyCodeUpArrow=>82,
                GCKeyCodeF1=>58,GCKeyCodeF2=>59,GCKeyCodeF3=>60,GCKeyCodeF4=>61,
                GCKeyCodeF5=>62,GCKeyCodeF6=>63,GCKeyCodeF7=>64,GCKeyCodeF8=>65,
                GCKeyCodeF9=>66,GCKeyCodeF10=>67,GCKeyCodeF11=>68,GCKeyCodeF12=>69
            } {
                if pressed(input, code) {
                    result.pressed.push(scan)
                }
            }
            for (code, bit) in keys! {GCKeyCodeLeftControl=>0,GCKeyCodeLeftShift=>1,GCKeyCodeLeftAlt=>2,GCKeyCodeLeftGUI=>3,GCKeyCodeRightControl=>4,GCKeyCodeRightShift=>5,GCKeyCodeRightAlt=>6,GCKeyCodeRightGUI=>7}
            {
                if pressed(input, code) {
                    result.modifiers |= 1 << bit
                }
            }
            Ok(result)
        }
    }
}
