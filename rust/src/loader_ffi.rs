//! Narrow ownership and callback ABI for the retained native mapping adapter.
#![allow(unsafe_code)]
use crate::loader::Image;
use std::ffi::c_void;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Bytes {
    pub data: *const u8,
    pub size: u64,
}
impl Bytes {
    fn of(bytes: &[u8]) -> Self {
        Self {
            data: bytes.as_ptr(),
            size: bytes.len() as u64,
        }
    }
}
#[repr(C)]
pub struct Resolution {
    pub value: u64,
    pub status: u64,
}
#[repr(C)]
pub struct Identity {
    pub name: Bytes,
    pub symbol: Bytes,
    pub module: Bytes,
    pub library_token: Bytes,
    pub module_id: u64,
    pub library: Bytes,
    /// 65536 is absent; all other values must fit u16.
    pub library_version: u64,
    /// Packed major:minor, or 65536 when absent.
    pub module_version: u64,
}
impl Identity {
    pub(crate) fn of(identity: &crate::imports::Identity) -> Self {
        Self {
            name: Bytes::of(identity.name.as_bytes()),
            symbol: Bytes::of(identity.symbol.as_bytes()),
            module: Bytes::of(identity.module.as_bytes()),
            library_token: Bytes::of(identity.library_token.as_bytes()),
            module_id: identity.module_id.map(u64::from).unwrap_or(65536),
            library: Bytes::of(
                identity
                    .library
                    .as_ref()
                    .map_or("", |v| v.name.as_str())
                    .as_bytes(),
            ),
            library_version: identity
                .library
                .as_ref()
                .map_or(65536, |v| u64::from(v.version)),
            module_version: identity.module_version.map_or(65536, |(major, minor)| {
                (u64::from(major) << 8) | u64::from(minor)
            }),
        }
    }
}
#[repr(C)]
pub struct Record {
    pub identity: Identity,
    pub address: u64,
    pub kind: u64,
}
#[repr(C)]
pub struct Patch {
    pub address: u64,
    pub value: u64,
}
/// # Safety
/// File is readable for size bytes (at most isize::MAX), callbacks never unwind,
/// and context remains valid/exclusive throughout this synchronous call.
/// Callback status: 0=resolved (including address zero), 1=missing, 2=error.
/// Returned pointer is owned and must be destroyed exactly once with delete_v3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_build_v3(
    file: *const u8,
    size: u64,
    bias: u64,
    budget: u64,
    context: *mut c_void,
    resolve: Option<unsafe extern "C" fn(*mut c_void, Identity) -> Resolution>,
) -> *mut Image {
    if file.is_null() || size > isize::MAX as u64 {
        return std::ptr::null_mut();
    }
    let Some(resolve) = resolve else {
        return std::ptr::null_mut();
    };
    // SAFETY: caller contract checked for null and representable slice length.
    let file = unsafe { std::slice::from_raw_parts(file, size as usize) };
    let Ok(budget) = usize::try_from(budget) else {
        return std::ptr::null_mut();
    };
    let Ok(mut image) = Image::load(file, bias, budget) else {
        return std::ptr::null_mut();
    };
    let mut callback_error = false;
    let result = image.relocate_scoped(|identity| {
        // SAFETY: live name bytes for the synchronous callback; callback contract above.
        let result = unsafe { resolve(context, Identity::of(identity)) };
        match result.status {
            0 => Some(result.value),
            1 => None,
            _ => {
                callback_error = true;
                None
            }
        }
    });
    if result.is_err() || callback_error {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(image))
}
/// # Safety
/// Pointer must be live from build_v3 and destroyed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_delete_v3(image: *mut Image) {
    if !image.is_null() {
        unsafe {
            drop(Box::from_raw(image));
        }
    }
}
/// # Safety
/// Image is a live immutable borrow of a pointer from build_v3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_count_v3(image: *const Image, kind: u64) -> u64 {
    let image = unsafe { &*image };
    match kind {
        0 => image.relocations.len() as u64,
        1 => image.unresolved.len() as u64,
        2 => image.dependencies.len() as u64,
        _ => 0,
    }
}
/// # Safety
/// Same live-pointer contract as count_v3. Index must be below count(kind=0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_patch_v3(image: *const Image, index: u64) -> Patch {
    let image = unsafe { &*image };
    let (address, value) = image.relocations[index as usize];
    Patch { address, value }
}
/// # Safety
/// Same live-pointer contract as count_v3. Index must be below count(kind=1).
/// Returned name bytes are immutable and valid only until delete_v3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_import_v3(image: *const Image, index: u64) -> Record {
    let image = unsafe { &*image };
    let entry = &image.unresolved[index as usize];
    Record {
        identity: Identity::of(&entry.identity),
        address: entry.address,
        kind: entry.kind as u64,
    }
}
/// # Safety
/// Same live-pointer contract as count_v3. Index must be below count(kind=2).
/// Returned bytes are immutable and valid only until delete_v3.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn native_ps5_image_dependency_v3(image: *const Image, index: u64) -> Bytes {
    let image = unsafe { &*image };
    Bytes::of(image.dependencies[index as usize].as_bytes())
}
