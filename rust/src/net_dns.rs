//! Opt-in host DNS-SD backend. All callbacks/process/deallocation run on one
//! thread; finite poll deadline then RefDeallocate cancels outstanding work.
//! ABI and lifetime rules: Apple SDK usr/include/dns_sd.h. No guest pointers
//! cross FFI and host DNS errors remain diagnostics, not SceNet error numbers.
//! Deadline bounds our polling loop; DNS-SD daemon IPC setup/ProcessResult are
//! platform calls without a hard preemption guarantee. No worker threads leak
//! after timeout, and no callback survives operation deallocation.
#![allow(unsafe_code)]
use crate::{Result, net_resolver::Backend};
use std::time::Duration;
#[derive(Default)]
pub struct DnsBackend {
    deadline: Option<Duration>,
}
impl DnsBackend {
    /// Explicit host-network opt-in. Guest zero timing does not mean infinite.
    pub fn enabled(deadline: Duration) -> Result<Self> {
        if !(Duration::from_millis(50)..=Duration::from_secs(5)).contains(&deadline) {
            return Err("DNS deadline must be50ms..5s".into());
        }
        Ok(Self {
            deadline: Some(deadline),
        })
    }
}
impl Backend for DnsBackend {
    fn forward(&mut self, name: &str) -> Result<[u8; 4]> {
        let deadline = self.deadline.ok_or("Host DNS is disabled")?;
        if name.is_empty() || name.len() > 253 || !name.is_ascii() {
            return Err("Unsupported DNS name".into());
        }
        if let Ok(address) = name.parse::<std::net::Ipv4Addr>() {
            return Ok(address.octets());
        }
        let bytes = query(name, false, deadline)?;
        bytes
            .try_into()
            .map_err(|_| "DNS returned invalid IPv4 extent".into())
    }
    fn reverse(&mut self, address: [u8; 4]) -> Result<Vec<u8>> {
        let deadline = self.deadline.ok_or("Host DNS is disabled")?;
        query(
            &format!(
                "{}.{}.{}.{}.in-addr.arpa.",
                address[3], address[2], address[1], address[0]
            ),
            true,
            deadline,
        )
    }
}
#[cfg(not(target_os = "macos"))]
fn query(_: &str, _: bool, _: Duration) -> Result<Vec<u8>> {
    Err("DNS-SD backend requires macOS".into())
}
#[cfg(target_os = "macos")]
fn query(name: &str, reverse: bool, deadline: Duration) -> Result<Vec<u8>> {
    apple::query(name, reverse, deadline)
}
fn ptr_name(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut offset = 0;
    loop {
        let n = *bytes.get(offset).ok_or("Truncated DNS PTR response")? as usize;
        offset += 1;
        if n == 0 {
            if offset != bytes.len() || out.is_empty() {
                return Err("Invalid DNS PTR termination".into());
            }
            return Ok(out);
        }
        if n > 63 {
            return Err("Compressed/invalid DNS PTR label unsupported".into());
        }
        let label = bytes
            .get(offset..offset + n)
            .ok_or("Truncated DNS PTR label")?;
        if !label
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
        {
            return Err("Unsupported DNS PTR label bytes".into());
        }
        if !out.is_empty() {
            out.push(b'.')
        }
        out.extend_from_slice(label);
        if out.len() > 253 {
            return Err("DNS PTR name too long".into());
        }
        offset += n;
    }
}
#[cfg(target_os = "macos")]
mod apple {
    use super::*;
    use std::{
        ffi::{CString, c_char, c_void},
        ptr,
        time::Instant,
    };
    type Ref = *mut c_void;
    type AddrCallback =
        unsafe extern "C" fn(Ref, u32, u32, i32, *const c_char, *const u8, u32, *mut c_void);
    type QueryCallback = unsafe extern "C" fn(
        Ref,
        u32,
        u32,
        i32,
        *const c_char,
        u16,
        u16,
        u16,
        *const u8,
        u32,
        *mut c_void,
    );
    #[repr(C)]
    struct PollFd {
        fd: i32,
        events: i16,
        revents: i16,
    }
    #[link(name = "System")]
    unsafe extern "C" {
        fn DNSServiceGetAddrInfo(
            out: *mut Ref,
            flags: u32,
            interface: u32,
            protocol: u32,
            name: *const c_char,
            callback: AddrCallback,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceQueryRecord(
            out: *mut Ref,
            flags: u32,
            interface: u32,
            name: *const c_char,
            kind: u16,
            class: u16,
            callback: QueryCallback,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceRefSockFD(service: Ref) -> i32;
        fn DNSServiceProcessResult(service: Ref) -> i32;
        fn DNSServiceRefDeallocate(service: Ref);
        fn poll(fds: *mut PollFd, count: u32, timeout: i32) -> i32;
    }
    struct Owner(Ref);
    impl Drop for Owner {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { DNSServiceRefDeallocate(self.0) }
            }
        }
    }
    struct State {
        result: Option<Result<Vec<u8>>>,
    }
    unsafe extern "C" fn address(
        _: Ref,
        flags: u32,
        _: u32,
        error: i32,
        _: *const c_char,
        address: *const u8,
        _: u32,
        context: *mut c_void,
    ) {
        unsafe {
            let state = &mut *context.cast::<State>();
            if state.result.is_some() {
                return;
            }
            if error != 0 {
                state.result = Some(Err(format!("Host DNS address error {error}")));
                return;
            }
            if flags & 2 == 0 {
                return;
            } // Only add records; removal is not a resolution.
            if address.is_null() || *address < 16 || *address.add(1) != 2 {
                state.result = Some(Err("Host DNS returned nonIPv4 address".into()));
                return;
            }
            state.result = Some(Ok(std::slice::from_raw_parts(address.add(4), 4).to_vec()));
        }
    }
    unsafe extern "C" fn record(
        _: Ref,
        flags: u32,
        _: u32,
        error: i32,
        _: *const c_char,
        kind: u16,
        class: u16,
        len: u16,
        data: *const u8,
        _: u32,
        context: *mut c_void,
    ) {
        unsafe {
            let state = &mut *context.cast::<State>();
            if state.result.is_some() {
                return;
            }
            if error != 0 {
                state.result = Some(Err(format!("Host DNS PTR error {error}")));
                return;
            }
            if flags & 2 == 0 {
                return;
            }
            state.result = Some(if kind != 12 || class != 1 || data.is_null() || len > 255 {
                Err("Invalid DNS PTR record".into())
            } else {
                ptr_name(std::slice::from_raw_parts(data, len as usize))
            });
        }
    }
    pub fn query(name: &str, reverse: bool, duration: Duration) -> Result<Vec<u8>> {
        if name.is_empty() || name.len() > 253 || !name.is_ascii() {
            return Err("Unsupported DNS name".into());
        }
        let name = CString::new(name).map_err(|_| "NUL in DNS name")?;
        let mut state = State { result: None };
        // State outlives Owner, including every error/timeout return.
        let mut owner = Owner(ptr::null_mut());
        let context = (&mut state as *mut State).cast();
        let end = Instant::now() + duration;
        let code = unsafe {
            if reverse {
                DNSServiceQueryRecord(&mut owner.0, 0, 0, name.as_ptr(), 12, 1, record, context)
            } else {
                DNSServiceGetAddrInfo(&mut owner.0, 0, 0, 1, name.as_ptr(), address, context)
            }
        };
        if code != 0 {
            return Err(format!("Host DNS start error {code}"));
        }
        if owner.0.is_null() {
            return Err("Host DNS returned null operation".into());
        }
        let fd = unsafe { DNSServiceRefSockFD(owner.0) };
        if fd < 0 {
            return Err("Invalid host DNS descriptor".into());
        }
        while state.result.is_none() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("Host DNS deadline exceeded; query cancelled".into());
            }
            let mut descriptor = PollFd {
                fd,
                events: 1,
                revents: 0,
            };
            let ready = unsafe { poll(&mut descriptor, 1, left.as_millis().clamp(1, 100) as i32) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("Host DNS poll error: {error}"));
            }
            if descriptor.revents & !1 != 0 {
                return Err("Host DNS descriptor failed".into());
            }
            if descriptor.revents & 1 != 0 {
                let code = unsafe { DNSServiceProcessResult(owner.0) };
                if code != 0 {
                    return Err(format!("Host DNS processing error {code}"));
                }
            }
        }
        state.result.take().unwrap()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ptr_parsing_bounds() {
        assert_eq!(ptr_name(b"\x09localhost\0").unwrap(), b"localhost");
        for bad in [b"\xc0\0".as_slice(), b"\x05abc", b"\0", b"\x01a\0x"] {
            assert!(ptr_name(bad).is_err());
        }
    }
}
