//! Guarded PCM frontend for the PS5 SDL audio subset.
//! ABI/config evidence:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/audio/ps5/SDL_ps5audio.h
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/audio/ps5/SDL_ps5audio.c
//! Supports only main/system, 48kHz, 256..2048 frames in steps of 256,
//! little-endian signed16 or float32 mono/stereo. This is a bounded subset,
//! not the console's complete accepted format set. Errors are runtime diagnostics.
//! Pinned crispy-doom.elf confirms Open at 0xf3e05 (system255/main0/index0,
//! 48000Hz), Output tail jump at 0xf3e8d (handle, PCM pointer), and Close at
//! 0xf3ed6. SDL leaves WaitDevice empty because Output performs pacing.
use crate::{Result, strings::Memory};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub frames: u32,
    pub channels: u32,
    pub float: bool,
}
impl Config {
    pub fn fragment_bytes(self) -> Result<usize> {
        if !(256..=2048).contains(&self.frames)
            || self.frames % 256 != 0
            || !matches!(self.channels, 1 | 2)
        {
            return Err("Unsupported AudioOut fragment configuration".into());
        }
        Ok(self.frames as usize * self.channels as usize * if self.float { 4 } else { 2 })
    }
}

/// `write` is invoked exactly once per validated guest output. It must consume
/// or copy all PCM before returning. An error may follow an external effect;
/// the frontend poisons the stream and never resubmits that fragment.
/// `close` retains stream ownership on error and may be retried for cleanup.
/// Streams must release their resources on Drop if a process exits without Close.
pub trait AudioBackend {
    type Stream;
    fn open(&mut self, config: Config) -> Result<Self::Stream>;
    fn write(&mut self, stream: &mut Self::Stream, pcm: &[u8]) -> Result<()>;
    fn close(&mut self, stream: &mut Self::Stream) -> Result<()>;
}
struct Port<S> {
    handle: i32,
    config: Config,
    stream: S,
    poisoned: bool,
}
pub struct AudioOut<B: AudioBackend> {
    backend: B,
    initialized: bool,
    last_handle: i32,
    port: Option<Port<B::Stream>>,
}
impl<B: AudioBackend> AudioOut<B> {
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            initialized: false,
            last_handle: 0,
            port: None,
        }
    }
    pub fn initialized(&self) -> bool {
        self.initialized
    }
    pub fn active_count(&self) -> usize {
        usize::from(self.port.is_some())
    }
    pub fn initialize(&mut self) -> Result<i32> {
        if self.initialized {
            return Err("Unsupported sceAudioOutInit repeated initialization semantics".into());
        }
        self.initialized = true;
        Ok(0)
    }
    pub fn open(
        &mut self,
        user: i32,
        port_type: i32,
        index: i32,
        frames: u32,
        rate: u32,
        param: u32,
    ) -> Result<i32> {
        if !self.initialized {
            return Err("AudioOut is not initialized".into());
        }
        if user != 0xff || port_type != 0 || index != 0 || rate != 48000 {
            return Err("Unsupported sceAudioOutOpen user/port/index/rate".into());
        }
        let (channels, float) = match param {
            0 => (1, false),
            1 => (2, false),
            3 => (1, true),
            4 => (2, true),
            _ => return Err("Unsupported AudioOut format".into()),
        };
        let config = Config {
            frames,
            channels,
            float,
        };
        config.fragment_bytes()?;
        if self.port.is_some() {
            return Err("AudioOut single output port budget exhausted".into());
        }
        let handle = self
            .last_handle
            .checked_add(1)
            .ok_or("AudioOut handle budget exhausted")?;
        let stream = self.backend.open(config)?;
        self.port = Some(Port {
            handle,
            config,
            stream,
            poisoned: false,
        });
        self.last_handle = handle;
        Ok(handle)
    }
    pub fn output(&mut self, memory: &impl Memory, handle: i32, address: u64) -> Result<i32> {
        let port = self
            .port
            .as_mut()
            .filter(|port| port.handle == handle)
            .ok_or("AudioOut invalid or stale handle")?;
        if port.poisoned {
            return Err("AudioOut stream is faulted after backend failure".into());
        }
        if address == 0 {
            return Err("Unsupported sceAudioOutOutput null/drain semantics".into());
        }
        let length = port.config.fragment_bytes()?;
        address
            .checked_add(length as u64 - 1)
            .ok_or("AudioOut PCM address overflow")?;
        let mut pcm = vec![0; length];
        memory
            .read_into(address, &mut pcm)
            .map_err(|fault| format!("AudioOut PCM read fault: {fault:?}"))?;
        if let Err(error) = self.backend.write(&mut port.stream, &pcm) {
            port.poisoned = true;
            return Err(error);
        }
        Ok(0)
    }
    pub fn close(&mut self, handle: i32) -> Result<i32> {
        let port = self
            .port
            .as_mut()
            .filter(|port| port.handle == handle)
            .ok_or("AudioOut invalid or stale handle")?;
        if let Err(error) = self.backend.close(&mut port.stream) {
            port.poisoned = true;
            return Err(error);
        }
        self.port = None;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Backend {
        opens: usize,
        writes: Vec<Vec<u8>>,
        closes: usize,
        fail_open: bool,
        fail_write: bool,
        fail_close: bool,
    }
    impl AudioBackend for Backend {
        type Stream = Config;
        fn open(&mut self, config: Config) -> Result<Config> {
            self.opens += 1;
            if self.fail_open {
                Err("open".into())
            } else {
                Ok(config)
            }
        }
        fn write(&mut self, _: &mut Config, pcm: &[u8]) -> Result<()> {
            self.writes.push(pcm.to_vec());
            if self.fail_write {
                Err("write".into())
            } else {
                Ok(())
            }
        }
        fn close(&mut self, _: &mut Config) -> Result<()> {
            self.closes += 1;
            if self.fail_close {
                Err("close".into())
            } else {
                Ok(())
            }
        }
    }
    #[test]
    fn configurations_lifecycle_and_handles_are_bounded() {
        let mut audio = AudioOut::new(Backend::default());
        assert!(audio.open(255, 0, 0, 256, 48000, 0).is_err());
        assert_eq!(audio.initialize().unwrap(), 0);
        assert!(audio.initialize().is_err());
        for args in [
            (1, 0, 0, 256, 48000, 0),
            (255, 1, 0, 256, 48000, 0),
            (255, 0, 1, 256, 48000, 0),
            (255, 0, 0, 256, 44100, 0),
            (255, 0, 0, 256, 48000, 2),
            (255, 0, 0, 0, 48000, 0),
            (255, 0, 0, 257, 48000, 0),
            (255, 0, 0, 2304, 48000, 0),
        ] {
            assert!(
                audio
                    .open(args.0, args.1, args.2, args.3, args.4, args.5)
                    .is_err()
            );
        }
        assert_eq!(audio.backend.opens, 0);
        let mut previous = 0;
        for param in [0, 1, 3, 4] {
            for frames in (256..=2048).step_by(256) {
                let handle = audio.open(255, 0, 0, frames, 48000, param).unwrap();
                assert!(handle > previous);
                previous = handle;
                assert_eq!(audio.active_count(), 1);
                assert!(audio.open(255, 0, 0, 256, 48000, 0).is_err());
                assert!(audio.close(handle + 1).is_err());
                assert_eq!(audio.close(handle).unwrap(), 0);
                assert!(audio.close(handle).is_err());
                assert_eq!(audio.active_count(), 0);
            }
        }
        audio.last_handle = i32::MAX;
        let opens = audio.backend.opens;
        assert!(audio.open(255, 0, 0, 256, 48000, 0).is_err());
        assert_eq!(audio.backend.opens, opens);
    }
    #[test]
    fn full_guarded_pcm_read_precedes_one_backend_submission() {
        let mut audio = AudioOut::new(Backend::default());
        audio.initialize().unwrap();
        let handle = audio.open(255, 0, 0, 256, 48000, 1).unwrap();
        let mut bytes: Vec<u8> = (0..1024).map(|i| i as u8).collect();
        let expected = bytes.clone();
        {
            let mut memory = crate::guest_memory::Memory::default();
            memory.add(0x1000, &mut bytes, false, false).unwrap();
            for address in [0, 1, u64::MAX, 0x1001] {
                assert!(audio.output(&memory, handle, address).is_err());
                assert!(audio.backend.writes.is_empty());
            }
            assert!(audio.output(&memory, handle + 1, 0x1000).is_err());
            assert_eq!(audio.output(&memory, handle, 0x1000).unwrap(), 0);
        }
        bytes.fill(0);
        assert_eq!(audio.backend.writes, vec![expected]);
    }
    #[test]
    fn backend_failures_preserve_ownership_and_never_repeat_pcm() {
        let mut audio = AudioOut::new(Backend {
            fail_open: true,
            ..Backend::default()
        });
        audio.initialize().unwrap();
        assert!(audio.open(255, 0, 0, 256, 48000, 0).is_err());
        assert_eq!(audio.active_count(), 0);
        assert_eq!(audio.last_handle, 0);
        audio.backend.fail_open = false;
        let handle = audio.open(255, 0, 0, 256, 48000, 0).unwrap();
        let mut bytes = [0; 512];
        let mut memory = crate::guest_memory::Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        audio.backend.fail_write = true;
        assert!(audio.output(&memory, handle, 0x1000).is_err());
        assert!(audio.output(&memory, handle, 0x1000).is_err());
        assert_eq!(audio.backend.writes.len(), 1);
        audio.backend.fail_close = true;
        assert!(audio.close(handle).is_err());
        assert_eq!(audio.active_count(), 1);
        audio.backend.fail_close = false;
        audio.close(handle).unwrap();
        assert_eq!(audio.active_count(), 0);
        assert!(audio.output(&memory, handle, 0x1000).is_err());
        assert_eq!(audio.backend.writes.len(), 1);
    }
}
