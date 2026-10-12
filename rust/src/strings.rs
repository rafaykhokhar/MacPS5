//! Bounded guest string operations. Guest addresses never become host pointers.
use sha1::{Digest, Sha1};
pub const LIMIT: usize = 65_536;
pub const NAMES: [&str; 10] = [
    "strcpy", "strcat", "strcmp", "strncmp", "strlen", "sprintf", "calloc", "free", "getenv",
    "malloc",
];
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Fault {
    Read = 1,
    Write = 2,
    Limit = 8,
    Allocation = 12,
}
pub trait Memory {
    fn read(&self, address: u64) -> Result<u8, Fault>;
    fn read_into(&self, address: u64, out: &mut [u8]) -> Result<(), Fault> {
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = self.read(address.checked_add(i as u64).ok_or(Fault::Read)?)?;
        }
        Ok(())
    }
    /// Must validate the entire destination before modifying any bytes.
    fn write(&mut self, address: u64, bytes: &[u8]) -> Result<(), Fault>;
}
fn byte(memory: &impl Memory, base: u64, offset: usize) -> Result<u8, Fault> {
    memory.read(base.checked_add(offset as u64).ok_or(Fault::Read)?)
}
fn string(memory: &impl Memory, address: u64) -> Result<Vec<u8>, Fault> {
    let mut out = Vec::new();
    for index in 0..LIMIT {
        let b = byte(memory, address, index)?;
        if out.len() == out.capacity() {
            out.try_reserve(64).map_err(|_| Fault::Limit)?;
        }
        out.push(b);
        if b == 0 {
            return Ok(out);
        }
    }
    Err(Fault::Limit)
}
fn length(memory: &impl Memory, address: u64) -> Result<usize, Fault> {
    for index in 0..LIMIT {
        if byte(memory, address, index)? == 0 {
            return Ok(index);
        }
    }
    Err(Fault::Limit)
}
pub fn call(
    memory: &mut impl Memory,
    operation: u64,
    first: u64,
    second: u64,
    count: u64,
) -> Result<u64, Fault> {
    match operation {
        0 | 1 => {
            let offset = if operation == 1 {
                length(memory, first)?
            } else {
                0
            };
            let data = string(memory, second)?;
            let destination = first.checked_add(offset as u64).ok_or(Fault::Write)?;
            destination
                .checked_add(data.len() as u64 - 1)
                .ok_or(Fault::Write)?;
            memory.write(destination, &data)?;
            Ok(first)
        }
        2 | 3 => {
            let count = if operation == 2 { u64::MAX } else { count };
            for index in 0..LIMIT {
                if index as u64 >= count {
                    return Ok(0);
                }
                let a = byte(memory, first, index)?;
                let b = byte(memory, second, index)?;
                if a != b {
                    return Ok((i32::from(a) - i32::from(b)) as u32 as u64);
                }
                if a == 0 {
                    return Ok(0);
                }
            }
            if count == LIMIT as u64 {
                Ok(0)
            } else {
                Err(Fault::Limit)
            }
        }
        4 => Ok(length(memory, first)? as u64),
        _ => Err(Fault::Limit),
    }
}
#[repr(C)]
pub struct Export {
    pub name: [u8; 16],
    pub nid: [u8; 12],
}
pub fn export(index: usize) -> Export {
    export_name(NAMES.get(index).copied().unwrap_or(""))
}
fn export_name(name: &str) -> Export {
    let mut out = Export {
        name: [0; 16],
        nid: [0; 12],
    };
    if name.is_empty() || name.len() >= out.name.len() {
        return out;
    }
    out.name[..name.len()].copy_from_slice(name.as_bytes());
    out.nid = nid(name);
    out
}
pub fn nid(name: &str) -> [u8; 12] {
    let mut out = [0u8; 12];
    let mut digest = Sha1::new();
    digest.update(name.as_bytes());
    digest.update([
        0x51, 0x8d, 0x64, 0xa6, 0x35, 0xde, 0xd8, 0xc1, 0xe6, 0xb0, 0x39, 0xb1, 0xc3, 0xe5, 0x52,
        0x30,
    ]);
    let digest = digest.finalize();
    let bytes: [u8; 8] = std::array::from_fn(|i| digest[7 - i]);
    let value = u64::from_be_bytes(bytes);
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
    for (i, byte) in out.iter_mut().take(11).enumerate() {
        let bits = if i == 10 {
            (value & 15) << 2
        } else {
            (value >> (58 - i * 6)) & 63
        };
        *byte = alphabet[bits as usize];
    }
    out
}
#[allow(unsafe_code)]
pub mod ffi {
    use super::*;
    use std::ffi::c_void;
    #[repr(C)]
    pub struct MemoryApi {
        pub context: *mut c_void,
        pub read: Option<unsafe extern "C" fn(*mut c_void, u64, *mut u8, u64) -> u64>,
        pub write: Option<unsafe extern "C" fn(*mut c_void, u64, *const u8, u64) -> u64>,
    }
    #[repr(C)]
    pub struct Outcome {
        pub value: u64,
        pub fault: u64,
    }
    struct Adapter(MemoryApi);
    /// # Safety
    /// Same callback lifetime, exclusive context and whole-access contract as libc_call_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_getenv_v1(
        api: MemoryApi,
        environment: u64,
        name: u64,
    ) -> Outcome {
        if api.context.is_null() || api.read.is_none() {
            return Outcome { value: 0, fault: 8 };
        }
        match crate::environment::getenv(&Adapter(api), environment, name) {
            Ok(value) => Outcome { value, fault: 0 },
            Err(error) => Outcome {
                value: 0,
                fault: error as u64,
            },
        }
    }

    impl Memory for Adapter {
        fn read_into(&self, address: u64, out: &mut [u8]) -> Result<(), Fault> {
            // SAFETY: same callback contract as read; the output span is live.
            let status = unsafe {
                self.0.read.ok_or(Fault::Read)?(
                    self.0.context,
                    address,
                    out.as_mut_ptr(),
                    out.len() as u64,
                )
            };
            if status == 0 {
                Ok(())
            } else {
                Err(Fault::Read)
            }
        }
        fn read(&self, address: u64) -> Result<u8, Fault> {
            let mut byte = 0u8;
            // SAFETY: the entrypoint's caller guarantees callback/context lifetime;
            // output points to one live writable byte for the duration of the call.
            let status =
                unsafe { self.0.read.ok_or(Fault::Read)?(self.0.context, address, &mut byte, 1) };
            if status == 0 {
                Ok(byte)
            } else {
                Err(Fault::Read)
            }
        }
        fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Fault> {
            // SAFETY: live input slice; callback must preflight the guest range.
            let status = unsafe {
                self.0.write.ok_or(Fault::Write)?(
                    self.0.context,
                    address,
                    data.as_ptr(),
                    data.len() as u64,
                )
            };
            if status == 0 {
                Ok(())
            } else {
                Err(Fault::Write)
            }
        }
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn native_ps5_string_export_v1(index: u64) -> Export {
        export(index as usize)
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn native_ps5_dlsym_export_v1() -> Export {
        export_name("sceKernelDlsym")
    }
    /// # Safety
    /// Callbacks must be valid for the call, never unwind, and obey buffer sizes.
    /// Context must remain alive and exclusive to this synchronous invocation.
    /// Write must reject an invalid whole guest range without partial modification.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_string_call_v1(
        api: MemoryApi,
        operation: u64,
        first: u64,
        second: u64,
        count: u64,
    ) -> Outcome {
        if api.context.is_null() || api.read.is_none() || api.write.is_none() {
            return Outcome { value: 0, fault: 8 };
        }
        match call(&mut Adapter(api), operation, first, second, count) {
            Ok(value) => Outcome { value, fault: 0 },
            Err(e) => Outcome {
                value: 0,
                fault: e as u64,
            },
        }
    }
    /// # Safety
    /// Same callback/context and whole-write requirements as string_call_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_libc_call_v1(
        api: MemoryApi,
        operation: u64,
        args: crate::format::Arguments,
    ) -> Outcome {
        if api.context.is_null() || api.read.is_none() || api.write.is_none() {
            return Outcome { value: 0, fault: 8 };
        }
        let mut memory = Adapter(api);
        let result = if operation == 5 {
            crate::format::sprintf(&mut memory, args)
        } else {
            call(
                &mut memory,
                operation,
                args.registers[0],
                args.registers[1],
                args.registers[2],
            )
        };
        match result {
            Ok(value) => Outcome { value, fault: 0 },
            Err(e) => Outcome {
                value: 0,
                fault: e as u64,
            },
        }
    }
    /// # Safety
    /// Same memory callback contract as libc_call_v1. Outcome.fault carries
    /// guest errno here, and value is the printf-family integer return value.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_format_call_v1(
        api: MemoryApi,
        mode: u64,
        args: crate::format::Arguments,
    ) -> Outcome {
        if api.context.is_null() || api.read.is_none() || api.write.is_none() {
            return Outcome {
                value: u32::MAX as u64,
                fault: 14,
            };
        }
        match crate::format::formatted(&mut Adapter(api), args, mode) {
            Ok(value) => Outcome { value, fault: 0 },
            Err(error) => Outcome {
                value: u32::MAX as u64,
                fault: match error {
                    Fault::Read | Fault::Write => 14,
                    Fault::Limit => 22,
                    Fault::Allocation => 12,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Mem {
        bytes: Vec<u8>,
        writable: bool,
    }
    impl Memory for Mem {
        fn read(&self, address: u64) -> Result<u8, Fault> {
            let i = address.checked_sub(0x1000).ok_or(Fault::Read)?;
            self.bytes.get(i as usize).copied().ok_or(Fault::Read)
        }
        fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Fault> {
            if !self.writable {
                return Err(Fault::Write);
            }
            let i = usize::try_from(address.checked_sub(0x1000).ok_or(Fault::Write)?)
                .map_err(|_| Fault::Write)?;
            let end = i.checked_add(data.len()).ok_or(Fault::Write)?;
            self.bytes
                .get_mut(i..end)
                .ok_or(Fault::Write)?
                .copy_from_slice(data);
            Ok(())
        }
    }
    #[test]
    fn copies_appends_and_returns_guest_pointer() {
        let mut m = Mem {
            bytes: vec![0xa5; 128],
            writable: true,
        };
        m.bytes[..4].copy_from_slice(b"abc\0");
        m.bytes[8..11].copy_from_slice(b"de\0");
        assert_eq!(call(&mut m, 0, 0x1040, 0x1000, 0), Ok(0x1040));
        assert_eq!(call(&mut m, 1, 0x1040, 0x1008, 0), Ok(0x1040));
        assert_eq!(&m.bytes[64..71], b"abcde\0\xa5");
        assert_eq!(call(&mut m, 4, 0x1040, 0, 0), Ok(5));
    }
    #[test]
    fn comparisons_use_unsigned_bytes_and_stop_without_overreading() {
        let mut m = Mem {
            bytes: vec![255, 0, 128, 0],
            writable: false,
        };
        assert_eq!(call(&mut m, 2, 0x1000, 0x1002, 0), Ok(127));
        assert_eq!(
            call(&mut m, 2, 0x1002, 0x1000, 0),
            Ok((-127i32) as u32 as u64)
        );
        assert_eq!(call(&mut m, 3, u64::MAX, u64::MAX, 0), Ok(0));
        assert_eq!(call(&mut m, 3, 0x1000, 0x1000, 1), Ok(0));
        assert_eq!(call(&mut m, 3, 0x1000, 0x1000, u64::MAX), Ok(0));
        assert_eq!(call(&mut m, 4, 0x1003, 0, 0), Ok(0));
    }
    #[test]
    fn faults_preserve_destination_and_bound_scans() {
        let mut m = Mem {
            bytes: vec![b'x'; LIMIT + 16],
            writable: true,
        };
        let before = m.bytes.clone();
        assert_eq!(call(&mut m, 0, 0x1008, 0x1000, 0), Err(Fault::Limit));
        assert_eq!(m.bytes, before);
        m.bytes[1] = 0;
        let before = m.bytes.clone();
        let end = 0x1000 + m.bytes.len() as u64 - 1;
        assert_eq!(call(&mut m, 0, end, 0x1000, 0), Err(Fault::Write));
        assert_eq!(m.bytes, before);
        m.writable = false;
        assert_eq!(call(&mut m, 0, 0x1008, 0x1000, 0), Err(Fault::Write));
        assert_eq!(m.bytes, before);
        assert_eq!(call(&mut m, 4, u64::MAX, 0, 0), Err(Fault::Read));
    }
}
