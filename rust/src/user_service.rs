//! Explicit process-local users; never reads host accounts or creates defaults.
//!
//! PS5 SDL declares Initialize(void*), GetLoginUserIdList(int[4]), and
//! GetUserName(int,char*,size_t) in:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/joystick/ps5/SDL_ps5joystick.h
//! Its joystick implementation uses -1 for empty slots and accepts 0x80960003
//! after repeated initialization. The keyboard implementation declares
//! GetForegroundUser(int*) and also initializes with NULL:
//! https://github.com/ps5-payload-dev/SDL/blob/release-2.30.x-ps5/src/video/ps5/SDL_ps5keyboard.c
//! Other error codes and non-null initialization parameters are not established
//! by these sources; those cases return runtime diagnostics, not guessed codes.
use crate::{Result, strings::Memory};

pub const ALREADY_INITIALIZED: i32 = 0x8096_0003u32 as i32;
/// Model resource bound, not a claim about the console's username limit.
pub const MAX_NAME_BYTES: usize = 255;

#[derive(Debug)]
struct Profile {
    id: i32,
    name: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct UserService {
    users: Vec<Profile>,
    foreground: Option<i32>,
    initialized: bool,
}

impl UserService {
    /// Configure virtual local users before guest initialization. Names are
    /// bounded byte strings, independent of host accounts or text encoding.
    pub fn register_user(&mut self, id: i32, name: &[u8]) -> Result<()> {
        if self.initialized
            || self.users.len() == 4
            || id == -1
            || self.users.iter().any(|user| user.id == id)
            || name.is_empty()
            || name.len() > MAX_NAME_BYTES
            || name.contains(&0)
        {
            return Err(
                "Invalid, duplicate, over-budget or frozen virtual user registration".into(),
            );
        }
        let mut name = name.to_vec();
        name.push(0);
        self.users.push(Profile { id, name });
        Ok(())
    }

    pub fn set_foreground_user(&mut self, id: i32) -> Result<()> {
        if self.initialized || !self.users.iter().any(|user| user.id == id) {
            return Err(
                "Foreground user must be explicitly registered before initialization".into(),
            );
        }
        self.foreground = Some(id);
        Ok(())
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn initialize(&mut self, params: u64) -> Result<i32> {
        if params != 0 {
            return Err("Unsupported sceUserServiceInitialize non-null parameters ABI".into());
        }
        if self.initialized {
            return Ok(ALREADY_INITIALIZED);
        }
        self.initialized = true;
        Ok(0)
    }

    fn require_initialized(&self) -> Result<()> {
        if self.initialized {
            Ok(())
        } else {
            Err("UserService is not initialized".into())
        }
    }

    pub fn get_foreground_user(&self, memory: &mut impl Memory, output: u64) -> Result<i32> {
        self.require_initialized()?;
        let id = self
            .foreground
            .ok_or("No virtual foreground user is configured")?;
        write(memory, output, &id.to_le_bytes())
    }

    pub fn get_login_user_id_list(&self, memory: &mut impl Memory, output: u64) -> Result<i32> {
        self.require_initialized()?;
        let mut bytes = [0xff; 16];
        for (slot, user) in bytes.chunks_exact_mut(4).zip(&self.users) {
            slot.copy_from_slice(&user.id.to_le_bytes());
        }
        write(memory, output, &bytes)
    }

    pub fn get_user_name(
        &self,
        memory: &mut impl Memory,
        id: i32,
        output: u64,
        capacity: u64,
    ) -> Result<i32> {
        self.require_initialized()?;
        let user = self
            .users
            .iter()
            .find(|user| user.id == id)
            .ok_or("Unknown virtual user ID")?;
        if capacity < user.name.len() as u64 {
            return Err("UserService name output capacity is too short".into());
        }
        // Capacity is never used for allocation or padding. Only the bounded
        // name plus its terminator is written, leaving the remaining bytes alone.
        write(memory, output, &user.name)
    }
}

fn write(memory: &mut impl Memory, output: u64, bytes: &[u8]) -> Result<i32> {
    if output == 0 || output.checked_add(bytes.len() as u64 - 1).is_none() {
        return Err("Invalid UserService output address".into());
    }
    memory
        .write(output, bytes)
        .map_err(|fault| format!("UserService output memory fault: {fault:?}"))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strings::Fault;
    struct Buffer {
        bytes: [u8; 32],
        writable: bool,
    }
    impl Default for Buffer {
        fn default() -> Self {
            Self {
                bytes: [0xa5; 32],
                writable: true,
            }
        }
    }
    impl Memory for Buffer {
        fn read(&self, _: u64) -> std::result::Result<u8, Fault> {
            panic!("UserService must not read guest memory")
        }
        fn write(&mut self, address: u64, bytes: &[u8]) -> std::result::Result<(), Fault> {
            let offset = usize::try_from(address.checked_sub(0x1000).ok_or(Fault::Write)?)
                .map_err(|_| Fault::Write)?;
            let end = offset.checked_add(bytes.len()).ok_or(Fault::Write)?;
            if !self.writable {
                return Err(Fault::Write);
            }
            self.bytes
                .get_mut(offset..end)
                .ok_or(Fault::Write)?
                .copy_from_slice(bytes);
            Ok(())
        }
    }
    #[test]
    fn explicit_configuration_is_bounded_and_frozen_after_initialization() {
        let mut service = UserService::default();
        for (id, name) in [(-1, b"name".as_slice()), (1, b""), (1, b"a\0b")] {
            assert!(service.register_user(id, name).is_err());
        }
        assert!(service.register_user(1, &[b'x'; 256]).is_err());
        service.register_user(1, &[b'x'; 255]).unwrap();
        assert!(service.register_user(1, b"duplicate").is_err());
        for id in [2, 3, 4] {
            service.register_user(id, b"user").unwrap();
        }
        assert!(service.register_user(5, b"fifth").is_err());
        assert!(service.set_foreground_user(5).is_err());
        service.set_foreground_user(2).unwrap();
        assert!(service.initialize(u64::MAX).is_err());
        assert!(!service.initialized());
        assert_eq!(service.initialize(0).unwrap(), 0);
        assert_eq!(service.initialize(0).unwrap(), ALREADY_INITIALIZED);
        assert!(service.register_user(6, b"late").is_err());
        assert!(service.set_foreground_user(1).is_err());
        assert!(!UserService::default().initialized());
    }
    #[test]
    fn empty_and_configured_profiles_have_exact_guest_layouts() {
        let mut service = UserService::default();
        let mut memory = Buffer::default();
        assert!(service.get_login_user_id_list(&mut memory, 0x1000).is_err());
        assert_eq!(memory.bytes, [0xa5; 32]);
        service.initialize(0).unwrap();
        service.get_login_user_id_list(&mut memory, 0x1000).unwrap();
        assert_eq!(&memory.bytes[..16], &[0xff; 16]);
        assert!(service.get_foreground_user(&mut memory, 0x1000).is_err());
        let mut service = UserService::default();
        service.register_user(0x12345678, b"Local").unwrap();
        service.set_foreground_user(0x12345678).unwrap();
        service.initialize(0).unwrap();
        service.get_login_user_id_list(&mut memory, 0x1000).unwrap();
        assert_eq!(&memory.bytes[..4], &0x12345678i32.to_le_bytes());
        assert_eq!(&memory.bytes[4..16], &[0xff; 12]);
        service.get_foreground_user(&mut memory, 0x1010).unwrap();
        service
            .get_user_name(&mut memory, 0x12345678, 0x1014, u64::MAX)
            .unwrap();
        assert_eq!(&memory.bytes[16..20], &0x12345678i32.to_le_bytes());
        assert_eq!(&memory.bytes[20..26], b"Local\0");
        assert_eq!(&memory.bytes[26..], &[0xa5; 6]);
    }
    #[test]
    fn invalid_outputs_and_unknown_users_leave_memory_unchanged() {
        let mut service = UserService::default();
        service.register_user(7, b"Local").unwrap();
        service.set_foreground_user(7).unwrap();
        service.initialize(0).unwrap();
        for writable in [true, false] {
            let mut memory = Buffer {
                writable,
                ..Buffer::default()
            };
            assert!(service.get_user_name(&mut memory, 8, 0x1000, 32).is_err());
            assert!(service.get_user_name(&mut memory, 7, 0x1000, 5).is_err());
            for address in [0, 1, u64::MAX, 0x101f] {
                assert!(service.get_foreground_user(&mut memory, address).is_err());
                assert!(
                    service
                        .get_login_user_id_list(&mut memory, address)
                        .is_err()
                );
                assert!(service.get_user_name(&mut memory, 7, address, 32).is_err());
            }
            if !writable {
                assert!(service.get_foreground_user(&mut memory, 0x1000).is_err());
                assert!(service.get_login_user_id_list(&mut memory, 0x1000).is_err());
                assert!(service.get_user_name(&mut memory, 7, 0x1000, 32).is_err());
            }
            assert_eq!(memory.bytes, [0xa5; 32]);
        }
    }
}
