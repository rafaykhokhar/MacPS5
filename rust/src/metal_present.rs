//! Host-only bounded RGBA presenter. A positive drawable presentedTime is required
//! for success; command completion alone never proves on-screen presentation.
//! Dropped startup drawables may retry the same RGBA up to three times within
//! a three-second deadline. A retry follows a completion callback for that
//! exact drawable with zero presentedTime; it is not a guest flip completion.
//! nextDrawable additionally uses Core Animation's built-in one-second timeout.
//! https://developer.apple.com/documentation/metal/mtldrawable/presentedtime
//! No guest VideoOut ABI or game-frame claim is implied by this window.
#![allow(unsafe_code)]
use crate::Result;
#[derive(Debug)]
pub struct Evidence {
    pub device_name: String,
    pub presented_time: f64,
    pub drawable_id: usize,
    pub attempts: u32,
}
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use apple::Presenter;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use super::*;
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr,
        time::{Duration, Instant},
    };
    type Obj = *mut c_void;
    #[repr(C)]
    struct BlockDescriptor {
        reserved: usize,
        size: usize,
    }
    #[repr(C)]
    struct PresentedBlock {
        isa: *const c_void,
        flags: i32,
        reserved: i32,
        invoke: unsafe extern "C" fn(*const PresentedBlock, Obj),
        descriptor: *const BlockDescriptor,
    }
    // Immutable global block has no captures; runtime never frees its storage.
    unsafe impl Sync for PresentedBlock {}
    unsafe extern "C" {
        static _NSConcreteGlobalBlock: c_void;
    }
    static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
        reserved: 0,
        size: std::mem::size_of::<PresentedBlock>(),
    };
    static PRESENT_CALLBACKS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    static FINISHED_DRAWABLE: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    unsafe extern "C" fn presented(_block: *const PresentedBlock, drawable: Obj) {
        PRESENT_CALLBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        FINISHED_DRAWABLE.store(drawable as usize, std::sync::atomic::Ordering::Release);
    }
    static PRESENTED_BLOCK: PresentedBlock = PresentedBlock {
        isa: &raw const _NSConcreteGlobalBlock,
        flags: 1 << 28,
        reserved: 0,
        invoke: presented,
        descriptor: &BLOCK_DESCRIPTOR,
    };
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend();
        fn objc_getClass(name: *const c_char) -> Obj;
        fn sel_registerName(name: *const c_char) -> Obj;
        fn objc_allocateClassPair(superclass: Obj, name: *const c_char, extra: usize) -> Obj;
        fn objc_registerClassPair(class: Obj);
        fn class_addMethod(
            class: Obj,
            selector: Obj,
            implementation: unsafe extern "C" fn(Obj, Obj, Obj),
            types: *const c_char,
        ) -> bool;
    }
    #[link(name = "Metal", kind = "framework")]
    unsafe extern "C" {
        fn MTLCreateSystemDefaultDevice() -> Obj;
    }
    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRunLoopRunInMode(mode: Obj, seconds: f64, return_after_source: u8) -> i32;
    }
    macro_rules! msg {($o:expr,$s:literal=>$ret:ty $(,$ty:ty=>$a:expr)*)=>{{let sel=sel_registerName(concat!($s,"\0").as_ptr().cast());let call:unsafe extern "C" fn(Obj,Obj $(,$ty)*)->$ret=std::mem::transmute(objc_msgSend as unsafe extern "C" fn());call($o,sel $(,$a)*)}};}
    fn checked(o: Obj) -> Result<Obj> {
        if o.is_null() {
            Err("Presenter required object unavailable".into())
        } else {
            Ok(o)
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
    fn owned(o: Obj) -> Result<Owned> {
        Ok(Owned(checked(o)?))
    }
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
    #[derive(Clone, Copy)]
    struct Extent {
        x: usize,
        y: usize,
        z: usize,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Region {
        origin: Extent,
        size: Extent,
    }
    fn main_thread() -> Result<()> {
        unsafe {
            if !msg!(checked(objc_getClass(c"NSThread".as_ptr()))?,"isMainThread"=>bool) {
                return Err("Presenter requires host main thread".into());
            }
        }
        Ok(())
    }
    unsafe fn pool() -> Result<Owned> {
        unsafe { owned(msg!(checked(objc_getClass(c"NSAutoreleasePool".as_ptr()))?,"new"=>Obj)) }
    }
    unsafe fn text(s: &'static CStr) -> Result<Obj> {
        unsafe {
            checked(
                msg!(checked(objc_getClass(c"NSString".as_ptr()))?,"stringWithUTF8String:"=>Obj,*const c_char=>s.as_ptr()),
            )
        }
    }
    unsafe fn events(app: Obj) -> Result<()> {
        unsafe {
            let date =
                checked(msg!(checked(objc_getClass(c"NSDate".as_ptr()))?,"distantPast"=>Obj))?;
            let mode = text(c"kCFRunLoopDefaultMode")?;
            for _ in 0..32 {
                let e = msg!(app,"nextEventMatchingMask:untilDate:inMode:dequeue:"=>Obj,usize=>usize::MAX,Obj=>date,Obj=>mode,bool=>true);
                if e.is_null() {
                    break;
                }
                msg!(app,"sendEvent:"=>(),Obj=>e);
            }
            msg!(app,"updateWindows"=>());
            msg!(checked(objc_getClass(c"CATransaction".as_ptr()))?,"flush"=>());
            CFRunLoopRunInMode(mode, 0.001, 1);
            Ok(())
        }
    }
    unsafe extern "C" fn end_launch(_object: Obj, _selector: Obj, app: Obj) {
        unsafe {
            msg!(app,"stop:"=>(),Obj=>ptr::null_mut());
            let event = msg!(objc_getClass(c"NSEvent".as_ptr()),"otherEventWithType:location:modifierFlags:timestamp:windowNumber:context:subtype:data1:data2:"=>Obj,usize=>15,Point=>Point{x:0.0,y:0.0},usize=>0,f64=>0.0,isize=>0,Obj=>ptr::null_mut(),i16=>0,isize=>0,isize=>0);
            msg!(app,"postEvent:atStart:"=>(),Obj=>event,bool=>true);
        }
    }
    unsafe fn launch_loop(app: Obj) -> Result<()> {
        unsafe {
            if msg!(app,"isRunning"=>bool) {
                return Ok(());
            }
            let mut class = objc_getClass(c"RustBoundedPresenterLaunch".as_ptr());
            if class.is_null() {
                class = checked(objc_allocateClassPair(
                    objc_getClass(c"NSObject".as_ptr()),
                    c"RustBoundedPresenterLaunch".as_ptr(),
                    0,
                ))?;
                if !class_addMethod(
                    class,
                    sel_registerName(c"finish:".as_ptr()),
                    end_launch,
                    c"v@:@".as_ptr(),
                ) {
                    return Err("Cannot register presenter launch callback".into());
                }
                objc_registerClassPair(class);
            }
            let helper = owned(msg!(class,"new"=>Obj))?;
            msg!(helper.0,"performSelector:withObject:afterDelay:"=>(),Obj=>sel_registerName(c"finish:".as_ptr()),Obj=>app,f64=>0.01);
            msg!(app,"run"=>());
            Ok(())
        }
    }
    /// Main-thread only (!Send/!Sync); fixed dimensions, one in-flight frame.
    pub struct Presenter {
        app: Obj,
        window: Option<Owned>,
        layer: Owned,
        _device: Owned,
        queue: Owned,
        texture: Owned,
        width: u32,
        height: u32,
        name: String,
        faulted: bool,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Presenter {
        pub fn new(width: u32, height: u32) -> Result<Self> {
            main_thread()?;
            if width == 0 || height == 0 || width > 1920 || height > 1080 {
                return Err("Presenter dimensions exceed1920x1080 limit".into());
            }
            unsafe {
                let _pool = pool()?;
                let app = checked(
                    msg!(checked(objc_getClass(c"NSApplication".as_ptr()))?,"sharedApplication"=>Obj),
                )?;
                if !msg!(app,"setActivationPolicy:"=>bool,isize=>0) {
                    return Err("Cannot activate presenter application".into());
                }
                msg!(app,"finishLaunching"=>());
                let device = owned(MTLCreateSystemDefaultDevice())?;
                let name_obj = checked(msg!(device.0,"name"=>Obj))?;
                let name_ptr = msg!(name_obj,"UTF8String"=>*const c_char);
                if name_ptr.is_null() {
                    return Err("Missing presenter device name".into());
                }
                let name = CStr::from_ptr(name_ptr).to_string_lossy().into_owned();
                let queue = owned(msg!(device.0,"newCommandQueue"=>Obj))?;
                let layer =
                    owned(msg!(checked(objc_getClass(c"CAMetalLayer".as_ptr()))?,"new"=>Obj))?;
                msg!(layer.0,"setDevice:"=>(),Obj=>device.0);
                msg!(layer.0,"setPixelFormat:"=>(),usize=>80); // BGRA8Unorm
                msg!(layer.0,"setFramebufferOnly:"=>(),bool=>false);
                msg!(layer.0,"setPresentsWithTransaction:"=>(),bool=>false);
                msg!(layer.0,"setMaximumDrawableCount:"=>(),usize=>2);
                msg!(layer.0,"setAllowsNextDrawableTimeout:"=>(),bool=>true);
                msg!(layer.0,"setDrawableSize:"=>(),Size=>Size{width:width as f64,height:height as f64});
                msg!(layer.0,"setFrame:"=>(),Rect=>Rect{origin:Point{x:0.0,y:0.0},size:Size{width:width as f64,height:height as f64}});
                let descriptor = checked(
                    msg!(checked(objc_getClass(c"MTLTextureDescriptor".as_ptr()))?,"texture2DDescriptorWithPixelFormat:width:height:mipmapped:"=>Obj,usize=>80,usize=>width as usize,usize=>height as usize,bool=>false),
                )?;
                msg!(descriptor,"setStorageMode:"=>(),usize=>0);
                let texture =
                    owned(msg!(device.0,"newTextureWithDescriptor:"=>Obj,Obj=>descriptor))?;
                let allocated =
                    checked(msg!(checked(objc_getClass(c"NSWindow".as_ptr()))?,"alloc"=>Obj))?;
                let window = owned(
                    msg!(allocated,"initWithContentRect:styleMask:backing:defer:"=>Obj,Rect=>Rect{origin:Point{x:100.0,y:100.0},size:Size{width:width as f64,height:height as f64}},usize=>3,usize=>2,bool=>false),
                )?;
                msg!(window.0,"setReleasedWhenClosed:"=>(),bool=>false);
                // Keep the bounded verification window visible even when the
                // invoking application occupies a full-screen desktop space.
                msg!(window.0,"setCollectionBehavior:"=>(),usize=>257);
                msg!(window.0,"setLevel:"=>(),isize=>3);
                msg!(window.0,"setTitle:"=>(),Obj=>text(c"Rust Metal presentation verification")?);
                let view = checked(msg!(window.0,"contentView"=>Obj))?;
                msg!(view,"setLayer:"=>(),Obj=>layer.0);
                msg!(view,"setWantsLayer:"=>(),bool=>true);
                msg!(window.0,"makeKeyAndOrderFront:"=>(),Obj=>ptr::null_mut());
                msg!(window.0,"orderFrontRegardless"=>());
                msg!(app,"activateIgnoringOtherApps:"=>(),bool=>true);
                launch_loop(app)?;
                let ready = Instant::now() + Duration::from_millis(250);
                while Instant::now() < ready {
                    launch_loop(app)?;
                    events(app)?;
                }
                Ok(Self {
                    app,
                    window: Some(window),
                    layer,
                    _device: device,
                    queue,
                    texture,
                    width,
                    height,
                    name,
                    faulted: false,
                    _thread: std::marker::PhantomData,
                })
            }
        }
        pub fn present(&mut self, rgba: &[u8]) -> Result<Evidence> {
            let deadline = Instant::now() + Duration::from_secs(3);
            for attempts in 1..=3 {
                if Instant::now() >= deadline {
                    break;
                }
                match self.present_once(rgba, deadline) {
                    Ok(mut evidence) => {
                        evidence.attempts = attempts;
                        return Ok(evidence);
                    }
                    Err(error) if error == "Drawable skipped by compositor" => {
                        self.pump(Duration::from_millis(100))?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err("Drawable retry count or presentation deadline exhausted".into())
        }
        fn present_once(&mut self, rgba: &[u8], deadline: Instant) -> Result<Evidence> {
            main_thread()?;
            if self.window.is_none() || self.faulted {
                return Err("Presenter closed or faulted".into());
            }
            if rgba.len() != self.width as usize * self.height as usize * 4 {
                return Err("Presenter RGBA length mismatch".into());
            }
            let mut pixels = rgba.to_vec();
            for p in pixels.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            unsafe {
                let _pool = pool()?;
                events(self.app)?;
                let window = self.window.as_ref().unwrap();
                if !msg!(window.0,"isVisible"=>bool) {
                    return Err("Presenter window is not visible".into());
                }
                let drawable = checked(msg!(self.layer.0,"nextDrawable"=>Obj))?;
                FINISHED_DRAWABLE.store(0, std::sync::atomic::Ordering::Release);
                msg!(drawable,"addPresentedHandler:"=>(),*const PresentedBlock=>&PRESENTED_BLOCK);
                let target = checked(msg!(drawable,"texture"=>Obj))?;
                let region = Region {
                    origin: Extent { x: 0, y: 0, z: 0 },
                    size: Extent {
                        x: self.width as usize,
                        y: self.height as usize,
                        z: 1,
                    },
                };
                msg!(self.texture.0,"replaceRegion:mipmapLevel:withBytes:bytesPerRow:"=>(),Region=>region,usize=>0,*const c_void=>pixels.as_ptr().cast(),usize=>self.width as usize*4);
                let command = checked(msg!(self.queue.0,"commandBuffer"=>Obj))?;
                let blit = checked(msg!(command,"blitCommandEncoder"=>Obj))?;
                msg!(blit,"copyFromTexture:toTexture:"=>(),Obj=>self.texture.0,Obj=>target);
                msg!(blit,"endEncoding"=>());
                msg!(command,"presentDrawable:"=>(),Obj=>drawable);
                msg!(command,"commit"=>());
                loop {
                    launch_loop(self.app)?;
                    events(self.app)?;
                    let status = msg!(command,"status"=>usize);
                    if status == 5 {
                        self.faulted = true;
                        return Err("Metal presentation command failed".into());
                    }
                    let time = msg!(drawable,"presentedTime"=>f64);
                    if status == 4 && time.is_finite() && time > 0.0 {
                        return Ok(Evidence {
                            device_name: self.name.clone(),
                            presented_time: time,
                            drawable_id: msg!(drawable,"drawableID"=>usize),
                            attempts: 1,
                        });
                    }
                    if Instant::now() >= deadline {
                        eprintln!(
                            "Presented callback count={}",
                            PRESENT_CALLBACKS.load(std::sync::atomic::Ordering::Relaxed)
                        );
                        self.faulted = true;
                        let bounds = msg!(self.layer.0,"bounds"=>Rect);
                        return Err(format!(
                            "Drawable was not confirmed presented before deadline: command={status}, presentedTime={time}, occlusion={}, layer={}x{}, super={:?}",
                            msg!(window.0,"occlusionState"=>usize),
                            bounds.size.width,
                            bounds.size.height,
                            msg!(self.layer.0,"superlayer"=>Obj)
                        ));
                    }
                    if status == 4
                        && FINISHED_DRAWABLE.load(std::sync::atomic::Ordering::Acquire)
                            == drawable as usize
                        && time == 0.0
                    {
                        return Err("Drawable skipped by compositor".into());
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        pub fn pump(&self, duration: Duration) -> Result<()> {
            main_thread()?;
            if duration > Duration::from_secs(2) {
                return Err("Presenter pump exceeds2second limit".into());
            }
            unsafe {
                let _pool = pool()?;
                let end = Instant::now() + duration;
                while Instant::now() < end {
                    events(self.app)?;
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            Ok(())
        }
        pub fn close(&mut self) -> Result<()> {
            main_thread()?;
            if let Some(window) = self.window.take() {
                unsafe {
                    msg!(window.0,"close"=>());
                }
            }
            Ok(())
        }
        /// Gate keyboard polling to this owned window; never expose its host pointer.
        pub fn keyboard_focus(&self) -> Result<bool> {
            main_thread()?;
            let Some(window) = self.window.as_ref() else {
                return Ok(false);
            };
            unsafe {
                Ok(msg!(self.app,"isActive"=>bool)
                    && msg!(self.app,"keyWindow"=>Obj) == window.0
                    && msg!(window.0,"isVisible"=>bool))
            }
        }
        /// Retain the owned, focused window for an attached IME sheet. The
        /// caller must release exactly once on this main thread, must not expose
        /// this pointer to a guest, and must recheck visibility before use.
        pub(crate) unsafe fn retained_dialog_window(&self) -> Result<*mut c_void> {
            if !self.keyboard_focus()? {
                return Err("IME requires a focused, visible presenter window".into());
            }
            let window = self.window.as_ref().ok_or("Presenter is closed")?;
            unsafe { Ok(msg!(window.0,"retain"=>Obj)) }
        }
    }
    impl Drop for Presenter {
        fn drop(&mut self) {
            let _ = self.close();
        }
    }
}
