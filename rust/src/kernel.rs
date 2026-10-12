//! Owned kernel model for the selected payload. Kernel storage is never mapped
//! into the guest CPU's user address space; descriptors mediate every transfer.
use crate::{Result, descriptors::Descriptors, guest_memory::Memory};
#[path = "guest_files.rs"]
mod guest_files;
pub(crate) use guest_files::{FileBacking, Quota};
pub use guest_files::{Files, STAT_SIZE, StatRequest, StatTrace};

struct Region {
    address: u64,
    bytes: Vec<u8>,
    writable: bool,
}

pub struct Kernel {
    regions: Vec<Region>,
    pub descriptors: Descriptors,
    pub files: Files,
    remaining: usize,
    errno_address: u64,
    process_parameters: std::collections::BTreeMap<u32, u64>,
    process_name: Option<(u32, Vec<u8>)>,
    sandbox_component: Option<Vec<u8>>,
    working_directory: Option<Vec<u8>>,
    logs: Vec<Vec<u8>>,
}
impl Kernel {
    /// Read-only preflight for the callable stat return-frame guard. The syscall
    /// still performs its normal pathname/error handling and tracing afterward.
    pub(crate) fn stat_will_write(&self, memory: &Memory<'_>, path: u64) -> bool {
        guest_files::read_path(memory, path)
            .and_then(|path| self.files.stat_record(&path))
            .is_ok()
    }
    pub fn errno_address(&self) -> u64 {
        self.errno_address
    }
    pub fn new(budget: usize, errno_address: u64) -> Result<Self> {
        if errno_address == 0 || errno_address.checked_add(4).is_none() {
            return Err("Invalid guest errno address".into());
        }
        Ok(Self {
            regions: Vec::new(),
            descriptors: Descriptors::default(),
            files: Files::default(),
            remaining: budget,
            errno_address,
            process_parameters: std::collections::BTreeMap::new(),
            process_name: None,
            sandbox_component: None,
            working_directory: None,
            logs: Vec::new(),
        })
    }
    pub fn take_logs(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.logs)
    }
    /// Configure the guest's sandbox path component. No host randomness or
    /// filesystem path is inferred; registration alone creates no files.
    pub fn register_sandbox_component(&mut self, component: &[u8]) -> Result<()> {
        if component.is_empty()
            || component.len() > 255
            || component.contains(&0)
            || component.contains(&b'/')
            || component == b"."
            || component == b".."
            || self.sandbox_component.is_some()
        {
            return Err("Invalid or duplicate guest sandbox component".into());
        }
        let mut terminated = component.to_vec();
        terminated.push(0);
        self.sandbox_component = Some(terminated);
        Ok(())
    }
    /// Explicit virtual cwd; this does not query the host or create directory entries.
    pub fn working_directory_bytes(&self) -> Option<&[u8]> {
        self.working_directory.as_deref()
    }
    /// Register a canonical absolute guest path, retained with its NUL terminator.
    pub fn register_working_directory(&mut self, path: &[u8]) -> Result<()> {
        if path.is_empty()
            || path.len() > 1023
            || path[0] != b'/'
            || path.contains(&0)
            || self.working_directory.is_some()
            || (path != b"/"
                && !path[1..].split(|byte| *byte == b'/').all(|part| {
                    !part.is_empty() && part != b"." && part != b".." && part.len() <= 255
                }))
        {
            return Err("Invalid or duplicate guest working directory".into());
        }
        let mut terminated = path.to_vec();
        terminated.push(0);
        self.working_directory = Some(terminated);
        Ok(())
    }
    /// Explicit guest process metadata; never derive names from host processes.
    pub fn register_process_name(&mut self, pid: u32, name: &[u8]) -> Result<()> {
        if pid == 0
            || pid > i32::MAX as u32
            || name.is_empty()
            || name.len() > 1023
            || name.contains(&0)
            || self.process_name.is_some()
        {
            return Err("Invalid or duplicate guest process name".into());
        }
        let mut terminated = name.to_vec();
        terminated.push(0);
        self.process_name = Some((pid, terminated));
        Ok(())
    }
    /// Register a process-owned user mapping, retained by the caller while the
    /// module is live. Reject incomplete records and duplicate registrations.
    pub fn register_process_parameters(
        &mut self,
        user: &Memory<'_>,
        module: u32,
        address: u64,
    ) -> Result<()> {
        if address == 0
            || self.process_parameters.contains_key(&module)
            || self.process_parameters.len() >= 256
        {
            return Err("Invalid or duplicate process parameters".into());
        }
        let size = user.load(address, 8)?;
        if !(24..=65536).contains(&size) {
            return Err("Invalid process parameter size".into());
        }
        user.read(address, size as usize)?;
        self.process_parameters.insert(module, address);
        Ok(())
    }
    pub fn map(&mut self, address: u64, bytes: Vec<u8>, writable: bool) -> Result<()> {
        let end = address
            .checked_add(bytes.len() as u64)
            .ok_or("Kernel mapping overflow")?;
        if bytes.is_empty()
            || bytes.len() > self.remaining
            || self.regions.len() >= 4096
            || self
                .regions
                .iter()
                .any(|r| address < r.address + r.bytes.len() as u64 && r.address < end)
        {
            return Err("Invalid, overlapping or over-budget kernel mapping".into());
        }
        self.remaining -= bytes.len();
        self.regions.push(Region {
            address,
            bytes,
            writable,
        });
        Ok(())
    }
    /// Host loader publication after a provider starts. The pathname backing
    /// must already belong to this exact record; only its initially null pointer
    /// is changed. This exposes no arbitrary kernel write service to the guest.
    pub fn publish_module_path(&mut self, record: u64, pathname: u64) -> Result<()> {
        let region = self
            .regions
            .iter_mut()
            .find(|r| r.address == record)
            .ok_or("Missing owned module record")?;
        if !region.writable
            || region.bytes.len() < 0x2a0
            || region.bytes[8..16] != [0; 8]
            || record.checked_add(0x180)
                != Some(u64::from_le_bytes(
                    region.bytes[0x148..0x150].try_into().unwrap(),
                ))
        {
            return Err("Invalid, readonly or already named module record".into());
        }
        let offset = usize::try_from(
            pathname
                .checked_sub(record)
                .ok_or("Module pathname precedes record")?,
        )
        .map_err(|_| "Module pathname offset overflow")?;
        let end = offset
            .checked_add(1024)
            .ok_or("Module pathname extent overflow")?;
        if offset < 0x2a0 || end > region.bytes.len() {
            return Err("Module pathname is outside owned backing".into());
        }
        let raw = &region.bytes[offset..end];
        let length = raw
            .iter()
            .position(|b| *b == 0)
            .ok_or("Unterminated module pathname")?;
        let path =
            std::str::from_utf8(&raw[..length]).map_err(|_| "Invalid module pathname encoding")?;
        if !crate::module_files::valid_path(path) || raw[length..].iter().any(|b| *b != 0) {
            return Err("Invalid module pathname or padding".into());
        }
        region.bytes[8..16].copy_from_slice(&pathname.to_le_bytes());
        Ok(())
    }
    pub fn kernel_pipe(&mut self, control: u64) -> Result<(u32, u32)> {
        let mut memory = Memory::default();
        for r in &mut self.regions {
            memory.add(r.address, &mut r.bytes, r.writable, false)?;
        }
        self.descriptors.kernel_pipe(&memory, control)
    }
    /// Callable library syscall convention: result, carry/error indicator.
    /// Validate errno storage before side effects; never forward to host syscalls.
    pub fn syscall(
        &mut self,
        user: &mut Memory<'_>,
        number: u64,
        args: [u64; 6],
    ) -> Result<(u64, bool)> {
        if !matches!(
            number,
            3 | 4 | 5 | 6 | 73 | 105 | 188 | 189 | 326 | 477 | 478 | 480 | 601 | 602 | 616 | 649
        ) {
            return Err(format!("Unsupported Rust kernel syscall {number}"));
        }
        let open_options = if number == 5 {
            let flags = args[1] as u32;
            if flags & !(3 | 8 | 0x200 | 0x400 | 0x800 | 0x100000) != 0 {
                return Err("Unsupported registered-file open flags".into());
            }
            if flags & 3 == 0 && flags & 0x400 != 0 {
                return Err("Read-only O_TRUNC remains unsupported".into());
            }
            Some(crate::descriptors::OpenOptions {
                read: flags & 3 != 1,
                write: flags & 3 == 1 || flags & 3 == 2,
                create: flags & 0x200 != 0,
                truncate: flags & 0x400 != 0,
                append: flags & 8 != 0,
                exclusive: flags & 0x800 != 0,
                cloexec: flags & 0x100000 != 0,
            })
        } else {
            None
        };
        if number == 480
            && (args[1] as i64) >= 0
            && matches!(
                self.descriptors.kind(args[0] as u32 as u64),
                Some(
                    crate::descriptors::DescriptorKind::Pipe
                        | crate::descriptors::DescriptorKind::Control
                )
            )
        {
            return Err("ftruncate for non-file endpoints is unsupported".into());
        }
        if number == 478 && matches!(args[2] as u32, 3 | 4) {
            return Err("Registered file SEEK_DATA/SEEK_HOLE are unsupported".into());
        }
        if number == 189
            && matches!(
                self.descriptors.kind(args[0] as u32 as u64),
                Some(
                    crate::descriptors::DescriptorKind::Pipe
                        | crate::descriptors::DescriptorKind::Control
                )
            )
        {
            return Err("fstat for non-file descriptor endpoints is unsupported".into());
        }
        user.validate_write(self.errno_address, 4)?;
        let mut memory = Memory::default();
        for r in &mut self.regions {
            memory.add(r.address, &mut r.bytes, r.writable, false)?;
        }
        let result = match number {
            // Pinned SDK SYS_open=5, SYS_lseek=478; fd/flags/whence are int,
            // off_t is signed64. No host syscall or implicit cwd resolution.
            5 => {
                if args[1] as u32 & 3 == 3 {
                    Err(22)
                } else {
                    match guest_files::read_path(user, args[0]) {
                        Err(error) => Err(error),
                        Ok(path) => {
                            let options = open_options.expect("Open options decoded");
                            // Explicit virtual writable directories currently use
                            // mode0666 with no umask; other creation modes are not
                            // silently rewritten. Existing file mode is untouched.
                            if options.create
                                && self.files.backing(&path).is_err()
                                && args[2] as u32 != 0o666
                            {
                                return Err(
                                    "Registered-file creation mode outside0666 is unsupported"
                                        .into(),
                                );
                            }
                            self.descriptors
                                .open_file_options(&mut self.files, &path, options)?
                                .map(|token| u64::from(token.descriptor()))
                        }
                    }
                }
            }
            480 => {
                if (args[1] as i64) < 0 {
                    Err(22)
                } else {
                    self.descriptors
                        .file_token(args[0] as u32 as u64)
                        .and_then(|token| self.descriptors.truncate_file(&token, args[1]))
                        .map(|()| 0)
                }
            }
            478 => {
                self.descriptors
                    .seek(args[0] as u32 as u64, args[1] as i64, args[2] as u32 as i32)
            }
            188 => self.files.stat(user, args[0], args[1]),
            // FreeBSD kern_fstat obtains descriptor metadata before sys_fstat copyout.
            189 => self
                .descriptors
                .file_token(args[0] as u32 as u64)
                .and_then(|token| self.descriptors.stat_file(&token))
                .and_then(|record| {
                    if args[1] == 0 {
                        Err(14)
                    } else {
                        user.write(args[1], &record).map(|()| 0).map_err(|_| 14)
                    }
                }),
            326 => {
                // SDK __rtld_find_file and ELF 0x4b9a: (output, capacity).
                // A successful __getcwd syscall returns 0, unlike libc getcwd.
                let path = self
                    .working_directory
                    .as_ref()
                    .ok_or("Guest working directory is not configured for syscall 326")?;
                // FreeBSD 11.4 kern___getcwd takes u_int and clamps to
                // MAXPATHLEN. Its path builder reports ENOMEM if it cannot fit.
                let capacity = (args[1] as u32).min(1024) as usize;
                if capacity < 2 {
                    Err(22)
                } else if capacity < path.len() {
                    Err(12)
                } else if args[0] == 0 {
                    Err(14)
                } else {
                    user.write(args[0], path).map(|()| 0).map_err(|_| 14)
                }
            }
            602 => {
                // SDK crt/rtld.c and this ELF at 0x4aba: (0, bytes, &u64_capacity).
                // Configured model policy: full NUL-terminated copy, output size
                // includes NUL; short buffers return ERANGE without truncation.
                // This caller does not establish console truncation/size-query
                // behavior. No configured component remains explicitly unsupported.
                let component = self
                    .sandbox_component
                    .as_ref()
                    .ok_or("Guest sandbox component is not configured for syscall 602")?;
                copy_sandbox_component(user, component, args[0], args[1], args[2])
            }
            601 => {
                // SDK klog uses (7, NUL-terminated message, 0). Capture in
                // process-owned storage, never forward a guest pointer to OS logging.
                if args[0] != 7 || args[2] != 0 {
                    Err(22)
                } else if args[1] == 0 {
                    Err(14)
                } else {
                    let read = || -> std::result::Result<Vec<u8>, u32> {
                        let mut message = Vec::new();
                        for offset in 0..4096 {
                            let address = args[1].checked_add(offset).ok_or(14u32)?;
                            let byte = user.load(address, 1).map_err(|_| 14u32)? as u8;
                            if byte == 0 {
                                return Ok(message);
                            }
                            message.push(byte);
                        }
                        Err(22)
                    };
                    match read() {
                        Ok(message)
                            if self.logs.len() < 256
                                && self.logs.iter().map(Vec::len).sum::<usize>()
                                    + message.len()
                                    <= 65536 =>
                        {
                            self.logs.push(message);
                            Ok(0)
                        }
                        Ok(_) => Err(12),
                        Err(error) => Err(error),
                    }
                }
            }
            616 => {
                // ps5-payload-dev/sdk crt/klog.c and crt/rtld_payload.c:
                // (pid, output, capacity). Restricted to complete names; no
                // claim of general console truncation or process enumeration.
                let (pid, name) = self
                    .process_name
                    .as_ref()
                    .ok_or("Guest process name is not configured")?;
                if args[0] as u32 != *pid {
                    Err(3)
                } else if args[2] < name.len() as u64 || args[2] > 1024 {
                    Err(22)
                } else if args[1] == 0 {
                    Err(14)
                } else {
                    user.write(args[1], name).map(|()| 0).map_err(|_| 14)
                }
            }
            477 => user.map_anonymous(args),
            73 => user.unmap_anonymous(args[0], args[1]),
            649 => {
                if args[1] != 8 {
                    Err(22)
                } else if let Some(&address) = self.process_parameters.get(&(args[0] as u32)) {
                    if args[2] == 0 {
                        Err(14)
                    } else {
                        user.store(args[2], address, 8).map(|()| 0).map_err(|_| 14)
                    }
                } else {
                    Err(3)
                }
            }
            // Explicit LP64 IOSIZE_MAX=SSIZE_MAX domain (FreeBSD permits an
            // optional INT_MAX clamp). Check count before descriptor lookup.
            3 | 4 if args[2] > i64::MAX as u64 => Err(22),
            3 | 4 => self.descriptors.transfer(
                user,
                &mut memory,
                args[0] as u32 as u64,
                args[1],
                args[2],
                number == 4,
            ),
            _ => {
                let error = if number == 6 {
                    self.descriptors.close(args[0] as u32 as u64)
                } else {
                    self.descriptors.set_socket_option(
                        user,
                        &mut memory,
                        args[..5].try_into().unwrap(),
                    )
                };
                if error == 0 { Ok(0) } else { Err(error) }
            }
        };
        match result {
            Ok(value) => Ok((value, false)),
            Err(error) => {
                user.store(self.errno_address, error.into(), 4)?;
                Ok((u64::MAX, true))
            }
        }
    }
}

fn copy_sandbox_component(
    user: &mut Memory<'_>,
    component: &[u8],
    mode: u64,
    output: u64,
    length: u64,
) -> std::result::Result<u64, u32> {
    if mode != 0 {
        return Err(22);
    }
    if output == 0 || length == 0 {
        return Err(14);
    }
    let capacity = user.load(length, 8).map_err(|_| 14u32)?;
    if capacity < component.len() as u64 {
        return Err(34);
    }
    let output_end = output.checked_add(component.len() as u64).ok_or(14u32)?;
    let length_end = length.checked_add(8).ok_or(14u32)?;
    if output < length_end && length < output_end {
        return Err(22);
    }
    user.validate_write(output, component.len())
        .map_err(|_| 14u32)?;
    user.validate_write(length, 8).map_err(|_| 14u32)?;
    // Both disjoint output ranges have been validated against the same stable
    // borrowed mappings; no allocation or mapping mutation occurs between writes.
    user.write(output, component).map_err(|_| 14u32)?;
    user.store(length, component.len() as u64, 8)
        .map_err(|_| 14u32)?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn descriptor_stat_matches_path_stat_without_moving_cursor() {
        let mut kernel = Kernel::new(4096, 0x1000).unwrap();
        kernel.files.register(b"/file", vec![7; 513]).unwrap();
        let token = kernel
            .descriptors
            .open_file(&kernel.files, b"/file")
            .unwrap();
        let other = kernel
            .descriptors
            .open_file(&kernel.files, b"/file")
            .unwrap();
        let record = kernel.descriptors.stat_file(&token).unwrap();
        assert_eq!(record, kernel.descriptors.stat_file(&other).unwrap());
        kernel.descriptors.seek_file(&token, 99, 0).unwrap();
        let mut bytes = [0xa5; 384];
        bytes[16..22].copy_from_slice(b"/file\0");
        let mut user = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 188, [0x1010, 0x1040, 0, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 189, [0x1_0000_0003, 0x10c0, 0, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(
            user.read(0x1040, STAT_SIZE).unwrap(),
            user.read(0x10c0, STAT_SIZE).unwrap()
        );
        assert_eq!(kernel.descriptors.file_position(&token), Ok(99));
        for (fd, output, errno) in [
            (u64::MAX, u64::MAX, 9),
            (3, 0, 14),
            (3, 0x1170, 14),
            (3, u64::MAX, 14),
        ] {
            let before = user.read(0x1004, 380).unwrap().to_vec();
            assert_eq!(
                kernel
                    .syscall(&mut user, 189, [fd, output, 0, 0, 0, 0])
                    .unwrap(),
                (u64::MAX, true)
            );
            assert_eq!(user.load(0x1000, 4).unwrap(), errno);
            assert_eq!(user.read(0x1004, 380).unwrap(), before);
            assert_eq!(kernel.descriptors.file_position(&token), Ok(99));
        }
        // An open description retains its own file identity even if the host
        // replaces the explicit registration namespace afterward.
        kernel.files = Files::default();
        kernel.files.register(b"/file", vec![8]).unwrap();
        assert_eq!(kernel.descriptors.stat_file(&token).unwrap(), record);
        assert_eq!(kernel.descriptors.close_file(&token), 0);
        assert_eq!(kernel.descriptors.stat_file(&token), Err(9));
        let (control, _) = kernel.descriptors.packet_control_pair().unwrap();
        assert!(
            kernel
                .syscall(&mut user, 189, [control.into(), 0x1040, 0, 0, 0, 0])
                .unwrap_err()
                .contains("unsupported")
        );
    }
    #[test]
    fn registered_file_syscalls_share_offsets_and_preserve_error_order() {
        let mut kernel = Kernel::new(4096, 0x1000).unwrap();
        kernel.files.register(b"/file", b"abcdef".to_vec()).unwrap();
        let mut bytes = [0xa5; 64];
        bytes[16..22].copy_from_slice(b"/file\0");
        let mut user = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        user.store(0x1000, 77, 4).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 5, [0x1010, 0, 0, 0, 0, 0])
                .unwrap(),
            (3, false)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 77);
        let token = kernel.descriptors.file_token(3).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 3, [0x1_0000_0003, 0x1020, 3, 0, 0, 0])
                .unwrap(),
            (3, false)
        );
        assert_eq!(kernel.descriptors.file_position(&token), Ok(3));
        assert_eq!(user.read(0x1020, 3).unwrap(), b"abc");
        assert_eq!(
            kernel
                .syscall(&mut user, 478, [3, (-1i64) as u64, 2, 0, 0, 0])
                .unwrap(),
            (5, false)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 3, [3, 0x103f, 50, 0, 0, 0])
                .unwrap(),
            (1, false)
        );
        assert_eq!(user.load(0x103f, 1).unwrap(), b'f' as u64);
        assert_eq!(
            kernel
                .syscall(&mut user, 3, [3, u64::MAX, 50, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 478, [3, i64::MAX as u64, 1, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 84);
        assert_eq!(kernel.descriptors.file_position(&token), Ok(6));
        assert_eq!(
            kernel
                .syscall(&mut user, 5, [0x1010, 1, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 13);
        assert_eq!(
            kernel.syscall(&mut user, 6, [3, 0, 0, 0, 0, 0]).unwrap(),
            (0, false)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 5, [0x1010, 0, 0, 0, 0, 0])
                .unwrap(),
            (3, false)
        );
        assert_eq!(kernel.descriptors.close_file(&token), 9);
        assert_eq!(
            kernel
                .syscall(&mut user, 3, [u64::MAX, 0x1020, u64::MAX, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 22);
        assert_eq!(
            kernel
                .syscall(&mut user, 4, [u64::MAX, 0x1020, u64::MAX, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 22);
        // Missing errno output must fail before moving the shared cursor.
        let fresh = kernel.descriptors.file_token(3).unwrap();
        let mut output = [0xa5; 8];
        let mut no_errno = Memory::default();
        no_errno.add(0x2000, &mut output, true, false).unwrap();
        assert!(
            kernel
                .syscall(&mut no_errno, 3, [3, 0x2000, 1, 0, 0, 0])
                .is_err()
        );
        assert_eq!(kernel.descriptors.file_position(&fresh), Ok(0));
        assert_eq!(no_errno.read(0x2000, 8).unwrap(), &[0xa5; 8]);
    }
    #[test]
    fn module_path_publication_is_owned_validated_and_atomic() {
        const BASE: u64 = 0x10000;
        let mut bytes = vec![0; 0x2a0 + 1024];
        bytes[0x148..0x150].copy_from_slice(&(BASE + 0x180).to_le_bytes());
        let name = b"/system/common/lib/libSceSysmodule.sprx";
        bytes[0x2a0..0x2a0 + name.len()].copy_from_slice(name);
        for writable in [false, true] {
            let mut kernel = Kernel::new(bytes.len(), 0x4000).unwrap();
            kernel.map(BASE, bytes.clone(), writable).unwrap();
            for pointer in [0, BASE + 8, BASE + 0x2a1, u64::MAX] {
                assert!(kernel.publish_module_path(BASE, pointer).is_err());
                assert_eq!(kernel.regions[0].bytes, bytes);
            }
            if writable {
                kernel.publish_module_path(BASE, BASE + 0x2a0).unwrap();
                assert_eq!(
                    &kernel.regions[0].bytes[8..16],
                    &(BASE + 0x2a0).to_le_bytes()
                );
                let published = kernel.regions[0].bytes.clone();
                assert!(kernel.publish_module_path(BASE, BASE + 0x2a0).is_err());
                assert_eq!(kernel.regions[0].bytes, published);
            } else {
                assert!(kernel.publish_module_path(BASE, BASE + 0x2a0).is_err());
                assert_eq!(kernel.regions[0].bytes, bytes);
            }
            assert_eq!(kernel.remaining, 0);
        }
        for malformed in [&b"/"[..], b"relative", b"/a/../b"] {
            // A root-only filename and traversal/relative paths are not module files.
            let mut bad = bytes.clone();
            bad[0x2a0..].fill(0);
            bad[0x2a0..0x2a0 + malformed.len()].copy_from_slice(malformed);
            let mut kernel = Kernel::new(bad.len(), 0x4000).unwrap();
            kernel.map(BASE, bad.clone(), true).unwrap();
            assert!(kernel.publish_module_path(BASE, BASE + 0x2a0).is_err());
            assert_eq!(kernel.regions[0].bytes, bad);
        }
    }
    #[test]
    fn process_path_configuration_is_explicit_and_bounded() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        for name in [&b""[..], b".", b"..", b"/guest", b"a/b", b"a\0b"] {
            assert!(kernel.register_sandbox_component(name).is_err());
        }
        assert!(kernel.register_sandbox_component(&[b'x'; 256]).is_err());
        kernel.register_sandbox_component(&[b'x'; 255]).unwrap();
        assert!(kernel.register_sandbox_component(b"second").is_err());
        for path in [
            &b""[..],
            b"relative",
            b"/a//b",
            b"/a/../b",
            b"/a/./b",
            b"/a/",
            b"/a\0b",
        ] {
            assert!(kernel.register_working_directory(path).is_err());
        }
        assert!(kernel.register_working_directory(&[b'/'; 1024]).is_err());
        let component_too_long = [&b"/"[..], &[b'x'; 256]].concat();
        assert!(
            kernel
                .register_working_directory(&component_too_long)
                .is_err()
        );
        kernel.register_working_directory(b"/").unwrap();
        assert!(kernel.register_working_directory(b"/second").is_err());
    }
    #[test]
    fn sandbox_path_validates_both_outputs_before_atomic_commit() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        let mut errno = [0u8; 4];
        let mut output = [0xa5u8; 32];
        let mut length = 32u64.to_le_bytes();
        let mut readonly_output = [0xa5u8; 32];
        let mut readonly_length = 32u64.to_le_bytes();
        let mut short = [0xa5u8; 13];
        let mut alias = [0xa5u8; 64];
        let mut user = Memory::default();
        user.add(0x1000, &mut errno, true, false).unwrap();
        user.add(0x2000, &mut output, true, false).unwrap();
        user.add(0x3000, &mut length, true, false).unwrap();
        user.add(0x4000, &mut readonly_output, false, false)
            .unwrap();
        user.add(0x5000, &mut readonly_length, false, false)
            .unwrap();
        user.add(0x6000, &mut short, true, false).unwrap();
        user.add(0x7000, &mut alias, true, false).unwrap();
        assert!(
            kernel
                .syscall(&mut user, 602, [0, 0x2000, 0x3000, 0, 0, 0])
                .is_err()
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 0);
        kernel.register_sandbox_component(b"guest_sandbox").unwrap();
        for capacity in [14, 255, u64::MAX] {
            user.write(0x2000, &[0xa5; 32]).unwrap();
            user.store(0x3000, capacity, 8).unwrap();
            assert_eq!(
                kernel
                    .syscall(&mut user, 602, [0, 0x2000, 0x3000, 0, 0, 0])
                    .unwrap(),
                (0, false)
            );
            assert_eq!(user.read(0x2000, 14).unwrap(), b"guest_sandbox\0");
            assert_eq!(user.read(0x200e, 18).unwrap(), &[0xa5; 18]);
            assert_eq!(user.load(0x3000, 8).unwrap(), 14);
        }
        for (mode, buffer, size_pointer, capacity, error) in [
            (1, 0x2000, 0x3000, 32, 22),
            (0, 0x2000, 0x3000, 13, 34),
            (0, 0x2000, 0x3000, 0, 34),
            (0, 0, 0x3000, 32, 14),
            (0, 0x2000, 0, 32, 14),
            (0, 0x2000, 0x9000, 32, 14),
            (0, 0x4000, 0x3000, 32, 14),
            (0, 0x2000, 0x5000, 32, 14),
            (0, 0x6000, 0x3000, 32, 14),
            (0, u64::MAX - 5, 0x3000, 32, 14),
            (0, 0x7000, 0x7008, 32, 22),
        ] {
            user.write(0x2000, &[0xa5; 32]).unwrap();
            user.write(0x7000, &[0xa5; 64]).unwrap();
            user.store(0x3000, capacity, 8).unwrap();
            user.store(0x7008, capacity, 8).unwrap();
            let before_alias = user.read(0x7000, 64).unwrap().to_vec();
            assert_eq!(
                kernel
                    .syscall(&mut user, 602, [mode, buffer, size_pointer, 0, 0, 0])
                    .unwrap(),
                (u64::MAX, true)
            );
            assert_eq!(user.load(0x1000, 4).unwrap(), error);
            assert_eq!(user.load(0x3000, 8).unwrap(), capacity);
            assert_eq!(user.load(0x5000, 8).unwrap(), 32);
            assert_eq!(user.read(0x2000, 32).unwrap(), &[0xa5; 32]);
            assert_eq!(user.read(0x4000, 32).unwrap(), &[0xa5; 32]);
            assert_eq!(user.read(0x6000, 13).unwrap(), &[0xa5; 13]);
            assert_eq!(user.read(0x7000, 64).unwrap(), before_alias);
        }
        // Disjoint outputs may share a mapping; rejected overlap is about bytes,
        // not a blanket ban on using one guest allocation.
        user.store(0x7010, 32, 8).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 602, [0, 0x7000, 0x7010, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(user.read(0x7000, 14).unwrap(), b"guest_sandbox\0");
        assert_eq!(user.load(0x7010, 8).unwrap(), 14);
        kernel.errno_address = 0x9000;
        user.store(0x3000, 32, 8).unwrap();
        assert!(
            kernel
                .syscall(&mut user, 602, [0, 0x2000, 0x3000, 0, 0, 0])
                .is_err()
        );
        assert_eq!(user.read(0x2000, 32).unwrap(), &[0xa5; 32]);
        assert_eq!(user.load(0x3000, 8).unwrap(), 32);
    }
    #[test]
    fn cwd_uses_guest_configuration_unsigned_capacity_and_atomic_copy() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        let mut errno = [0u8; 4];
        let mut output = [0xa5u8; 32];
        let mut readonly = [0xa5u8; 32];
        let mut short = [0xa5u8; 5];
        let mut user = Memory::default();
        user.add(0x1000, &mut errno, true, false).unwrap();
        user.add(0x2000, &mut output, true, false).unwrap();
        user.add(0x3000, &mut readonly, false, false).unwrap();
        user.add(0x4000, &mut short, true, false).unwrap();
        assert!(
            kernel
                .syscall(&mut user, 326, [0x2000, 32, 0, 0, 0, 0])
                .is_err()
        );
        kernel.register_working_directory(b"/app0").unwrap();
        for capacity in [6, 1024, u64::MAX] {
            user.write(0x2000, &[0xa5; 32]).unwrap();
            assert_eq!(
                kernel
                    .syscall(&mut user, 326, [0x2000, capacity, 0, 0, 0, 0])
                    .unwrap(),
                (0, false)
            );
            assert_eq!(user.read(0x2000, 6).unwrap(), b"/app0\0");
            assert_eq!(user.read(0x2006, 26).unwrap(), &[0xa5; 26]);
        }
        for (buffer, capacity, error) in [
            (0x2000, 0, 22),
            (0x2000, 1, 22),
            (0x2000, 3, 12),
            (0x2000, 0x100000001, 22),
            (0, 6, 14),
            (0x3000, 6, 14),
            (0x4000, 6, 14),
            (u64::MAX - 2, 6, 14),
        ] {
            user.write(0x2000, &[0xa5; 32]).unwrap();
            assert_eq!(
                kernel
                    .syscall(&mut user, 326, [buffer, capacity, 0, 0, 0, 0])
                    .unwrap(),
                (u64::MAX, true)
            );
            assert_eq!(user.load(0x1000, 4).unwrap(), error);
            assert_eq!(user.read(0x2000, 32).unwrap(), &[0xa5; 32]);
            assert_eq!(user.read(0x3000, 32).unwrap(), &[0xa5; 32]);
            assert_eq!(user.read(0x4000, 5).unwrap(), &[0xa5; 5]);
        }
    }
    #[test]
    fn stat_returns_guest_errno_and_validates_errno_before_file_effects() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        kernel
            .files
            .register(b"/fixture", b"payload".to_vec())
            .unwrap();
        kernel.files.set_stat_tracing(true);
        let mut errno = 0x55555555u32.to_le_bytes();
        let mut path = *b"/fixture\0";
        let mut missing = *b"/missing\0";
        let mut output = [0xa5; STAT_SIZE];
        let mut user = Memory::default();
        user.add(0x1000, &mut errno, true, false).unwrap();
        user.add(0x2000, &mut path, false, false).unwrap();
        user.add(0x2100, &mut missing, false, false).unwrap();
        user.add(0x3000, &mut output, true, false).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 188, [0x2000, 0x3000, 0, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(user.load(0x3000 + 72, 8).unwrap(), 7);
        assert_eq!(user.load(0x1000, 4).unwrap(), 0x55555555);
        user.write(0x3000, &[0xa5; STAT_SIZE]).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 188, [0x2100, 0x3000, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 2);
        assert_eq!(user.read(0x3000, STAT_SIZE).unwrap(), &[0xa5; STAT_SIZE]);
        assert_eq!(
            kernel
                .syscall(&mut user, 188, [0x2000, 0x3001, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 14);
        assert_eq!(user.read(0x3000, STAT_SIZE).unwrap(), &[0xa5; STAT_SIZE]);
        let trace = kernel.files.take_stat_trace();
        assert_eq!(
            trace.requests.iter().map(|r| r.error).collect::<Vec<_>>(),
            [None, Some(2), Some(14)]
        );
        kernel.errno_address = 0x4000;
        assert!(
            kernel
                .syscall(&mut user, 188, [0x2000, 0x3000, 0, 0, 0, 0])
                .is_err()
        );
        assert!(kernel.files.take_stat_trace().requests.is_empty());
        assert_eq!(user.read(0x3000, STAT_SIZE).unwrap(), &[0xa5; STAT_SIZE]);
    }
    #[test]
    fn logging_captures_bytes_and_rejects_partial_or_over_budget_messages() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        let mut errno = [0; 4];
        let mut text = *b"hello\0";
        let mut user = Memory::default();
        user.add(0x1000, &mut errno, true, false).unwrap();
        user.add(0x2000, &mut text, false, false).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 601, [7, 0x2000, 0, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(kernel.take_logs(), [b"hello".to_vec()]);
        assert_eq!(
            kernel
                .syscall(&mut user, 601, [6, 0x2000, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 601, [7, u64::MAX, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert!(kernel.take_logs().is_empty());
        for _ in 0..256 {
            assert_eq!(
                kernel
                    .syscall(&mut user, 601, [7, 0x2000, 0, 0, 0, 0])
                    .unwrap(),
                (0, false)
            );
        }
        assert_eq!(
            kernel
                .syscall(&mut user, 601, [7, 0x2000, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 12);
        assert_eq!(kernel.take_logs().len(), 256);
    }
    #[test]
    fn process_name_lookup_is_owned_bounded_and_atomic() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        let mut bytes = [0xa5; 64];
        let mut readonly = [0xa5; 32];
        let mut user = Memory::default();
        user.add(0x1000, &mut bytes, true, false).unwrap();
        user.add(0x2000, &mut readonly, false, false).unwrap();
        assert!(
            kernel
                .syscall(&mut user, 616, [7, 0x1010, 32, 0, 0, 0])
                .is_err()
        );
        assert!(kernel.register_process_name(0, b"guest").is_err());
        assert!(kernel.register_process_name(7, b"bad\0name").is_err());
        kernel.register_process_name(7, b"guest").unwrap();
        assert!(kernel.register_process_name(7, b"other").is_err());
        for (pid, output, capacity, errno) in [
            (8, 0x1010, 32, 3),
            (7, 0x1010, 5, 22),
            (7, 0x1010, 1025, 22),
            (7, 0x103e, 32, 14),
            (7, 0x2000, 32, 14),
            (7, 0, 32, 14),
        ] {
            assert_eq!(
                kernel
                    .syscall(&mut user, 616, [pid, output, capacity, 0, 0, 0])
                    .unwrap(),
                (u64::MAX, true)
            );
            assert_eq!(user.load(0x1000, 4).unwrap(), errno);
            assert_eq!(user.read(0x1010, 32).unwrap(), &[0xa5; 32]);
        }
        assert_eq!(
            kernel
                .syscall(&mut user, 616, [7, 0x1010, 32, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(user.read(0x1010, 6).unwrap(), b"guest\0");
        assert_eq!(user.read(0x1016, 26).unwrap(), &[0xa5; 26]);
    }
    #[test]
    fn metadata_registration_and_lookup_preserve_failed_outputs() {
        let mut kernel = Kernel::new(0, 0x1000).unwrap();
        let mut storage = [0u8; 128];
        let mut user = Memory::default();
        user.add(0x1000, &mut storage, true, false).unwrap();
        user.store(0x1020, 24, 8).unwrap();
        kernel
            .register_process_parameters(&user, 2, 0x1020)
            .unwrap();
        assert!(
            kernel
                .register_process_parameters(&user, 2, 0x1020)
                .is_err()
        );
        user.store(0x1040, 23, 8).unwrap();
        assert!(
            kernel
                .register_process_parameters(&user, 3, 0x1040)
                .is_err()
        );
        user.store(0x1040, 65536, 8).unwrap();
        assert!(
            kernel
                .register_process_parameters(&user, 3, 0x1040)
                .is_err()
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 649, [2, 8, 0x1060, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(user.load(0x1060, 8).unwrap(), 0x1020);
        for (args, errno) in [
            ([3, 8, 0x1060, 0, 0, 0], 3),
            ([2, 7, 0x1060, 0, 0, 0], 22),
            ([2, 8, 0x107c, 0, 0, 0], 14),
            ([2, 8, 0, 0, 0, 0], 14),
        ] {
            assert_eq!(
                kernel.syscall(&mut user, 649, args).unwrap(),
                (u64::MAX, true)
            );
            assert_eq!(user.load(0x1000, 4).unwrap(), errno);
            assert_eq!(user.load(0x1060, 8).unwrap(), 0x1020);
        }
        for module in 3..258 {
            kernel
                .register_process_parameters(&user, module, 0x1020)
                .unwrap();
        }
        assert!(
            kernel
                .register_process_parameters(&user, 258, 0x1020)
                .is_err()
        );
    }
    #[test]
    fn mapping_budget_and_errno_validation_precede_side_effects() {
        let mut kernel = Kernel::new(24, 0x1000).unwrap();
        kernel.map(0x2000, vec![0; 24], true).unwrap();
        assert!(kernel.map(0x3000, vec![0], true).is_err());
        assert!(kernel.map(0x2000, vec![0], true).is_err());
        let (reader, _) = kernel.kernel_pipe(0x2000).unwrap();
        let mut errno = [0u8; 4];
        let mut user = Memory::default();
        assert!(
            kernel
                .syscall(&mut user, 6, [reader.into(), 0, 0, 0, 0, 0])
                .is_err()
        );
        user.add(0x1000, &mut errno, true, false).unwrap();
        assert_eq!(
            kernel
                .syscall(&mut user, 6, [reader.into(), 0, 0, 0, 0, 0])
                .unwrap(),
            (0, false)
        );
        assert_eq!(
            kernel
                .syscall(&mut user, 6, [reader.into(), 0, 0, 0, 0, 0])
                .unwrap(),
            (u64::MAX, true)
        );
        assert_eq!(user.load(0x1000, 4).unwrap(), 9);
        assert!(kernel.syscall(&mut user, 21, [0; 6]).is_err());
        assert_eq!(user.load(0x1000, 4).unwrap(), 9);
    }
}
