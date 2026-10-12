//! Bounded HLE for the pinned SDL VideoOut call sequence. Descriptor stride32
//! and opaque attribute size80 are from PS5 SDL SDL_ps5video.h. We deliberately
//! store an owned attribute token, NOT a guessed console attribute layout.
//! https://github.com/ps5-payload-dev/SDL/blob/8c56053f13ca13a0c050de613706ff69eb615836/src/video/ps5/SDL_ps5video.h
//! Flip event field interpretation is injected host policy until console ABI
//! evidence exists. Completion is emitted only after actual presentation.
use crate::{
    Result,
    direct_memory::DirectMemory,
    equeue::{Equeues, Event, MAX_EVENTS},
    guest_memory::Memory,
    metal_present::Evidence,
    video_tile,
};

pub trait Backend {
    type Surface;
    fn open(&mut self, width: u32, height: u32) -> Result<Self::Surface>;
    fn present(&mut self, surface: &mut Self::Surface, rgba: &[u8]) -> Result<Evidence>;
    fn close(&mut self, surface: &mut Self::Surface) -> Result<()>;
}
#[derive(Default)]
pub struct MetalBackend;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Backend for MetalBackend {
    type Surface = crate::metal_present::Presenter;
    fn open(&mut self, w: u32, h: u32) -> Result<Self::Surface> {
        Self::Surface::new(w, h)
    }
    fn present(&mut self, s: &mut Self::Surface, p: &[u8]) -> Result<Evidence> {
        s.present(p)
    }
    fn close(&mut self, s: &mut Self::Surface) -> Result<()> {
        s.close()
    }
}
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl Backend for MetalBackend {
    type Surface = ();
    fn open(&mut self, _: u32, _: u32) -> Result<()> {
        Err("VideoOut presentation requires macOS ARM64".into())
    }
    fn present(&mut self, _: &mut (), _: &[u8]) -> Result<Evidence> {
        Err("VideoOut presentation requires macOS ARM64".into())
    }
    fn close(&mut self, _: &mut ()) -> Result<()> {
        Err("VideoOut presentation requires macOS ARM64".into())
    }
}
/// Explicit integration policy, not an assertion of undocumented PS5 fields.
#[derive(Clone, Copy)]
pub struct FlipEventPolicy {
    pub ident: u64,
    pub filter: i16,
    pub flags: u16,
    pub fflags: u32,
}
struct Attribute {
    address: u64,
    token: [u8; 80],
    width: u32,
    height: u32,
}
struct Buffer {
    address: u64,
    pin: u64,
}
struct Port<S> {
    handle: i32,
    surface: Option<S>,
    buffers: Vec<Buffer>,
    dimensions: Option<(u32, u32)>,
    event: Option<(u64, u64)>,
    poisoned: bool,
}
pub struct VideoOut<B: Backend> {
    backend: B,
    port: Option<Port<B::Surface>>,
    attribute: Option<Attribute>,
    generation: u64,
    last_handle: i32,
    event_policy: Option<FlipEventPolicy>,
    last_evidence: Option<Evidence>,
}
impl<B: Backend> VideoOut<B> {
    pub fn new(backend: B, event_policy: Option<FlipEventPolicy>) -> Self {
        Self {
            backend,
            port: None,
            attribute: None,
            generation: 0,
            last_handle: 0,
            event_policy,
            last_evidence: None,
        }
    }
    pub fn last_evidence(&self) -> Option<&Evidence> {
        self.last_evidence.as_ref()
    }
    /// Configure only between port lifetimes, never reinterpret an active
    /// subscription. Already queued events remain owned by Equeues unchanged.
    pub fn configure_event_policy(&mut self, policy: Option<FlipEventPolicy>) -> Result<()> {
        if self.port.is_some() {
            return Err("Cannot change flip policy while VideoOut is open".into());
        }
        self.event_policy = policy;
        Ok(())
    }
    fn port(&mut self, handle: i32) -> Result<&mut Port<B::Surface>> {
        self.port
            .as_mut()
            .filter(|p| p.handle == handle)
            .ok_or_else(|| "Invalid VideoOut handle".into())
    }
    pub fn open(&mut self, user: i32, bus: i32, index: i32, param: u64) -> Result<i32> {
        if user != 0xff || bus != 0 || index != 0 || param != 0 || self.port.is_some() {
            return Err("Unsupported VideoOut open parameters/state".into());
        }
        let handle = self
            .last_handle
            .checked_add(1)
            .ok_or("VideoOut handle exhaustion")?;
        self.port = Some(Port {
            handle,
            surface: None,
            buffers: Vec::new(),
            dimensions: None,
            event: None,
            poisoned: false,
        });
        self.last_handle = handle;
        Ok(handle)
    }
    pub fn set_flip_rate(&mut self, handle: i32, rate: i32) -> Result<i32> {
        self.port(handle)?;
        if rate != 0 {
            return Err("Unsupported flip rate".into());
        }
        Ok(0)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn set_buffer_attribute(
        &mut self,
        memory: &mut Memory<'_>,
        output: u64,
        format: u64,
        tiling: u32,
        width: u32,
        height: u32,
        options: u64,
        unused: u32,
        reserved: u64,
    ) -> Result<()> {
        if output == 0
            || format != 0x8000000022000000
            || tiling != 0
            || options != 0
            || unused != 0
            || reserved != 0
        {
            return Err("Unsupported VideoOut attributes".into());
        }
        video_tile::required_bytes(width, height)?;
        memory.validate_write(output, 80)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or("VideoOut attribute token exhaustion")?;
        let mut token = [0; 80];
        token[..8].copy_from_slice(b"RUSTVOUT");
        token[8..16].copy_from_slice(&generation.to_le_bytes());
        memory.write(output, &token)?;
        self.attribute = Some(Attribute {
            address: output,
            token,
            width,
            height,
        });
        self.generation = generation;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn register_buffers(
        &mut self,
        memory: &Memory<'_>,
        direct: &mut DirectMemory,
        handle: i32,
        set: i32,
        start: i32,
        descriptors: u64,
        count: i32,
        attribute: u64,
        category: i32,
        option: u64,
    ) -> Result<i32> {
        if set != 0 || start != 0 || count != 2 || category != 0 || option != 0 || descriptors == 0
        {
            return Err("Unsupported VideoOut registration".into());
        }
        let p = self.port(handle)?;
        if p.surface.is_some() || p.poisoned {
            return Err("VideoOut already registered/faulted".into());
        }
        let a = self
            .attribute
            .as_ref()
            .ok_or("Missing owned VideoOut attribute")?;
        if a.address != attribute || memory.read(attribute, 80)? != a.token {
            return Err("Stale/copied/mutated VideoOut attribute token".into());
        }
        let (width, height) = (a.width, a.height);
        let size = video_tile::required_bytes(width, height)?;
        let raw = memory.read(descriptors, 64)?;
        let mut addresses = Vec::new();
        for descriptor in raw.chunks_exact(32) {
            if descriptor[8..].iter().any(|&b| b != 0) {
                return Err("Unsupported VideoOut descriptor fields".into());
            }
            let address = u64::from_le_bytes(descriptor[..8].try_into().unwrap());
            memory.read(address, size)?;
            addresses.push(address);
        }
        if addresses[0].abs_diff(addresses[1]) < size as u64 {
            return Err("Overlapping VideoOut buffers".into());
        }
        let mut buffers: Vec<Buffer> = Vec::new();
        for address in addresses {
            match direct.pin(address, size as u64) {
                Ok(pin) => buffers.push(Buffer { address, pin }),
                Err(e) => {
                    for b in buffers {
                        direct.unpin(b.pin)?;
                    }
                    return Err(e);
                }
            }
        }
        let surface = match self.backend.open(width, height) {
            Ok(s) => s,
            Err(e) => {
                for b in buffers {
                    direct.unpin(b.pin)?;
                }
                return Err(e);
            }
        };
        let p = self.port(handle)?;
        p.surface = Some(surface);
        p.buffers = buffers;
        p.dimensions = Some((width, height));
        Ok(0)
    }
    pub fn add_flip_event(
        &mut self,
        queues: &Equeues,
        queue: u64,
        handle: i32,
        udata: u64,
    ) -> Result<i32> {
        if self.event_policy.is_none() {
            return Err("Flip event ABI policy not configured".into());
        }
        queues.pending_count(queue)?;
        let p = self.port(handle)?;
        if p.event.is_some() {
            return Err("Flip event already registered".into());
        }
        p.event = Some((queue, udata));
        Ok(0)
    }
    pub fn delete_flip_event(&mut self, queue: u64, handle: i32) -> Result<i32> {
        let p = self.port(handle)?;
        if p.event.map(|e| e.0) != Some(queue) {
            return Err("Unknown flip event subscription".into());
        }
        p.event = None;
        Ok(0)
    }
    pub fn submit_flip(
        &mut self,
        memory: &Memory<'_>,
        queues: &mut Equeues,
        handle: i32,
        index: i32,
        mode: u32,
        argument: i64,
    ) -> Result<i32> {
        if mode != 1 || !(0..2).contains(&index) {
            return Err("Unsupported flip mode/index".into());
        }
        let p = self.port(handle)?;
        if p.poisoned {
            return Err("VideoOut presentation faulted; close required".into());
        }
        let (queue, udata) = p.event.ok_or("Flip completion subscription required")?;
        if queues.pending_count(queue)? >= MAX_EVENTS {
            return Err("Flip queue full before presentation".into());
        }
        let (width, height) = p.dimensions.ok_or("VideoOut buffers not registered")?;
        let pixels = video_tile::untile_rgba(
            memory.read(
                p.buffers[index as usize].address,
                video_tile::required_bytes(width, height)?,
            )?,
            width,
            height,
        )?;
        let p = self.port.as_mut().unwrap();
        // Any backend failure may follow a visible side effect: forbid retries.
        p.poisoned = true;
        let evidence = self
            .backend
            .present(p.surface.as_mut().ok_or("Missing presenter")?, &pixels)?;
        if !evidence.presented_time.is_finite() || evidence.presented_time <= 0.0 {
            return Err("Backend lacks actual presentation confirmation".into());
        }
        let policy = self.event_policy.ok_or("Missing flip event policy")?;
        queues.enqueue(
            queue,
            Event {
                ident: policy.ident,
                filter: policy.filter,
                flags: policy.flags,
                fflags: policy.fflags,
                data: argument,
                udata,
            },
        )?;
        p.poisoned = false;
        self.last_evidence = Some(evidence);
        Ok(0)
    }
    pub fn close(&mut self, direct: &mut DirectMemory, handle: i32) -> Result<()> {
        self.port(handle)?;
        let p = self.port.as_mut().unwrap();
        p.poisoned = true;
        if let Some(surface) = p.surface.as_mut() {
            self.backend.close(surface)?;
        }
        // A close failure retains both surface and pins for cleanup retry.
        for b in &p.buffers {
            direct.unpin(b.pin)?;
        }
        self.port = None;
        self.last_evidence = None;
        Ok(())
    }
}
