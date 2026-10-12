use std::ffi::{CStr, CString, c_char, c_void};

type Obj = *mut c_void;
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend();
    fn objc_getClass(name: *const c_char) -> Obj;
    fn sel_registerName(name: *const c_char) -> Obj;
}
#[link(name = "Metal", kind = "framework")]
unsafe extern "C" {
    fn MTLCreateSystemDefaultDevice() -> Obj;
}
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}

// All message signatures below match the public Foundation/Metal ABI on ARM64.
// Calls only use host-owned strings/buffers and validated, non-null objects.
macro_rules! msg {
    ($obj:expr, $sel:literal => $ret:ty $(, $ty:ty => $arg:expr)*) => {{
        let selector = sel_registerName(concat!($sel, "\0").as_ptr().cast());
        let call: unsafe extern "C" fn(Obj, Obj $(, $ty)*) -> $ret =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        call($obj, selector $(, $arg)*)
    }};
}
struct Owned(Obj);
impl Owned {
    fn checked(object: Obj) -> Self {
        assert!(!object.is_null(), "Metal object creation failed");
        Self(object)
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            msg!(self.0, "release" => ());
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Size {
    width: usize,
    height: usize,
    depth: usize,
}
unsafe fn string(value: &str) -> Obj {
    let value = CString::new(value).unwrap();
    unsafe {
        let class = objc_getClass(c"NSString".as_ptr());
        msg!(class, "stringWithUTF8String:" => Obj, *const c_char => value.as_ptr())
    }
}
unsafe fn library(device: Obj, source: &str) -> Owned {
    unsafe {
        let mut error: Obj = std::ptr::null_mut();
        let object = msg!(device, "newLibraryWithSource:options:error:" => Obj,
            Obj => string(source), Obj => std::ptr::null_mut(), *mut Obj => &mut error);
        if object.is_null() && !error.is_null() {
            let description = msg!(error, "localizedDescription" => Obj);
            let bytes = msg!(description, "UTF8String" => *const c_char);
            panic!(
                "Metal compilation failed: {}",
                CStr::from_ptr(bytes).to_string_lossy()
            );
        }
        Owned::checked(object)
    }
}
