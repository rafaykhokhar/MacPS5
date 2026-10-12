//! Fixed-size PS5 SDL PadData guest serialization, independent of host ABI.
//! Layout and button definitions:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/joystick/ps5/SDL_ps5joystick.h
//! The pinned crispy-doom.elf PS5_JoystickUpdate at 0x33a40 passes its buffer
//! to ReadState at 0x33ab2, reads buttons at +0 and axes/triggers at +4..+9,
//! and copies 120 bytes at 0x33d29..0x33d52. This confirms the consumed layout.
//! Timestamp units, extension bytes, and count semantics are not established
//! by the header; they are caller-supplied, never inferred here. No no-device
//! return code is established. Serialization does not imply a connected device.
use crate::{Result, strings::Memory};

pub const SIZE: usize = 120;
pub const BUTTON_L3: u32 = 0x0002;
pub const BUTTON_R3: u32 = 0x0004;
pub const BUTTON_OPTIONS: u32 = 0x0008;
pub const BUTTON_UP: u32 = 0x0010;
pub const BUTTON_RIGHT: u32 = 0x0020;
pub const BUTTON_DOWN: u32 = 0x0040;
pub const BUTTON_LEFT: u32 = 0x0080;
pub const BUTTON_L2: u32 = 0x0100;
pub const BUTTON_R2: u32 = 0x0200;
pub const BUTTON_L1: u32 = 0x0400;
pub const BUTTON_R1: u32 = 0x0800;
pub const BUTTON_TRIANGLE: u32 = 0x1000;
pub const BUTTON_CIRCLE: u32 = 0x2000;
pub const BUTTON_CROSS: u32 = 0x4000;
pub const BUTTON_SQUARE: u32 = 0x8000;
pub const BUTTON_TOUCH_PAD: u32 = 0x0010_0000;

#[derive(Clone, Copy, Debug, Default)]
pub struct Touch {
    pub x: u16,
    pub y: u16,
    pub finger: u8,
}

/// Semantic fields are encoded explicitly; host struct padding is never read.
#[derive(Clone, Debug)]
pub struct PadData {
    pub buttons: u32,
    pub left_stick: [u8; 2],
    pub right_stick: [u8; 2],
    pub triggers: [u8; 2],
    pub quaternion: [f32; 4],
    pub angular_velocity: [f32; 3],
    pub acceleration: [f32; 3],
    pub fingers: u8,
    pub touches: [Touch; 2],
    pub connected: bool,
    pub timestamp: u64,
    pub extension: [u8; 16],
    pub count: u8,
    pub unknown: [u8; 15],
}
impl Default for PadData {
    fn default() -> Self {
        Self {
            buttons: 0,
            left_stick: [128; 2],
            right_stick: [128; 2],
            triggers: [0; 2],
            quaternion: [0.0; 4],
            angular_velocity: [0.0; 3],
            acceleration: [0.0; 3],
            fingers: 0,
            touches: [Touch::default(); 2],
            connected: false,
            timestamp: 0,
            extension: [0; 16],
            count: 0,
            unknown: [0; 15],
        }
    }
}
impl PadData {
    pub fn encode(&self) -> [u8; SIZE] {
        let mut out = [0; SIZE];
        out[0..4].copy_from_slice(&self.buttons.to_le_bytes());
        out[4..6].copy_from_slice(&self.left_stick);
        out[6..8].copy_from_slice(&self.right_stick);
        out[8..10].copy_from_slice(&self.triggers);
        for (slot, value) in out[12..52].chunks_exact_mut(4).zip(
            self.quaternion
                .iter()
                .chain(&self.angular_velocity)
                .chain(&self.acceleration),
        ) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        out[52] = self.fingers;
        for (slot, touch) in out[60..76].chunks_exact_mut(8).zip(&self.touches) {
            slot[0..2].copy_from_slice(&touch.x.to_le_bytes());
            slot[2..4].copy_from_slice(&touch.y.to_le_bytes());
            slot[4] = touch.finger;
        }
        out[76] = u8::from(self.connected);
        out[80..88].copy_from_slice(&self.timestamp.to_le_bytes());
        out[88..104].copy_from_slice(&self.extension);
        out[104] = self.count;
        out[105..120].copy_from_slice(&self.unknown);
        out
    }
    /// One guarded atomic write of exactly 120 bytes; no guest address is cast
    /// to a host pointer and no allocation depends on guest data.
    pub fn write(&self, memory: &mut impl Memory, address: u64) -> Result<()> {
        if address == 0 || address.checked_add(SIZE as u64 - 1).is_none() {
            return Err("Invalid PadData output address".into());
        }
        memory
            .write(address, &self.encode())
            .map_err(|error| format!("PadData output memory fault: {error:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::Fault;
    #[test]
    fn all_fields_use_verified_little_endian_offsets_and_zero_padding() {
        let state = PadData {
            buttons: BUTTON_CROSS | BUTTON_UP | BUTTON_TOUCH_PAD,
            left_stick: [0, 255],
            right_stick: [127, 128],
            triggers: [1, 254],
            quaternion: [1.0, -2.0, 3.0, -0.0],
            angular_velocity: [4.0, 5.0, 6.0],
            acceleration: [7.0, 8.0, f32::from_bits(0x7fc12345)],
            fingers: 2,
            touches: [
                Touch {
                    x: 0x1234,
                    y: 0xabcd,
                    finger: 7,
                },
                Touch {
                    x: 0xffff,
                    y: 1,
                    finger: 8,
                },
            ],
            connected: true,
            timestamp: 0x0123456789abcdef,
            extension: [0x56; 16],
            count: 9,
            unknown: [0x78; 15],
        };
        let bytes = state.encode();
        assert_eq!(
            &bytes[..12],
            &[0x10, 0x40, 0x10, 0, 0, 255, 127, 128, 1, 254, 0, 0]
        );
        for (offset, bits) in [
            (12, 1f32.to_bits()),
            (24, (-0f32).to_bits()),
            (28, 4f32.to_bits()),
            (40, 7f32.to_bits()),
            (48, 0x7fc12345),
        ] {
            assert_eq!(&bytes[offset..offset + 4], &bits.to_le_bytes());
        }
        assert_eq!(
            &bytes[52..80],
            &[
                2, 0, 0, 0, 0, 0, 0, 0, 0x34, 0x12, 0xcd, 0xab, 7, 0, 0, 0, 0xff, 0xff, 1, 0, 8, 0,
                0, 0, 1, 0, 0, 0
            ]
        );
        assert_eq!(
            &bytes[80..88],
            &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(&bytes[88..104], &[0x56; 16]);
        assert_eq!(bytes[104], 9);
        assert_eq!(&bytes[105..], &[0x78; 15]);
        let defaults = PadData::default().encode();
        assert_eq!(&defaults[4..8], &[128; 4]);
        assert_eq!(defaults[76], 0);
        assert!(defaults[8..].iter().all(|&byte| byte == 0));
    }
    #[test]
    fn guest_copy_is_exact_and_failure_atomic() {
        struct Buffer {
            bytes: [u8; 122],
            writable: bool,
        }
        impl Memory for Buffer {
            fn read(&self, _: u64) -> std::result::Result<u8, Fault> {
                panic!("No reads expected")
            }
            fn write(&mut self, address: u64, bytes: &[u8]) -> std::result::Result<(), Fault> {
                let start = usize::try_from(address.checked_sub(0x1000).ok_or(Fault::Write)?)
                    .map_err(|_| Fault::Write)?;
                let end = start.checked_add(bytes.len()).ok_or(Fault::Write)?;
                if !self.writable {
                    return Err(Fault::Write);
                }
                self.bytes
                    .get_mut(start..end)
                    .ok_or(Fault::Write)?
                    .copy_from_slice(bytes);
                Ok(())
            }
        }
        let state = PadData {
            connected: true,
            ..PadData::default()
        };
        let mut memory = Buffer {
            bytes: [0xa5; 122],
            writable: true,
        };
        state.write(&mut memory, 0x1001).unwrap();
        assert_eq!(&memory.bytes[1..121], &state.encode());
        assert_eq!((memory.bytes[0], memory.bytes[121]), (0xa5, 0xa5));
        for writable in [true, false] {
            for address in [0, 1, u64::MAX, 0x1003] {
                memory = Buffer {
                    bytes: [0xa5; 122],
                    writable,
                };
                assert!(state.write(&mut memory, address).is_err());
                assert_eq!(memory.bytes, [0xa5; 122]);
            }
        }
        memory = Buffer {
            bytes: [0xa5; 122],
            writable: false,
        };
        assert!(state.write(&mut memory, 0x1001).is_err());
        assert_eq!(memory.bytes, [0xa5; 122]);
    }
}
