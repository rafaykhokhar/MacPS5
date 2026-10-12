//! Bounded real macOS PCM output using AudioQueue's C ABI.
//! Three owned fragments, 48 kHz, interleaved mono/stereo S16LE or F32LE.
//! Buffer callbacks mean reusable, NOT audible completion. Producer pacing uses
//! AudioQueueGetCurrentTime sample progress, with at most two fragments ahead.
//! https://developer.apple.com/documentation/audiotoolbox/audioqueueoutputcallback
//! https://developer.apple.com/documentation/audiotoolbox/audioqueuegetcurrenttime(_:_:_:_:)
//! Close cancels queued output synchronously; console drain semantics are not
//! inferred. Starvation is not filled with fabricated guest samples. A stalled
//! device faults within two seconds. Recovery/restart after underrun is not modeled.
#![allow(unsafe_code)]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub use apple::{AudioQueueBackend, Queue};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use crate::{
        Result,
        audio_out::{AudioBackend, Config},
    };
    use std::{
        ffi::c_void,
        ptr,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    #[derive(Default)]
    pub struct AudioQueueBackend;
    impl AudioBackend for AudioQueueBackend {
        type Stream = Queue;
        fn open(&mut self, config: Config) -> Result<Queue> {
            Queue::new(config.frames, config.channels, config.float)
        }
        fn write(&mut self, stream: &mut Queue, pcm: &[u8]) -> Result<()> {
            stream.write(pcm)
        }
        fn close(&mut self, stream: &mut Queue) -> Result<()> {
            stream.close()
        }
    }
    #[repr(C)]
    struct Format {
        rate: f64,
        id: u32,
        flags: u32,
        bytes_packet: u32,
        frames_packet: u32,
        bytes_frame: u32,
        channels: u32,
        bits: u32,
        reserved: u32,
    }
    #[repr(C)]
    struct Buffer {
        capacity: u32,
        data: *mut c_void,
        size: u32,
        user: *mut c_void,
        packet_capacity: u32,
        packets: *mut c_void,
        packet_count: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Timestamp {
        sample: f64,
        host: u64,
        scalar: f64,
        word: u64,
        smpte: [u8; 24],
        flags: u32,
        reserved: u32,
    }
    type AQ = *mut c_void;
    #[link(name = "AudioToolbox", kind = "framework")]
    unsafe extern "C" {
        fn AudioQueueNewOutput(
            format: *const Format,
            callback: unsafe extern "C" fn(*mut c_void, AQ, *mut Buffer),
            user: *mut c_void,
            run_loop: *mut c_void,
            mode: *mut c_void,
            flags: u32,
            out: *mut AQ,
        ) -> i32;
        fn AudioQueueAllocateBuffer(queue: AQ, size: u32, out: *mut *mut Buffer) -> i32;
        fn AudioQueueEnqueueBuffer(
            queue: AQ,
            buffer: *mut Buffer,
            packets: u32,
            descriptions: *const c_void,
        ) -> i32;
        fn AudioQueueStart(queue: AQ, time: *const Timestamp) -> i32;
        fn AudioQueueCreateTimeline(queue: AQ, timeline: *mut *mut c_void) -> i32;
        fn AudioQueueDisposeTimeline(queue: AQ, timeline: *mut c_void) -> i32;
        fn AudioQueueGetCurrentTime(
            queue: AQ,
            timeline: *mut c_void,
            time: *mut Timestamp,
            discontinuity: *mut u8,
        ) -> i32;
        fn AudioQueueStop(queue: AQ, immediate: u8) -> i32;
        fn AudioQueueDispose(queue: AQ, immediate: u8) -> i32;
    }
    struct Callback {
        addresses: [AtomicUsize; 3],
        free: [AtomicBool; 3],
        returned: AtomicUsize,
    }
    // A failed final disposal keeps this slot reserved permanently, preventing
    // repeated creation from accumulating abandoned native queues/callback state.
    static ACTIVE: AtomicBool = AtomicBool::new(false);
    unsafe extern "C" fn returned(user: *mut c_void, _queue: AQ, buffer: *mut Buffer) {
        // SAFETY: Box stays pinned until synchronous successful queue disposal.
        let state = unsafe { &*(user as *const Callback) };
        for i in 0..3 {
            if state.addresses[i].load(Ordering::Acquire) == buffer as usize {
                state.returned.fetch_add(1, Ordering::Relaxed);
                state.free[i].store(true, Ordering::Release);
                break;
            }
        }
    }
    fn status(code: i32, operation: &str) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(format!("{operation} failed with OSStatus {code}"))
        }
    }
    /// Exclusive queue owner. Host pointers never originate from guest addresses.
    /// Not Send/Sync; callbacks access only their pinned atomic bookkeeping.
    pub struct Queue {
        queue: AQ,
        timeline: *mut c_void,
        owns_slot: bool,
        last_clock: std::cell::Cell<f64>,
        clock_established: std::cell::Cell<bool>,
        clock_fault: std::cell::Cell<bool>,
        buffers: [*mut Buffer; 3],
        state: Option<Box<Callback>>,
        frames: u32,
        bytes: usize,
        submitted: u64,
        started: bool,
        faulted: bool,
        _owner: std::marker::PhantomData<std::rc::Rc<()>>,
    }
    impl Queue {
        pub fn new(frames: u32, channels: u32, float: bool) -> Result<Self> {
            if !(256..=2048).contains(&frames) || frames % 256 != 0 || !(1..=2).contains(&channels)
            {
                return Err("Unsupported AudioQueue PCM configuration".into());
            }
            if ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err("AudioQueue process slot occupied or quarantined".into());
            }
            let bytes = frames * channels * if float { 4 } else { 2 };
            let format = Format {
                rate: 48000.0,
                id: u32::from_be_bytes(*b"lpcm"),
                flags: 8 | if float { 1 } else { 4 },
                bytes_packet: channels * if float { 4 } else { 2 },
                frames_packet: 1,
                bytes_frame: channels * if float { 4 } else { 2 },
                channels,
                bits: if float { 32 } else { 16 },
                reserved: 0,
            };
            let state = Box::new(Callback {
                addresses: std::array::from_fn(|_| AtomicUsize::new(0)),
                free: std::array::from_fn(|_| AtomicBool::new(true)),
                returned: AtomicUsize::new(0),
            });
            let mut result = Self {
                queue: ptr::null_mut(),
                timeline: ptr::null_mut(),
                owns_slot: true,
                last_clock: std::cell::Cell::new(0.0),
                clock_established: std::cell::Cell::new(false),
                clock_fault: std::cell::Cell::new(false),
                buffers: [ptr::null_mut(); 3],
                state: Some(state),
                frames,
                bytes: bytes as usize,
                submitted: 0,
                started: false,
                faulted: false,
                _owner: std::marker::PhantomData,
            };
            let user = &mut **result.state.as_mut().unwrap() as *mut Callback;
            // SAFETY: SDK ABI structs and callback, pinned state, null run-loop uses
            // internal AudioQueue threads. Queue owns all allocated audio buffers.
            unsafe {
                status(
                    AudioQueueNewOutput(
                        &format,
                        returned,
                        user.cast(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        0,
                        &mut result.queue,
                    ),
                    "AudioQueueNewOutput",
                )?;
                if result.queue.is_null() {
                    return Err("AudioQueue returned null queue".into());
                }
                status(
                    AudioQueueCreateTimeline(result.queue, &mut result.timeline),
                    "AudioQueueCreateTimeline",
                )?;
                for i in 0..3 {
                    status(
                        AudioQueueAllocateBuffer(result.queue, bytes, &mut result.buffers[i]),
                        "AudioQueueAllocateBuffer",
                    )?;
                    let buffer = result.buffers[i];
                    if buffer.is_null() || (*buffer).data.is_null() || (*buffer).capacity < bytes {
                        return Err("AudioQueue invalid buffer allocation".into());
                    }
                    result.state.as_ref().unwrap().addresses[i]
                        .store(buffer as usize, Ordering::Release);
                }
            }
            Ok(result)
        }
        pub fn sample_time(&self) -> Result<f64> {
            if self.queue.is_null() || !self.started {
                return Err("AudioQueue is not running".into());
            }
            let mut time = Timestamp::default();
            let mut discontinuity = 0u8;
            unsafe {
                status(
                    AudioQueueGetCurrentTime(
                        self.queue,
                        self.timeline,
                        &mut time,
                        &mut discontinuity,
                    ),
                    "AudioQueueGetCurrentTime",
                )?;
            }
            // Starting the queue establishes its first timeline epoch. Thereafter,
            // discontinuities never silently reset producer pacing or replay data.
            if (self.clock_established.get()
                && (discontinuity != 0 || time.sample < self.last_clock.get()))
                || self.clock_fault.get()
            {
                self.clock_fault.set(true);
                return Err("AudioQueue clock discontinuity or backwards progress".into());
            }
            if time.flags & 1 == 0 || !time.sample.is_finite() || time.sample < 0.0 {
                return Err("AudioQueue sample clock unavailable".into());
            }
            self.last_clock.set(time.sample);
            self.clock_established.set(true);
            Ok(time.sample)
        }
        pub fn returned_buffers(&self) -> usize {
            self.state
                .as_ref()
                .map_or(0, |s| s.returned.load(Ordering::Acquire))
        }
        pub fn write(&mut self, pcm: &[u8]) -> Result<()> {
            if self.queue.is_null() || self.faulted || self.clock_fault.get() {
                return Err("AudioQueue closed or faulted".into());
            }
            if pcm.len() != self.bytes {
                return Err("AudioQueue fragment length mismatch".into());
            }
            let next = self
                .submitted
                .checked_add(u64::from(self.frames))
                .ok_or("AudioQueue frame count overflow")?;
            let deadline = Instant::now() + Duration::from_secs(2);
            let slot = loop {
                let clock_ready = !self.started
                    || next <= u64::from(self.frames) * 2
                    || self.sample_time().is_ok_and(|time| {
                        time >= next.saturating_sub(u64::from(self.frames) * 2) as f64
                    });
                if self.clock_fault.get() {
                    self.faulted = true;
                    return Err("AudioQueue sample clock discontinuity".into());
                }
                if clock_ready {
                    if let Some(i) = self
                        .state
                        .as_ref()
                        .unwrap()
                        .free
                        .iter()
                        .position(|free| free.load(Ordering::Acquire))
                    {
                        break i;
                    }
                }
                if Instant::now() >= deadline {
                    self.faulted = true;
                    return Err("AudioQueue stalled: bounded buffer/clock wait expired".into());
                }
                std::thread::sleep(Duration::from_millis(1));
            };
            let state = self.state.as_ref().unwrap();
            state.free[slot].store(false, Ordering::Release);
            unsafe {
                let buffer = self.buffers[slot];
                ptr::copy_nonoverlapping(pcm.as_ptr(), (*buffer).data.cast(), pcm.len());
                (*buffer).size = pcm.len() as u32;
                if let Err(error) = status(
                    AudioQueueEnqueueBuffer(self.queue, buffer, 0, ptr::null()),
                    "AudioQueueEnqueueBuffer",
                ) {
                    self.faulted = true;
                    return Err(error);
                }
                self.submitted = next;
                if !self.started {
                    if let Err(error) =
                        status(AudioQueueStart(self.queue, ptr::null()), "AudioQueueStart")
                    {
                        self.faulted = true;
                        return Err(error);
                    }
                    self.started = true;
                }
            }
            Ok(())
        }
        /// Immediate cancellation, not drain. Failed disposal retains callback state
        /// and queue ownership, allowing a later close retry.
        pub fn close(&mut self) -> Result<()> {
            if self.queue.is_null() {
                if self.owns_slot {
                    ACTIVE.store(false, Ordering::Release);
                    self.owns_slot = false;
                }
                return Ok(());
            }
            self.faulted = true;
            unsafe {
                status(AudioQueueStop(self.queue, 1), "AudioQueueStop")?;
                if !self.timeline.is_null() {
                    status(
                        AudioQueueDisposeTimeline(self.queue, self.timeline),
                        "AudioQueueDisposeTimeline",
                    )?;
                    self.timeline = ptr::null_mut();
                }
                status(AudioQueueDispose(self.queue, 1), "AudioQueueDispose")?;
            }
            self.queue = ptr::null_mut();
            self.buffers = [ptr::null_mut(); 3];
            self.state.take();
            ACTIVE.store(false, Ordering::Release);
            self.owns_slot = false;
            Ok(())
        }
    }
    impl Drop for Queue {
        fn drop(&mut self) {
            if self.close().is_err() {
                // Last-resort quarantine: freeing callback state after failed native
                // disposal could cause UAF. Leak this bounded state/queue instead.
                if let Some(state) = self.state.take() {
                    let _ = Box::into_raw(state);
                }
            }
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[derive(Default)]
pub struct AudioQueueBackend;
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
pub struct Queue;
#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
impl crate::audio_out::AudioBackend for AudioQueueBackend {
    type Stream = Queue;
    fn open(&mut self, _: crate::audio_out::Config) -> crate::Result<Queue> {
        Err("AudioQueue backend requires Apple Silicon macOS".into())
    }
    fn write(&mut self, _: &mut Queue, _: &[u8]) -> crate::Result<()> {
        Err("AudioQueue backend requires Apple Silicon macOS".into())
    }
    fn close(&mut self, _: &mut Queue) -> crate::Result<()> {
        Err("AudioQueue backend requires Apple Silicon macOS".into())
    }
}
