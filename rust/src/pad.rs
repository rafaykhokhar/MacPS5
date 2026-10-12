//! Process-local Pad lifecycle with explicitly supplied controller snapshots.
//!
//! Signatures and standard port type/index are established by PS5 SDL:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/joystick/ps5/SDL_ps5joystick.h
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/joystick/ps5/SDL_ps5joystick.c
//! These sources do not establish no-device or repeated-init return codes.
//! Such cases stop with runtime diagnostics instead of invented console errors.
//! Initialization owns an empty device set; it never discovers host devices.
use crate::{Result, pad_data::PadData, strings::Memory};

#[derive(Debug)]
struct Device {
    source: u64,
    user: i32,
    snapshot: PadData,
    handle: Option<i32>,
}

#[derive(Debug, Default)]
pub struct Pad {
    initialized: bool,
    devices: Vec<Device>,
    last_handle: i32,
}
impl Pad {
    pub fn initialized(&self) -> bool {
        self.initialized
    }
    pub fn connected_count(&self) -> usize {
        self.devices.iter().filter(|d| d.snapshot.connected).count()
    }

    /// At most 16 distinct source records per process. Retaining disconnected
    /// identities prevents accidental reassignment of a source to another user.
    /// A source can reconnect explicitly with its original user and a new handle.
    pub fn register_device(&mut self, source_id: u64, user: i32, snapshot: PadData) -> Result<()> {
        if source_id == 0 || user == -1 || !snapshot.connected {
            return Err("Invalid explicit Pad source/user/connection".into());
        }
        if self
            .devices
            .iter()
            .any(|d| d.user == user && d.snapshot.connected)
        {
            return Err("Pad user already has a connected source".into());
        }
        if let Some(device) = self.devices.iter_mut().find(|d| d.source == source_id) {
            if device.user != user || device.snapshot.connected {
                return Err("Pad source is already registered or assigned to another user".into());
            }
            device.snapshot = snapshot;
            device.handle = None;
        } else {
            if self.devices.len() == 16 {
                return Err("Pad source record budget exhausted".into());
            }
            self.devices.push(Device {
                source: source_id,
                user,
                snapshot,
                handle: None,
            });
        }
        Ok(())
    }
    pub fn update_device(&mut self, source_id: u64, snapshot: PadData) -> Result<()> {
        let device = self
            .devices
            .iter_mut()
            .find(|d| d.source == source_id)
            .ok_or("Unknown Pad source")?;
        if !device.snapshot.connected || !snapshot.connected {
            return Err("Pad update requires a connected source; disconnect explicitly".into());
        }
        device.snapshot = snapshot;
        Ok(())
    }
    pub fn disconnect_device(&mut self, source_id: u64) -> Result<()> {
        let device = self
            .devices
            .iter_mut()
            .find(|d| d.source == source_id)
            .ok_or("Unknown Pad source")?;
        device.snapshot.connected = false;
        device.handle = None;
        Ok(())
    }

    pub fn initialize(&mut self) -> Result<i32> {
        if self.initialized {
            return Err("Unsupported scePadInit repeated initialization semantics".into());
        }
        self.initialized = true;
        Ok(0)
    }

    fn unavailable(&self, operation: &str) -> Result<i32> {
        if !self.initialized {
            return Err(format!("{operation}: Pad is not initialized"));
        }
        Err(format!(
            "Unsupported {operation}: no connected controller backend"
        ))
    }

    pub fn open(&mut self, user: i32, port_type: i32, index: i32, params: u64) -> Result<i32> {
        if port_type != 0 || index != 0 || params != 0 {
            return Err("Unsupported scePadOpen port/index/parameters ABI".into());
        }
        if !self.initialized || self.connected_count() == 0 {
            return self.unavailable("scePadOpen");
        }
        let device = self
            .devices
            .iter_mut()
            .find(|d| d.user == user && d.snapshot.connected)
            .ok_or("scePadOpen: no connected source assigned to this user")?;
        if device.handle.is_some() {
            return Ok(0x8092_0004u32 as i32);
        }
        let handle = self
            .last_handle
            .checked_add(1)
            .ok_or("Pad handle budget exhausted")?;
        self.last_handle = handle;
        device.handle = Some(handle);
        Ok(handle)
    }
    pub fn close(&mut self, handle: i32) -> Result<i32> {
        self.device(handle, "scePadClose")?;
        self.devices
            .iter_mut()
            .find(|d| d.handle == Some(handle))
            .unwrap()
            .handle = None;
        Ok(0)
    }
    fn device(&self, handle: i32, operation: &str) -> Result<&Device> {
        if !self.initialized || self.connected_count() == 0 {
            return Err(self.unavailable(operation).unwrap_err());
        }
        self.devices
            .iter()
            .find(|d| d.snapshot.connected && d.handle == Some(handle))
            .ok_or_else(|| format!("{operation}: invalid or stale Pad handle"))
    }
    pub fn read_state(&self, memory: &mut impl Memory, handle: i32, output: u64) -> Result<i32> {
        self.device(handle, "scePadReadState")?
            .snapshot
            .write(memory, output)?;
        Ok(0)
    }
    pub fn set_light_bar(&self, _handle: i32, _color: u64) -> Result<i32> {
        if self.connected_count() == 0 || !self.initialized {
            return self.unavailable("scePadSetLightBar");
        }
        Err("Unsupported scePadSetLightBar: no output backend".into())
    }
    pub fn set_vibration(&self, _handle: i32, _vibration: u64) -> Result<i32> {
        if self.connected_count() == 0 || !self.initialized {
            return self.unavailable("scePadSetVibration");
        }
        Err("Unsupported scePadSetVibration: no output backend".into())
    }
    pub fn set_vibration_mode(&self, _handle: i32, _mode: i32) -> Result<i32> {
        if self.connected_count() == 0 || !self.initialized {
            return self.unavailable("scePadSetVibrationMode");
        }
        Err("Unsupported scePadSetVibrationMode: no output backend".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> PadData {
        PadData {
            connected: true,
            ..PadData::default()
        }
    }
    #[test]
    fn handles_are_owned_monotonic_and_invalidated_on_close_or_disconnect() {
        let mut pad = Pad::default();
        pad.register_device(10, 42, snapshot()).unwrap();
        assert!(pad.open(42, 0, 0, 0).is_err());
        pad.initialize().unwrap();
        assert!(pad.open(43, 0, 0, 0).is_err());
        let first = pad.open(42, 0, 0, 0).unwrap();
        assert!(first > 0);
        assert_eq!(pad.open(42, 0, 0, 0).unwrap(), 0x8092_0004u32 as i32);
        pad.close(first).unwrap();
        assert!(pad.close(first).is_err());
        let second = pad.open(42, 0, 0, 0).unwrap();
        assert!(second > first);
        pad.disconnect_device(10).unwrap();
        assert_eq!(pad.connected_count(), 0);
        assert!(pad.close(second).is_err());
        assert!(pad.update_device(10, snapshot()).is_err());
        assert!(pad.register_device(10, 43, snapshot()).is_err());
        pad.register_device(10, 42, snapshot()).unwrap();
        let third = pad.open(42, 0, 0, 0).unwrap();
        assert!(third > second);
        let mut memory = crate::guest_memory::Memory::default();
        for stale in [first, second] {
            assert!(pad.read_state(&mut memory, stale, 0x1000).is_err());
        }
        assert!(pad.set_light_bar(third, 0).is_err());
        assert!(pad.set_vibration(third, 0).is_err());
        assert!(pad.set_vibration_mode(third, 0).is_err());
        pad.close(third).unwrap();
        pad.last_handle = i32::MAX;
        assert!(pad.open(42, 0, 0, 0).is_err());
        assert!(pad.devices[0].handle.is_none());
    }
    #[test]
    fn registration_and_updates_require_explicit_bounded_sources() {
        let mut pad = Pad::default();
        assert!(pad.register_device(0, 1, snapshot()).is_err());
        assert!(pad.register_device(1, -1, snapshot()).is_err());
        assert!(pad.register_device(1, 1, PadData::default()).is_err());
        for source in 1..=16 {
            pad.register_device(source, source as i32, snapshot())
                .unwrap();
        }
        assert!(pad.register_device(17, 17, snapshot()).is_err());
        assert!(pad.register_device(2, 1, snapshot()).is_err());
        assert!(pad.update_device(17, snapshot()).is_err());
        assert!(pad.disconnect_device(17).is_err());
        assert!(pad.update_device(1, PadData::default()).is_err());
        assert_eq!(pad.connected_count(), 16);
        pad.disconnect_device(1).unwrap();
        pad.register_device(1, 1, snapshot()).unwrap();
        assert_eq!(pad.connected_count(), 16);
    }
    #[test]
    fn real_guest_mappings_preserve_atomic_read_state_and_live_updates() {
        let mut pad = Pad::default();
        pad.register_device(1, 7, snapshot()).unwrap();
        pad.initialize().unwrap();
        let handle = pad.open(7, 0, 0, 0).unwrap();
        let changed = PadData {
            buttons: crate::pad_data::BUTTON_CROSS,
            timestamp: 123,
            ..snapshot()
        };
        pad.update_device(1, changed.clone()).unwrap();
        for writable in [true, false] {
            let mut bytes = [0xa5; 122];
            {
                let mut memory = crate::guest_memory::Memory::default();
                memory.add(0x1000, &mut bytes, writable, false).unwrap();
                for output in [0, 1, u64::MAX, 0x1003] {
                    assert!(pad.read_state(&mut memory, handle, output).is_err());
                    assert_eq!(memory.read(0x1000, 122).unwrap(), &[0xa5; 122]);
                }
                if writable {
                    assert_eq!(pad.read_state(&mut memory, handle, 0x1001).unwrap(), 0);
                    assert_eq!(memory.read(0x1001, 120).unwrap(), &changed.encode());
                } else {
                    assert!(pad.read_state(&mut memory, handle, 0x1001).is_err());
                    assert_eq!(memory.read(0x1000, 122).unwrap(), &[0xa5; 122]);
                }
            }
            assert_eq!((bytes[0], bytes[121]), (0xa5, 0xa5));
        }
    }
    #[test]
    fn initialization_is_owned_and_never_invents_a_device() {
        let mut pad = Pad::default();
        let other = Pad::default();
        assert!(!pad.initialized());
        assert!(
            pad.open(1, 0, 0, 0)
                .unwrap_err()
                .contains("not initialized")
        );
        assert_eq!(pad.initialize().unwrap(), 0);
        assert!(pad.initialized());
        assert!(!other.initialized());
        assert_eq!(pad.connected_count(), 0);
        assert!(
            pad.initialize()
                .unwrap_err()
                .contains("repeated initialization")
        );
        assert!(pad.initialized());
    }
    #[test]
    fn absent_devices_and_unsupported_arguments_never_succeed() {
        let mut pad = Pad::default();
        for initialized in [false, true] {
            if initialized {
                pad.initialize().unwrap();
            }
            for handle in [i32::MIN, -1, 0, 1, i32::MAX] {
                assert!(pad.open(handle, 0, 0, 0).is_err());
                assert!(pad.close(handle).is_err());
                for address in [0, 1, 0x1000, u64::MAX] {
                    assert!(
                        pad.read_state(
                            &mut crate::guest_memory::Memory::default(),
                            handle,
                            address
                        )
                        .is_err()
                    );
                    assert!(pad.set_light_bar(handle, address).is_err());
                    assert!(pad.set_vibration(handle, address).is_err());
                }
                for mode in [i32::MIN, 0, 1, 2, i32::MAX] {
                    assert!(pad.set_vibration_mode(handle, mode).is_err());
                }
            }
            for (port, index, params) in [(16, 0, 0), (0, 1, 0), (0, 0, 1)] {
                assert!(
                    pad.open(1, port, index, params)
                        .unwrap_err()
                        .contains("parameters ABI")
                );
            }
            assert_eq!(pad.connected_count(), 0);
            assert_eq!(pad.initialized(), initialized);
        }
    }
}
