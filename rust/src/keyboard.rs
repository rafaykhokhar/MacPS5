//! Owned keyboard snapshots for the pinned SDL consumer; no host discovery.
//!
//! Primary declaration and caller:
//! https://github.com/ps5-payload-dev/SDL/blob/8c56053f13ca13a0c050de613706ff69eb615836/src/video/ps5/SDL_ps5keyboard.c
//! Its opaque fourth Open argument is an uninitialized `long` address. The ELF
//! passes that at 0xc402e and consumes the returned EAX handle at 0xc4038; the
//! pointed-to value is never consumed. This does NOT establish output semantics.
//! Non-null opaque arguments remain unsupported. NULL is our restricted subset.
//!
//! ELF ReadState at 0xc417a reads availability +16, modifiers +28, and sixteen
//! u16 slots +32, then copies 96 bytes at 0xc472b..0xc4753. Unknown fields remain
//! zero, without assigning them invented semantics. SDL compares slots rather
//! than key sets: a nonzero-to-nonzero replacement loses the new key press.
use crate::{Result, strings::Memory};

pub const STATE_SIZE: usize = 96;
pub const MAX_DEVICES: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyboardState {
    pub available: bool,
    pub modifiers: u32,
    pub scancodes: [u16; 16],
}
impl KeyboardState {
    fn validate(&self) -> Result<()> {
        if self.modifiers & !0xff != 0 {
            return Err("Keyboard modifier bits outside observed SDL subset".into());
        }
        if !self.available && (self.modifiers != 0 || self.scancodes.iter().any(|k| *k != 0)) {
            return Err("Unavailable keyboard snapshot contains pressed keys".into());
        }
        for (i, key) in self.scancodes.iter().copied().enumerate() {
            // SDL_NUM_SCANCODES is 512. Modifier keys use the separate bit mask.
            if key >= 512 || (224..=231).contains(&key) {
                return Err("Unsupported keyboard scancode; modifiers use their mask".into());
            }
            if key != 0 && self.scancodes[..i].contains(&key) {
                return Err("Duplicate keyboard scancode".into());
            }
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<[u8; STATE_SIZE]> {
        self.validate()?;
        let mut bytes = [0; STATE_SIZE];
        bytes[16] = u8::from(self.available);
        bytes[28..32].copy_from_slice(&self.modifiers.to_le_bytes());
        for (i, key) in self.scancodes.iter().enumerate() {
            bytes[32 + i * 2..34 + i * 2].copy_from_slice(&key.to_le_bytes());
        }
        Ok(bytes)
    }
}

#[derive(Debug)]
struct Device {
    source: u64,
    user: i32,
    connected: bool,
    state: KeyboardState,
    delivered: KeyboardState,
    handle: Option<i32>,
    desired: Option<KeyboardState>,
}
impl Device {
    fn refresh_pressed(&mut self) {
        let Some(desired) = &self.desired else {
            return;
        };
        let mut next = KeyboardState {
            available: true,
            modifiers: desired.modifiers,
            scancodes: [0; 16],
        };
        // Retain slots already occupied by still-held keys, including a key
        // released/repressed since the last successful guest read.
        for prior in [&self.state.scancodes, &self.delivered.scancodes] {
            for (slot, key) in prior.iter().copied().enumerate() {
                if key != 0 && desired.scancodes.contains(&key) && !next.scancodes.contains(&key) {
                    next.scancodes[slot] = key;
                }
            }
        }
        for key in desired.scancodes.iter().copied().filter(|key| *key != 0) {
            if next.scancodes.contains(&key) {
                continue;
            }
            if let Some(slot) = (0..16).find(|slot| {
                next.scancodes[*slot] == 0
                    && (self.handle.is_none() || self.delivered.scancodes[*slot] == 0)
            }) {
                next.scancodes[slot] = key;
            }
        }
        self.state = next;
    }
}
#[derive(Debug, Default)]
pub struct Keyboard {
    initialized: bool,
    devices: Vec<Device>,
    last_handle: i32,
}
impl Keyboard {
    pub fn initialized(&self) -> bool {
        self.initialized
    }
    pub fn connected_count(&self) -> usize {
        self.devices.iter().filter(|d| d.connected).count()
    }
    /// Bounded to sixteen lifetime source identities. Reconnection is explicit
    /// and retains the original user assignment; there is no implicit profile.
    pub fn register_device(&mut self, source: u64, user: i32, state: KeyboardState) -> Result<()> {
        state.validate()?;
        if source == 0 || user == -1 || !state.available {
            return Err("Invalid explicit keyboard source/user/availability".into());
        }
        if self.devices.iter().any(|d| d.connected && d.user == user) {
            return Err("Keyboard user already has a connected source".into());
        }
        if let Some(d) = self.devices.iter_mut().find(|d| d.source == source) {
            if d.connected || d.user != user {
                return Err(
                    "Keyboard source already registered or assigned to another user".into(),
                );
            }
            d.connected = true;
            d.state = state;
            d.delivered = KeyboardState::default();
            d.handle = None;
            d.desired = None;
        } else {
            if self.devices.len() == MAX_DEVICES {
                return Err("Keyboard source budget exhausted".into());
            }
            self.devices.push(Device {
                source,
                user,
                connected: true,
                state,
                delivered: KeyboardState::default(),
                handle: None,
                desired: None,
            });
        }
        Ok(())
    }
    pub fn update_device(&mut self, source: u64, state: KeyboardState) -> Result<()> {
        state.validate()?;
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.source == source && d.connected)
            .ok_or("Unknown or disconnected keyboard source")?;
        if !state.available {
            return Err(
                "Connected keyboard updates must expose availability; disconnect explicitly".into(),
            );
        }
        for (i, key) in state.scancodes.iter().copied().enumerate() {
            if key == 0 {
                continue;
            }
            if d.state
                .scancodes
                .iter()
                .position(|k| *k == key)
                .is_some_and(|old| old != i)
            {
                return Err("Keyboard held key moved to another slot".into());
            }
            if d.handle.is_some()
                && d.delivered
                    .scancodes
                    .iter()
                    .position(|k| *k == key)
                    .is_some_and(|old| old != i)
            {
                return Err(
                    "Keyboard delivered key needs a zero release before moving slots".into(),
                );
            }
            if d.handle.is_some()
                && d.delivered.scancodes[i] != 0
                && d.delivered.scancodes[i] != key
            {
                return Err("Keyboard slot needs a delivered zero release before reuse".into());
            }
        }
        d.state = state;
        d.desired = None;
        Ok(())
    }
    /// Accept an unordered pressed set from an explicitly assigned host source.
    /// Ok means the desired snapshot was accepted, not that every press was
    /// delivered. New keys wait when all slots need acknowledged releases.
    /// A successful read promotes deferred keys for the *next* guest read.
    /// Later snapshots replace earlier desired state; this is not an event log.
    pub fn update_pressed(&mut self, source: u64, modifiers: u32, pressed: &[u16]) -> Result<()> {
        if pressed.len() > 16 || pressed.contains(&0) {
            return Err("Keyboard pressed set exceeds sixteen keys or includes zero".into());
        }
        let mut desired = KeyboardState {
            available: true,
            modifiers,
            scancodes: [0; 16],
        };
        desired.scancodes[..pressed.len()].copy_from_slice(pressed);
        desired.validate()?;
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.source == source && d.connected)
            .ok_or("Unknown or disconnected keyboard source")?;
        d.desired = Some(desired);
        d.refresh_pressed();
        Ok(())
    }
    /// Disconnect invalidates the handle. For focus loss, submit an available
    /// empty snapshot and let the guest read it first so SDL can release keys.
    pub fn disconnect_device(&mut self, source: u64) -> Result<()> {
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.source == source && d.connected)
            .ok_or("Unknown or disconnected keyboard source")?;
        d.connected = false;
        d.state = KeyboardState::default();
        d.delivered = KeyboardState::default();
        d.handle = None;
        d.desired = None;
        Ok(())
    }
    pub fn initialize(&mut self) -> Result<i32> {
        if self.initialized {
            return Err("Unsupported sceKeyboardInit repeated initialization semantics".into());
        }
        self.initialized = true;
        Ok(0)
    }
    pub fn open(&mut self, user: i32, port: i32, index: i32, opaque: u64) -> Result<i32> {
        if opaque != 0 {
            return Err("Unsupported sceKeyboardOpen non-null opaque argument ABI".into());
        }
        if port != 0 || index != 0 {
            return Err("Unsupported sceKeyboardOpen port/index".into());
        }
        if !self.initialized {
            return Err("sceKeyboardOpen: keyboard is not initialized".into());
        }
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.connected && d.user == user)
            .ok_or("sceKeyboardOpen: no explicit connected source assigned to user")?;
        if d.handle.is_some() {
            return Err("sceKeyboardOpen: source already open; console errno unverified".into());
        }
        let handle = self
            .last_handle
            .checked_add(1)
            .ok_or("Keyboard handle budget exhausted")?;
        self.last_handle = handle;
        d.handle = Some(handle);
        d.delivered = KeyboardState::default();
        d.refresh_pressed();
        Ok(handle)
    }
    fn device_mut(&mut self, handle: i32) -> Result<&mut Device> {
        if !self.initialized {
            return Err("Keyboard is not initialized".into());
        }
        self.devices
            .iter_mut()
            .find(|d| d.connected && d.handle == Some(handle))
            .ok_or_else(|| "Invalid or stale keyboard handle".into())
    }
    pub fn close(&mut self, handle: i32) -> Result<i32> {
        let d = self.device_mut(handle)?;
        d.handle = None;
        d.delivered = KeyboardState::default();
        d.refresh_pressed();
        Ok(0)
    }
    pub fn read_state(
        &mut self,
        memory: &mut impl Memory,
        handle: i32,
        output: u64,
    ) -> Result<i32> {
        let d = self.device_mut(handle)?;
        let bytes = d.state.encode()?;
        memory
            .write(output, &bytes)
            .map_err(|fault| format!("Keyboard state output fault: {fault:?}"))?;
        d.delivered = d.state.clone();
        d.refresh_pressed();
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(keys: &[u16]) -> KeyboardState {
        let mut s = KeyboardState {
            available: true,
            ..KeyboardState::default()
        };
        s.scancodes[..keys.len()].copy_from_slice(keys);
        s
    }
    #[test]
    fn pressed_sets_preserve_slots_and_defer_until_real_delivery() {
        let mut k = Keyboard::default();
        k.initialize().unwrap();
        let original: Vec<u16> = (4..20).collect();
        let replacement: Vec<u16> = (40..56).collect();
        k.register_device(1, 7, state(&original)).unwrap();
        let handle = k.open(7, 0, 0, 0).unwrap();
        let mut bytes = [0; 96];
        let mut readonly = [0xa5; 96];
        let mut memory = crate::guest_memory::Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        k.read_state(&mut memory, handle, 0x1000).unwrap();
        let mut reversed = original.clone();
        reversed.reverse();
        k.update_pressed(1, 0, &reversed).unwrap();
        assert_eq!(k.devices[0].state, state(&original));
        // No spare slot: replacement waits behind sixteen real releases.
        k.update_pressed(1, 2, &replacement).unwrap();
        assert_eq!(k.devices[0].state.scancodes, [0; 16]);
        assert!(k.read_state(&mut memory, handle, 0x2000).is_err());
        assert_eq!(k.devices[0].delivered.scancodes, state(&original).scancodes);
        assert_eq!(k.devices[0].state.scancodes, [0; 16]);
        assert_eq!(memory.read(0x2000, 96).unwrap(), [0xa5; 96]);
        k.read_state(&mut memory, handle, 0x1000).unwrap();
        assert_eq!(memory.read(0x1020, 32).unwrap(), [0; 32]);
        assert_eq!(k.devices[0].state.scancodes, state(&replacement).scancodes);
        k.read_state(&mut memory, handle, 0x1000).unwrap();
        assert_eq!(
            memory.read(0x1020, 32).unwrap(),
            &state(&replacement).encode().unwrap()[32..64]
        );
        let before = k.devices[0].state.clone();
        let pending = k.devices[0].desired.clone();
        for (mods, keys) in [
            (256, vec![4]),
            (0, vec![0]),
            (0, vec![4, 4]),
            (0, vec![224]),
            (0, vec![512]),
            (0, (4..21).collect()),
        ] {
            assert!(k.update_pressed(1, mods, &keys).is_err());
            assert_eq!(k.devices[0].state, before);
            assert_eq!(k.devices[0].desired, pending);
        }
        // New input supersedes deferred input; no invented replay of a key
        // that was already released before the guest could observe it.
        k.update_pressed(1, 0, &original).unwrap();
        k.update_pressed(1, 0, &[]).unwrap();
        k.read_state(&mut memory, handle, 0x1000).unwrap();
        assert_eq!(k.devices[0].state.scancodes, [0; 16]);
        assert!(k.update_pressed(99, 0, &[]).is_err());
    }
    #[test]
    fn exact_layout_and_rejected_snapshots() {
        let mut s = state(&[4, 30, 511]);
        s.modifiers = 0x81;
        let bytes = s.encode().unwrap();
        let mut expected = [0; 96];
        expected[16] = 1;
        expected[28] = 0x81;
        expected[32..38].copy_from_slice(&[4, 0, 30, 0, 255, 1]);
        assert_eq!(bytes, expected);
        for bad in [state(&[4, 4]), state(&[512]), state(&[224])] {
            assert!(bad.encode().is_err());
        }
        s.modifiers = 256;
        assert!(s.encode().is_err());
        s.modifiers = 0;
        s.available = false;
        assert!(s.encode().is_err());
    }
    #[test]
    fn owned_lifecycle_budget_and_opaque_abi_boundary() {
        let mut k = Keyboard::default();
        assert!(k.open(1, 0, 0, 0).is_err());
        k.initialize().unwrap();
        assert!(k.initialize().is_err());
        assert_eq!(k.connected_count(), 0);
        assert!(k.open(1, 0, 0, 0).is_err());
        assert!(k.register_device(0, 1, state(&[])).is_err());
        assert!(k.register_device(1, -1, state(&[])).is_err());
        for i in 1..=16 {
            k.register_device(i, i as i32, state(&[])).unwrap();
        }
        assert!(k.register_device(17, 17, state(&[])).is_err());
        assert!(k.open(1, 0, 0, 0x1000).unwrap_err().contains("opaque"));
        assert!(k.open(1, 1, 0, 0).is_err());
        let a = k.open(1, 0, 0, 0).unwrap();
        assert!(a > 0);
        assert!(k.open(1, 0, 0, 0).is_err());
        k.close(a).unwrap();
        assert!(k.close(a).is_err());
        let b = k.open(1, 0, 0, 0).unwrap();
        assert!(b > a);
        k.disconnect_device(1).unwrap();
        assert!(k.close(b).is_err());
        assert!(k.register_device(1, 2, state(&[])).is_err());
        k.register_device(1, 1, state(&[])).unwrap();
        let c = k.open(1, 0, 0, 0).unwrap();
        assert!(c > b);
        k.close(c).unwrap();
        k.last_handle = i32::MAX;
        assert!(k.open(1, 0, 0, 0).is_err());
        assert!(k.devices[0].handle.is_none());
    }
    #[test]
    fn failed_read_cannot_acknowledge_release_or_change_guest_bytes() {
        let mut k = Keyboard::default();
        k.initialize().unwrap();
        k.register_device(1, 7, state(&[4, 5])).unwrap();
        let handle = k.open(7, 0, 0, 0).unwrap();
        let mut bytes = [0xa5; 98];
        let mut readonly = [0xa5; 96];
        let mut memory = crate::guest_memory::Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        k.read_state(&mut memory, handle, 0x1001).unwrap();
        assert!(k.update_device(1, state(&[5, 4])).is_err());
        assert!(k.update_device(1, state(&[6, 5])).is_err());
        k.update_device(1, state(&[0, 5])).unwrap();
        assert!(k.update_device(1, state(&[0, 5, 4])).is_err());
        let before = memory.read(0x1000, 98).unwrap().to_vec();
        for pointer in [0, 0x1003, 0x2000, u64::MAX] {
            assert!(k.read_state(&mut memory, handle, pointer).is_err());
            assert_eq!(memory.read(0x1000, 98).unwrap(), before);
            assert_eq!(memory.read(0x2000, 96).unwrap(), [0xa5; 96]);
            assert!(k.update_device(1, state(&[6, 5])).is_err());
        }
        k.read_state(&mut memory, handle, 0x1001).unwrap();
        k.update_device(1, state(&[6, 5])).unwrap();
        k.read_state(&mut memory, handle, 0x1001).unwrap();
        assert_eq!(
            memory.read(0x1001, 96).unwrap(),
            state(&[6, 5]).encode().unwrap()
        );
        assert_eq!(memory.load(0x1000, 1).unwrap(), 0xa5);
        assert_eq!(memory.load(0x1061, 1).unwrap(), 0xa5);
        assert!(k.update_device(1, KeyboardState::default()).is_err());
        k.disconnect_device(1).unwrap();
        assert!(k.read_state(&mut memory, handle, 0x1001).is_err());
    }
}
