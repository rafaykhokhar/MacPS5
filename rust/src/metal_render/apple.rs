//! Private host-only Objective-C ABI; no guest pointers cross this module.
use super::{Frame, Inputs};
use crate::{Result, render_plan::DrawPlan};
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
    fn checked(object: Obj) -> Result<Self> {
        if object.is_null() {
            return Err("Metal object creation failed".into());
        }
        Ok(Self(object))
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
unsafe fn string(value: &str) -> Result<Obj> {
    let value = CString::new(value).map_err(|_| "Metal source contains NUL")?;
    unsafe {
        let class = objc_getClass(c"NSString".as_ptr());
        nonnull(msg!(class, "stringWithUTF8String:" => Obj, *const c_char => value.as_ptr()))
    }
}
unsafe fn library(device: Obj, source: &str) -> Result<Owned> {
    unsafe {
        let mut error: Obj = std::ptr::null_mut();
        let object = msg!(device, "newLibraryWithSource:options:error:" => Obj,
            Obj => string(source)?, Obj => std::ptr::null_mut(), *mut Obj => &mut error);
        if object.is_null() && !error.is_null() {
            let description = msg!(error, "localizedDescription" => Obj);
            let bytes = msg!(description, "UTF8String" => *const c_char);
            return Err(format!(
                "Metal compilation failed: {}",
                if bytes.is_null() {
                    "unknown error".into()
                } else {
                    CStr::from_ptr(bytes).to_string_lossy()
                }
            ));
        }
        Owned::checked(object)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ClearColor {
    red: f64,
    green: f64,
    blue: f64,
    alpha: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Region {
    origin: Size,
    size: Size,
}

fn nonnull(object: Obj) -> Result<Obj> {
    if object.is_null() {
        Err("Metal object creation failed".into())
    } else {
        Ok(object)
    }
}
unsafe fn error_message(error: Obj, operation: &str) -> String {
    if error.is_null() {
        return operation.into();
    }
    unsafe {
        let description = msg!(error,"localizedDescription" => Obj);
        let bytes = msg!(description,"UTF8String" => *const c_char);
        if bytes.is_null() {
            operation.into()
        } else {
            format!("{operation}: {}", CStr::from_ptr(bytes).to_string_lossy())
        }
    }
}
pub(super) fn render(
    plans: &[DrawPlan],
    inputs: Inputs<'_>,
    width: u32,
    height: u32,
) -> Result<Frame> {
    // SAFETY: Narrow Foundation/Metal ABI on ARM64, fixed-size host-owned buffers,
    // validated draw ranges, bounded texture and synchronous completion before
    // CPU readback. No guest address is dereferenced through this interface.
    unsafe {
        let _pool =
            Owned::checked(msg!(objc_getClass(c"NSAutoreleasePool".as_ptr()),"new" => Obj))?;
        let device = Owned::checked(MTLCreateSystemDefaultDevice())?;
        let name = nonnull(msg!(device.0,"name" => Obj))?;
        let name_bytes = msg!(name,"UTF8String" => *const c_char);
        if name_bytes.is_null() {
            return Err("Metal device name unavailable".into());
        }
        let device_name = CStr::from_ptr(name_bytes).to_string_lossy().into_owned();
        if !msg!(device.0,"hasUnifiedMemory" => bool) {
            return Err("Apple unified-memory GPU required".into());
        }
        // Build every pipeline before encoding any draw, preserving submission
        // validation atomicity even when a later shader fails Metal compilation.
        let mut pipelines = Vec::new();
        for plan in plans {
            let library = library(
                device.0,
                &format!("{}\n{}", plan.vertex_msl, plan.fragment_msl),
            )?;
            let vs = Owned::checked(
                msg!(library.0,"newFunctionWithName:" => Obj,Obj => string("rdna_vertex")?),
            )?;
            let fs = Owned::checked(
                msg!(library.0,"newFunctionWithName:" => Obj,Obj => string("rdna_fragment")?),
            )?;
            let descriptor = Owned::checked(
                msg!(objc_getClass(c"MTLRenderPipelineDescriptor".as_ptr()),"new" => Obj),
            )?;
            msg!(descriptor.0,"setVertexFunction:" => (),Obj => vs.0);
            msg!(descriptor.0,"setFragmentFunction:" => (),Obj => fs.0);
            let colors = nonnull(msg!(descriptor.0,"colorAttachments" => Obj))?;
            let color = nonnull(msg!(colors,"objectAtIndexedSubscript:" => Obj,usize => 0))?;
            msg!(color,"setPixelFormat:" => (),usize => 70);
            let mut error: Obj = std::ptr::null_mut();
            let object = msg!(device.0,"newRenderPipelineStateWithDescriptor:error:" => Obj,Obj => descriptor.0,*mut Obj => &mut error);
            if object.is_null() {
                return Err(error_message(error, "Metal pipeline creation failed"));
            }
            let pipeline = Owned::checked(object)?;
            pipelines.push(pipeline);
        }
        let buffers: Vec<Owned> = [
            inputs.vertices.as_flattened(),
            &inputs.fragment[..],
            &inputs.scalars[..],
        ]
        .iter()
        .map(|data| {
            Owned::checked(msg!(device.0,"newBufferWithBytes:length:options:" => Obj,
                *const u32 => data.as_ptr(),usize => std::mem::size_of_val(*data),usize => 0))
        })
        .collect::<Result<_>>()?;
        let descriptor = msg!(objc_getClass(c"MTLTextureDescriptor".as_ptr()),"texture2DDescriptorWithPixelFormat:width:height:mipmapped:" => Obj,
            usize => 70,usize => width as usize,usize => height as usize,bool => false);
        nonnull(descriptor)?;
        msg!(descriptor,"setStorageMode:" => (),usize => 0);
        msg!(descriptor,"setUsage:" => (),usize => 4);
        let texture =
            Owned::checked(msg!(device.0,"newTextureWithDescriptor:" => Obj,Obj => descriptor))?;
        let pass =
            msg!(objc_getClass(c"MTLRenderPassDescriptor".as_ptr()),"renderPassDescriptor" => Obj);
        nonnull(pass)?;
        let colors = nonnull(msg!(pass,"colorAttachments" => Obj))?;
        let color = nonnull(msg!(colors,"objectAtIndexedSubscript:" => Obj,usize => 0))?;
        msg!(color,"setTexture:" => (),Obj => texture.0);
        msg!(color,"setLoadAction:" => (),usize => 2);
        msg!(color,"setStoreAction:" => (),usize => 1);
        msg!(color,"setClearColor:" => (),ClearColor => ClearColor{red:0.0,green:0.0,blue:0.0,alpha:1.0});
        let pixel_bytes = width as usize * height as usize * 4;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(pixel_bytes)
            .map_err(|_| "Metal readback allocation failed")?;
        pixels.resize(pixel_bytes, 0_u8);
        let queue = Owned::checked(msg!(device.0,"newCommandQueue" => Obj))?;
        let command = msg!(queue.0,"commandBuffer" => Obj);
        nonnull(command)?;
        let encoder = msg!(command,"renderCommandEncoderWithDescriptor:" => Obj,Obj => pass);
        nonnull(encoder)?;
        msg!(encoder,"setVertexBuffer:offset:atIndex:" => (),Obj => buffers[0].0,usize => 0,usize => 0);
        msg!(encoder,"setVertexBuffer:offset:atIndex:" => (),Obj => buffers[2].0,usize => 0,usize => 1);
        msg!(encoder,"setFragmentBuffer:offset:atIndex:" => (),Obj => buffers[1].0,usize => 0,usize => 0);
        msg!(encoder,"setFragmentBuffer:offset:atIndex:" => (),Obj => buffers[2].0,usize => 0,usize => 1);
        for (plan, pipeline) in plans.iter().zip(&pipelines) {
            msg!(encoder,"setRenderPipelineState:" => (),Obj => pipeline.0);
            msg!(encoder,"drawPrimitives:vertexStart:vertexCount:instanceCount:" => (),
                usize => 3,usize => plan.first_vertex as usize,usize => plan.count as usize,usize => 1);
        }
        msg!(encoder,"endEncoding" => ());
        msg!(command,"commit" => ());
        msg!(command,"waitUntilCompleted" => ());
        if msg!(command,"status" => usize) != 4 {
            return Err(error_message(
                msg!(command,"error" => Obj),
                "Metal rendering did not complete",
            ));
        }
        msg!(texture.0,"getBytes:bytesPerRow:fromRegion:mipmapLevel:" => (),
            *mut u8 => pixels.as_mut_ptr(),usize => width as usize*4,
            Region => Region{origin:Size{width:0,height:0,depth:0},size:Size{width:width as usize,height:height as usize,depth:1}},usize => 0);

        Ok(Frame {
            width,
            height,
            device_name,
            pixels,
        })
    }
}
