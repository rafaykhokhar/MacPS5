//! Restricted IME ABI established by pinned PS5 SDL and its compiled consumer.
//! Primary: https://github.com/ps5-payload-dev/SDL/blob/8c56053f13ca13a0c050de613706ff69eb615836/src/video/ps5/SDL_ps5keyboard.c
//! Actual ELF parameter object 0x554190: size96, user@0, capacity@36=2048,
//! text@40=0x592040, title@72=0x593050. The caller clears4096 bytes at0xc478b,
//! establishing two-byte code units.
//! Independently SDL_wcslcpy at0x19b94 doubles its element count for memcpy and
//! at0x19ba1 writes a16-bit NUL at dst+count*2; wchar is not host macOS UTF-32.
//! Other parameter fields are zero in this consumer; nonzero unsupported
//! options/callbacks are rejected, not guessed.
//! GetStatus is polled at0xc40ee; GetResult at0xc4114 reads a16-byte result.
//! Capacity includes the terminator in this bounded model. General console
//! max-length semantics and nondefault parameter modes remain unverified.
use crate::{Result, guest_memory::Memory};

pub const PARAM_SIZE: usize = 96;
pub const RESULT_SIZE: usize = 16;
pub const MAX_TEXT_UNITS: usize = 2048;
const MAX_TITLE_UNITS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub user: i32,
    pub title: Vec<u16>,
    pub initial_text: Vec<u16>,
    /// Includes one reserved NUL unit; accepted text must be shorter.
    pub capacity_units: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    Accepted(Vec<u16>),
    Cancelled,
    Aborted,
}
/// Backends own their UI and release it on Drop. A failed begin/close may have
/// external effects; the model retains a faulted lifecycle until close succeeds.
pub trait Backend {
    fn available(&self) -> bool;
    fn begin(&mut self, request: &Request) -> Result<()>;
    fn poll(&mut self) -> Result<Option<Completion>>;
    fn close(&mut self) -> Result<()>;
}
pub struct UnsupportedBackend;
impl Backend for UnsupportedBackend {
    fn available(&self) -> bool {
        false
    }
    fn begin(&mut self, _: &Request) -> Result<()> {
        Err("IME UI backend unavailable".into())
    }
    fn poll(&mut self) -> Result<Option<Completion>> {
        Err("IME UI backend unavailable".into())
    }
    fn close(&mut self) -> Result<()> {
        Err("IME UI backend unavailable".into())
    }
}
struct Active {
    request: Request,
    output: u64,
    completion: Option<Completion>,
    faulted: bool,
}
pub struct ImeDialog {
    backend: Box<dyn Backend>,
    active: Option<Active>,
}
impl Default for ImeDialog {
    fn default() -> Self {
        Self::new(UnsupportedBackend)
    }
}
fn valid_text(text: &[u16], capacity: usize) -> Result<()> {
    if text.len() >= capacity || text.contains(&0) || String::from_utf16(text).is_err() {
        return Err("IME text exceeds capacity or is invalid UTF-16".into());
    }
    Ok(())
}
fn read_text(memory: &Memory<'_>, address: u64, capacity: usize) -> Result<Vec<u16>> {
    if address == 0 {
        return Err("Null IME text address".into());
    }
    let mut text = Vec::new();
    for index in 0..capacity {
        let unit = memory.load(
            address
                .checked_add(index as u64 * 2)
                .ok_or("IME string overflow")?,
            2,
        )? as u16;
        if unit == 0 {
            valid_text(&text, capacity)?;
            return Ok(text);
        }
        text.push(unit);
    }
    Err("IME UTF-16 string has no bounded terminator".into())
}
fn parse_request(memory: &Memory<'_>, address: u64, opaque: u64) -> Result<(Request, u64)> {
    if address == 0 || opaque != 0 {
        return Err("Unsupported IME parameter pointer/options".into());
    }
    let bytes = memory.read(address, PARAM_SIZE)?;
    // Only the four observed fields are accepted. This includes rejecting all
    // callbacks, placement, language/type flags, placeholder and reserved bytes.
    if bytes[4..36]
        .iter()
        .chain(bytes[48..72].iter())
        .chain(bytes[80..96].iter())
        .any(|b| *b != 0)
    {
        return Err("Unsupported nondefault IME parameter fields".into());
    }
    let user = i32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let capacity = u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize;
    if user == -1 || !(1..=MAX_TEXT_UNITS).contains(&capacity) {
        return Err("Invalid IME user or text capacity".into());
    }
    let output = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
    let title = u64::from_le_bytes(bytes[72..80].try_into().unwrap());
    memory.validate_write(output, capacity * 2)?;
    let initial_text = read_text(memory, output, capacity)?;
    let title = if title == 0 {
        Vec::new()
    } else {
        read_text(memory, title, MAX_TITLE_UNITS)?
    };
    Ok((
        Request {
            user,
            title,
            initial_text,
            capacity_units: capacity,
        },
        output,
    ))
}
impl ImeDialog {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self {
            backend: Box::new(backend),
            active: None,
        }
    }
    pub fn configure_backend(&mut self, backend: impl Backend + 'static) -> Result<()> {
        if self.active.is_some() {
            return Err("Cannot replace active IME backend".into());
        }
        self.backend = Box::new(backend);
        Ok(())
    }
    pub fn initialize(&mut self, memory: &Memory<'_>, parameters: u64, opaque: u64) -> Result<i32> {
        if self.active.is_some() {
            return Err("IME dialog already active".into());
        }
        let (request, output) = parse_request(memory, parameters, opaque)?;
        if !self.backend.available() {
            return Err("Unsupported sceImeDialogInit: no UI backend".into());
        }
        self.active = Some(Active {
            request,
            output,
            completion: None,
            faulted: true,
        });
        let active = self.active.as_mut().unwrap();
        self.backend.begin(&active.request)?;
        active.faulted = false;
        Ok(0)
    }
    pub fn get_status(&mut self) -> Result<i32> {
        let Some(active) = &mut self.active else {
            return Ok(0);
        };
        if active.faulted {
            return Err("IME backend faulted; termination required".into());
        }
        if active.completion.is_none() {
            active.faulted = true;
            if let Some(completion) = self.backend.poll()? {
                if let Completion::Accepted(text) = &completion {
                    valid_text(text, active.request.capacity_units)?;
                }
                active.completion = Some(completion);
            }
            active.faulted = false;
        }
        Ok(if active.completion.is_some() { 2 } else { 1 })
    }
    /// Exact write spans for the executor's return-frame protection. No backend
    /// call, result consumption, or guest mutation occurs here.
    pub fn result_output_ranges(&self, result: u64) -> Result<Vec<(u64, usize)>> {
        let active = self.active.as_ref().ok_or("IME dialog is inactive")?;
        if active.faulted {
            return Err("IME backend faulted; termination required".into());
        }
        let completion = active
            .completion
            .as_ref()
            .ok_or("IME result not finished")?;
        let mut ranges = vec![(result, RESULT_SIZE)];
        if let Completion::Accepted(text) = completion {
            ranges.push((active.output, (text.len() + 1) * 2));
        }
        Ok(ranges)
    }
    pub fn get_result(&mut self, memory: &mut Memory<'_>, result: u64) -> Result<i32> {
        let active = self.active.as_ref().ok_or("IME dialog is inactive")?;
        if active.faulted {
            return Err("IME backend faulted; termination required".into());
        }
        let completion = active
            .completion
            .as_ref()
            .ok_or("IME result not finished")?;
        if result == 0 {
            return Err("Null IME result pointer".into());
        }
        memory.validate_write(result, RESULT_SIZE)?;
        let mut encoded = [0; RESULT_SIZE];
        match completion {
            Completion::Accepted(text) => {
                let mut bytes = Vec::with_capacity((text.len() + 1) * 2);
                for unit in text {
                    bytes.extend_from_slice(&unit.to_le_bytes());
                }
                bytes.extend_from_slice(&0u16.to_le_bytes());
                // Prevalidate BOTH writes and their disjointness before either
                // is committed; guest memory mappings are fixed during a call.
                memory.validate_write(active.output, bytes.len())?;
                let output_end = active
                    .output
                    .checked_add(bytes.len() as u64)
                    .ok_or("IME text range overflow")?;
                let result_end = result
                    .checked_add(RESULT_SIZE as u64)
                    .ok_or("IME result range overflow")?;
                if active.output < result_end && result < output_end {
                    return Err("IME result overlaps text output".into());
                }
                memory.write(active.output, &bytes)?;
            }
            Completion::Cancelled => encoded[0..4].copy_from_slice(&1u32.to_le_bytes()),
            Completion::Aborted => encoded[0..4].copy_from_slice(&2u32.to_le_bytes()),
        }
        memory.write(result, &encoded)?;
        Ok(0)
    }
    pub fn terminate(&mut self) -> Result<i32> {
        let active = self
            .active
            .as_mut()
            .ok_or("IME dialog is inactive; termination semantics unverified")?;
        active.faulted = true;
        self.backend.close()?;
        self.active = None;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct Host {
        begins: usize,
        polls: usize,
        closes: usize,
        completion: Option<Completion>,
        fail_begin: bool,
        fail_close: bool,
    }
    struct TestBackend(Rc<RefCell<Host>>);
    impl Backend for TestBackend {
        fn available(&self) -> bool {
            true
        }
        fn begin(&mut self, r: &Request) -> Result<()> {
            let mut h = self.0.borrow_mut();
            h.begins += 1;
            assert_eq!(r.initial_text, vec![65]);
            if h.fail_begin {
                Err("begin".into())
            } else {
                Ok(())
            }
        }
        fn poll(&mut self) -> Result<Option<Completion>> {
            let mut h = self.0.borrow_mut();
            h.polls += 1;
            Ok(h.completion.take())
        }
        fn close(&mut self) -> Result<()> {
            let mut h = self.0.borrow_mut();
            h.closes += 1;
            if h.fail_close {
                Err("close".into())
            } else {
                Ok(())
            }
        }
    }
    fn setup(memory: &mut Memory<'_>) {
        memory.write(0x1000, &[0; 96]).unwrap();
        memory.store(0x1000, 7, 4).unwrap();
        memory.store(0x1024, 8, 4).unwrap();
        memory.store(0x1028, 0x1100, 8).unwrap();
        memory.write(0x1100, &[65, 0, 0, 0]).unwrap();
    }
    #[test]
    fn unsupported_backend_and_invalid_parameters_never_start() {
        let mut backing = [0xa5; 512];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut backing, true, false).unwrap();
        setup(&mut memory);
        let mut model = ImeDialog::default();
        assert_eq!(model.get_status().unwrap(), 0);
        assert!(model.initialize(&memory, 0x1000, 0).is_err());
        assert_eq!(model.get_status().unwrap(), 0);
        let host = Rc::new(RefCell::new(Host::default()));
        model.configure_backend(TestBackend(host.clone())).unwrap();
        for offset in [4, 8, 16, 24, 32, 48, 64, 80, 95] {
            memory.store(0x1000 + offset, 1, 1).unwrap();
            assert!(model.initialize(&memory, 0x1000, 0).is_err());
            memory.store(0x1000 + offset, 0, 1).unwrap();
        }
        assert!(model.initialize(&memory, 0x1000, 1).is_err());
        memory.store(0x1024, 2049, 4).unwrap();
        assert!(model.initialize(&memory, 0x1000, 0).is_err());
        memory.store(0x1024, 8, 4).unwrap();
        memory.write(0x1100, &[0, 0xd8, 0, 0]).unwrap();
        assert!(model.initialize(&memory, 0x1000, 0).is_err());
        assert_eq!(host.borrow().begins, 0);
    }
    #[test]
    fn completion_comes_from_backend_and_both_outputs_are_atomic() {
        let mut backing = [0xa5; 512];
        let mut readonly = [0xa5; 16];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut backing, true, false).unwrap();
        memory.add(0x2000, &mut readonly, false, false).unwrap();
        setup(&mut memory);
        let host = Rc::new(RefCell::new(Host::default()));
        let mut model = ImeDialog::new(TestBackend(host.clone()));
        model.initialize(&memory, 0x1000, 0).unwrap();
        assert_eq!(model.get_status().unwrap(), 1);
        assert!(model.get_result(&mut memory, 0x1180).is_err());
        assert!(model.configure_backend(UnsupportedBackend).is_err());
        host.borrow_mut().completion = Some(Completion::Accepted(vec![0xd83d, 0xde00]));
        assert_eq!(model.get_status().unwrap(), 2);
        assert_eq!(
            model.result_output_ranges(0x1180).unwrap(),
            vec![(0x1180, 16), (0x1100, 6)]
        );
        let mut protected_text = [0xa5; 16];
        let mut result_bytes = [0xa5; 16];
        let mut changed_mapping = Memory::default();
        changed_mapping
            .add(0x1100, &mut protected_text, false, false)
            .unwrap();
        changed_mapping
            .add(0x1180, &mut result_bytes, true, false)
            .unwrap();
        assert!(model.get_result(&mut changed_mapping, 0x1180).is_err());
        assert_eq!(changed_mapping.read(0x1180, 16).unwrap(), [0xa5; 16]);
        let before = memory.read(0x1000, 512).unwrap().to_vec();
        for output in [0, 0x2000, 0x11f8, 0x1102] {
            assert!(model.get_result(&mut memory, output).is_err());
            assert_eq!(memory.read(0x1000, 512).unwrap(), before);
        }
        model.get_result(&mut memory, 0x1180).unwrap();
        assert_eq!(memory.read(0x1100, 6).unwrap(), [0x3d, 0xd8, 0, 0xde, 0, 0]);
        assert_eq!(memory.read(0x1180, 16).unwrap(), [0; 16]);
        model.get_result(&mut memory, 0x1180).unwrap();
        assert_eq!(host.borrow().polls, 2);
        model.terminate().unwrap();
        assert_eq!(model.get_status().unwrap(), 0);
        assert_eq!(host.borrow().closes, 1);
    }
    #[test]
    fn backend_faults_require_cleanup_and_cancel_never_changes_text() {
        let mut backing = [0xa5; 512];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut backing, true, false).unwrap();
        setup(&mut memory);
        let host = Rc::new(RefCell::new(Host {
            fail_begin: true,
            ..Host::default()
        }));
        let mut model = ImeDialog::new(TestBackend(host.clone()));
        assert!(model.initialize(&memory, 0x1000, 0).is_err());
        assert!(model.get_status().is_err());
        assert!(model.initialize(&memory, 0x1000, 0).is_err());
        host.borrow_mut().fail_close = true;
        assert!(model.terminate().is_err());
        assert!(model.configure_backend(UnsupportedBackend).is_err());
        host.borrow_mut().fail_close = false;
        model.terminate().unwrap();
        host.borrow_mut().fail_begin = false;
        for (completion, outcome) in [(Completion::Cancelled, 1), (Completion::Aborted, 2)] {
            model.initialize(&memory, 0x1000, 0).unwrap();
            host.borrow_mut().completion = Some(completion);
            assert_eq!(model.get_status().unwrap(), 2);
            model.get_result(&mut memory, 0x1180).unwrap();
            assert_eq!(memory.load(0x1180, 4).unwrap(), outcome);
            assert_eq!(memory.read(0x1100, 4).unwrap(), [65, 0, 0, 0]);
            model.terminate().unwrap();
        }
        model.initialize(&memory, 0x1000, 0).unwrap();
        host.borrow_mut().completion = Some(Completion::Accepted(vec![65; 8]));
        assert!(model.get_status().is_err());
        assert!(model.get_result(&mut memory, 0x1180).is_err());
        model.terminate().unwrap();
    }
}
