//! Explicitly configured AppKit sheet attached to an owned presenter window.
//! No global keyboard tap and no standalone unowned input window.
//! Completion requires this window's Confirm/Cancel button or its close control.
//! Polling dispatches at most 32 application events; it never runs a modal loop.
//! AppKit references: NSWindow.makeKeyAndOrderFront, NSControlTextEditingDelegate.
#![allow(unsafe_code)]
use crate::{
    Result,
    ime_dialog::{Backend, Completion, Request},
};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use apple::AppKitBackend;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use super::*;
    use std::{
        cell::RefCell,
        ffi::{c_char, c_void},
        marker::PhantomData,
        ptr,
        rc::Rc,
    };
    type Obj = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend();
        fn objc_getClass(name: *const c_char) -> Obj;
        fn sel_registerName(name: *const c_char) -> Obj;
        fn objc_allocateClassPair(parent: Obj, name: *const c_char, extra: usize) -> Obj;
        fn objc_registerClassPair(class: Obj);
        fn class_addMethod(
            class: Obj,
            selector: Obj,
            implementation: unsafe extern "C" fn(Obj, Obj, Obj),
            types: *const c_char,
        ) -> bool;
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    macro_rules! msg {($o:expr,$s:literal=>$ret:ty $(,$ty:ty=>$a:expr)*)=>{{let sel=sel_registerName(concat!($s,"\0").as_ptr().cast());let call:unsafe extern "C" fn(Obj,Obj $(,$ty)*)->$ret=std::mem::transmute(objc_msgSend as unsafe extern "C" fn());call($o,sel $(,$a)*)}};}
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Size {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        origin: Point,
        size: Size,
    }
    #[repr(C)]
    struct Range {
        location: usize,
        length: usize,
    }
    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }
    struct Owned(Obj);
    impl Drop for Owned {
        fn drop(&mut self) {
            unsafe {
                msg!(self.0,"release"=>());
            }
        }
    }
    fn object(o: Obj) -> Result<Obj> {
        if o.is_null() {
            Err("IME AppKit object unavailable".into())
        } else {
            Ok(o)
        }
    }
    fn owned(o: Obj) -> Result<Owned> {
        Ok(Owned(object(o)?))
    }
    fn main_thread() -> Result<()> {
        unsafe {
            if !msg!(object(objc_getClass(c"NSThread".as_ptr()))?,"isMainThread"=>bool) {
                return Err("IME AppKit requires main thread".into());
            }
        }
        Ok(())
    }
    unsafe fn string(units: &[u16]) -> Result<Owned> {
        unsafe {
            owned(
                msg!(msg!(objc_getClass(c"NSString".as_ptr()),"alloc"=>Obj),"initWithCharacters:length:"=>Obj,*const u16=>units.as_ptr(),usize=>units.len()),
            )
        }
    }
    unsafe fn pool() -> Result<Owned> {
        unsafe { owned(msg!(objc_getClass(c"NSAutoreleasePool".as_ptr()),"new"=>Obj)) }
    }
    struct CallbackState {
        target: Obj,
        field: Obj,
        limit: usize,
        action: u8,
    }
    thread_local! { static CALLBACK: RefCell<Option<CallbackState>> = const { RefCell::new(None) }; }
    unsafe extern "C" fn confirm(target: Obj, _sel: Obj, _sender: Obj) {
        set_action(target, 1);
    }
    unsafe extern "C" fn cancel(target: Obj, _sel: Obj, _sender: Obj) {
        set_action(target, 2);
    }
    fn set_action(target: Obj, action: u8) {
        CALLBACK.with(|state| {
            if let Ok(mut state) = state.try_borrow_mut() {
                if let Some(state) = state
                    .as_mut()
                    .filter(|s| s.target == target && s.action == 0)
                {
                    state.action = action;
                }
            }
        });
    }
    // Cocoa may transiently receive an oversized paste. Bound retained field text
    // immediately after each edit; never copy unbounded strings into Rust.
    unsafe extern "C" fn changed(target: Obj, _sel: Obj, _notification: Obj) {
        let pair = CALLBACK.with(|state| {
            state.try_borrow().ok().and_then(|s| {
                s.as_ref()
                    .filter(|s| s.target == target)
                    .map(|s| (s.field, s.limit))
            })
        });
        if let Some((field, limit)) = pair {
            unsafe {
                trim(field, limit);
            }
        }
    }
    unsafe fn trim(field: Obj, limit: usize) {
        unsafe {
            let editor = msg!(field,"currentEditor"=>Obj);
            let value = if editor.is_null() {
                msg!(field,"stringValue"=>Obj)
            } else {
                msg!(editor,"string"=>Obj)
            };
            let length = msg!(value,"length"=>usize);
            if length > limit {
                let mut end = limit;
                if end > 0 {
                    let last = msg!(value,"characterAtIndex:"=>u16,usize=>end-1);
                    if (0xd800..=0xdbff).contains(&last) {
                        end -= 1;
                    }
                }
                let shortened = msg!(value,"substringToIndex:"=>Obj,usize=>end);
                if !editor.is_null() {
                    msg!(editor,"setString:"=>(),Obj=>shortened);
                }
                msg!(field,"setStringValue:"=>(),Obj=>shortened);
            }
        }
    }
    unsafe fn target_class() -> Result<Obj> {
        unsafe {
            let existing = objc_getClass(c"RustOwnedImeActions".as_ptr());
            if !existing.is_null() {
                return Ok(existing);
            }
            let class = object(objc_allocateClassPair(
                objc_getClass(c"NSObject".as_ptr()),
                c"RustOwnedImeActions".as_ptr(),
                0,
            ))?;
            for (selector, implementation) in [
                (c"confirm:", confirm as unsafe extern "C" fn(Obj, Obj, Obj)),
                (c"cancel:", cancel),
                (c"controlTextDidChange:", changed),
            ] {
                if !class_addMethod(
                    class,
                    sel_registerName(selector.as_ptr()),
                    implementation,
                    c"v@:@".as_ptr(),
                ) {
                    return Err("IME callback registration failed".into());
                }
            }
            objc_registerClassPair(class);
            Ok(class)
        }
    }
    struct Dialog {
        window: Owned,
        field: Owned,
        target: Owned,
        parent: Obj,
        app: Obj,
        limit: usize,
    }
    impl Drop for Dialog {
        fn drop(&mut self) {
            CALLBACK.with(|state| {
                let mut state = state.borrow_mut();
                if state.as_ref().is_some_and(|s| s.target == self.target.0) {
                    *state = None;
                }
            });
            unsafe {
                let key = msg!(self.app,"keyWindow"=>Obj);
                let restore_parent = msg!(self.app,"isActive"=>bool)
                    && (key == self.window.0 || key == self.parent)
                    && msg!(self.parent,"isVisible"=>bool);
                msg!(self.field.0,"setDelegate:"=>(),Obj=>ptr::null_mut());
                if msg!(self.window.0,"sheetParent"=>Obj) == self.parent {
                    msg!(self.parent,"endSheet:"=>(),Obj=>self.window.0);
                }
                msg!(self.window.0,"close"=>());
                if restore_parent {
                    msg!(self.parent,"makeKeyWindow"=>());
                }
            }
        }
    }
    /// Construction opts in to guest-triggered, owned text sheets. Does not
    /// present anything until begin. !Send/!Sync; one active dialog per process.
    pub struct AppKitBackend {
        dialog: Option<Dialog>,
        parent: Owned,
        _thread: PhantomData<Rc<()>>,
    }
    impl AppKitBackend {
        pub fn new(presenter: &crate::metal_present::Presenter) -> Result<Self> {
            main_thread()?;
            let parent = unsafe { owned(presenter.retained_dialog_window()?)? };
            Ok(Self {
                dialog: None,
                parent,
                _thread: PhantomData,
            })
        }
    }
    impl Backend for AppKitBackend {
        fn available(&self) -> bool {
            true
        }
        fn begin(&mut self, request: &Request) -> Result<()> {
            main_thread()?;
            if self.dialog.is_some() {
                return Err("IME backend already active".into());
            }
            if !(1..=2048).contains(&request.capacity_units)
                || request.initial_text.len() >= request.capacity_units
                || request.title.len() >= 128
                || String::from_utf16(&request.initial_text).is_err()
                || String::from_utf16(&request.title).is_err()
                || request.initial_text.contains(&0)
                || request.title.contains(&0)
            {
                return Err("IME invalid bounded UTF16 request".into());
            }
            if CALLBACK.with(|s| s.borrow().is_some()) {
                return Err("Only one AppKit IME dialog may be active".into());
            }
            unsafe {
                let _pool = pool()?;
                let app = object(
                    msg!(objc_getClass(c"NSApplication".as_ptr()),"sharedApplication"=>Obj),
                )?;
                if !msg!(app,"isActive"=>bool)
                    || msg!(app,"keyWindow"=>Obj) != self.parent.0
                    || !msg!(self.parent.0,"isVisible"=>bool)
                    || !msg!(self.parent.0,"attachedSheet"=>Obj).is_null()
                {
                    return Err(format!(
                        "IME parent unavailable: active={}, focused={}, visible={}, attached={}",
                        msg!(app,"isActive"=>bool),
                        msg!(app,"keyWindow"=>Obj) == self.parent.0,
                        msg!(self.parent.0,"isVisible"=>bool),
                        !msg!(self.parent.0,"attachedSheet"=>Obj).is_null()
                    ));
                }
                let window = owned(
                    msg!(msg!(objc_getClass(c"NSWindow".as_ptr()),"alloc"=>Obj),"initWithContentRect:styleMask:backing:defer:"=>Obj,Rect=>rect(160.,160.,520.,150.),usize=>3,usize=>2,bool=>false),
                )?;
                msg!(window.0,"setReleasedWhenClosed:"=>(),bool=>false);
                let title = string(&request.title)?;
                msg!(window.0,"setTitle:"=>(),Obj=>title.0);
                let content = object(msg!(window.0,"contentView"=>Obj))?;
                let field = owned(
                    msg!(msg!(objc_getClass(c"NSTextField".as_ptr()),"alloc"=>Obj),"initWithFrame:"=>Obj,Rect=>rect(20.,85.,480.,26.)),
                )?;
                let initial = string(&request.initial_text)?;
                msg!(field.0,"setStringValue:"=>(),Obj=>initial.0);
                msg!(content,"addSubview:"=>(),Obj=>field.0);
                let target = owned(msg!(target_class()?,"new"=>Obj))?;
                for (label, x, action) in
                    [("Cancel", 290., c"cancel:"), ("Confirm", 395., c"confirm:")]
                {
                    let button = owned(
                        msg!(msg!(objc_getClass(c"NSButton".as_ptr()),"alloc"=>Obj),"initWithFrame:"=>Obj,Rect=>rect(x,25.,100.,32.)),
                    )?;
                    let label = string(&label.encode_utf16().collect::<Vec<_>>())?;
                    msg!(button.0,"setTitle:"=>(),Obj=>label.0);
                    msg!(button.0,"setBezelStyle:"=>(),usize=>1);
                    msg!(button.0,"setTarget:"=>(),Obj=>target.0);
                    msg!(button.0,"setAction:"=>(),Obj=>sel_registerName(action.as_ptr()));
                    msg!(content,"addSubview:"=>(),Obj=>button.0);
                }
                let dialog = Dialog {
                    window,
                    field,
                    target,
                    parent: self.parent.0,
                    app,
                    limit: request.capacity_units - 1,
                };
                CALLBACK.with(|state| {
                    *state.borrow_mut() = Some(CallbackState {
                        target: dialog.target.0,
                        field: dialog.field.0,
                        limit: dialog.limit,
                        action: 0,
                    })
                });
                msg!(dialog.field.0,"setDelegate:"=>(),Obj=>dialog.target.0);
                msg!(self.parent.0,"beginSheet:completionHandler:"=>(),Obj=>dialog.window.0,Obj=>ptr::null_mut());
                msg!(dialog.window.0,"makeFirstResponder:"=>bool,Obj=>dialog.field.0);
                self.dialog = Some(dialog);
            }
            Ok(())
        }
        fn poll(&mut self) -> Result<Option<Completion>> {
            main_thread()?;
            let Some(dialog) = self.dialog.as_ref() else {
                return Ok(None);
            };
            unsafe {
                let _pool = pool()?;
                let date = object(msg!(objc_getClass(c"NSDate".as_ptr()),"distantPast"=>Obj))?;
                let mode = string(&"kCFRunLoopDefaultMode".encode_utf16().collect::<Vec<_>>())?;
                for _ in 0..32 {
                    let event = msg!(dialog.app,"nextEventMatchingMask:untilDate:inMode:dequeue:"=>Obj,usize=>usize::MAX,Obj=>date,Obj=>mode.0,bool=>true);
                    if event.is_null() {
                        break;
                    }
                    msg!(dialog.app,"sendEvent:"=>(),Obj=>event);
                    if CALLBACK.with(|s| s.borrow().as_ref().is_some_and(|s| s.action != 0)) {
                        break;
                    }
                }
                msg!(dialog.app,"updateWindows"=>());
                if !msg!(dialog.parent,"isVisible"=>bool) {
                    return Ok(Some(Completion::Aborted));
                }
                let action = CALLBACK.with(|s| s.borrow().as_ref().map_or(0, |s| s.action));
                if action == 2 || !msg!(dialog.window.0,"isVisible"=>bool) {
                    return Ok(Some(Completion::Cancelled));
                }
                if action != 1 {
                    return Ok(None);
                }
                trim(dialog.field.0, dialog.limit);
                let editor = msg!(dialog.field.0,"currentEditor"=>Obj);
                let value = if editor.is_null() {
                    msg!(dialog.field.0,"stringValue"=>Obj)
                } else {
                    msg!(editor,"string"=>Obj)
                };
                let length = msg!(value,"length"=>usize);
                if length > dialog.limit {
                    return Err("IME text exceeded capacity".into());
                }
                let mut units = vec![0u16; length];
                msg!(value,"getCharacters:range:"=>(),*mut u16=>units.as_mut_ptr(),Range=>Range{location:0,length});
                if units.contains(&0) || String::from_utf16(&units).is_err() {
                    return Err("IME host returned invalid UTF16".into());
                }
                Ok(Some(Completion::Accepted(units)))
            }
        }
        fn close(&mut self) -> Result<()> {
            main_thread()?;
            self.dialog = None;
            Ok(())
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[derive(Default)]
pub struct AppKitBackend;
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl Backend for AppKitBackend {
    fn available(&self) -> bool {
        false
    }
    fn begin(&mut self, _: &Request) -> Result<()> {
        Err("IME AppKit unavailable".into())
    }
    fn poll(&mut self) -> Result<Option<Completion>> {
        Ok(None)
    }
    fn close(&mut self) -> Result<()> {
        Ok(())
    }
}
