//! Rust-owned translated-module executor with dependency mappings and bounded services.
//! Hosts using the standalone executor must retain the exact supplied Image
//! objects and backing throughout suspension, including image-resident semaphore
//! waits. Equal bytes at a reused load bias do not fulfill that host contract.
//! Managed Modules enforce guest stop/unload lifetime guards; this low-level API
//! does not pin host-owned images or detect host replacement of their backing.
#![allow(unsafe_code)]
use crate::{
    Result,
    guest_memory::{DataRegion, Memory},
    legacy_cpu::Cpu,
    loader::Image,
    modules::{Arguments, Executor},
};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
type FaultStack = crate::guest_callback::RetainedStacks;
struct StackIdentity {
    address: u64,
    length: usize,
    backing: usize,
    lease: Weak<()>,
}
/// A failed dispatch cannot release backing still owned by a semaphore waiter.
/// Successful suspension transfers the bytes out, leaving this guard empty.
struct ExecutionStack {
    condition_transfer: bool,
    lease: Rc<()>,
    bytes: Vec<u8>,
    address: u64,
    pending: Rc<Cell<bool>>,
    fault: FaultStack,
}
impl std::ops::Deref for ExecutionStack {
    type Target = Vec<u8>;
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}
impl std::ops::DerefMut for ExecutionStack {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bytes
    }
}
impl ExecutionStack {
    fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}
impl Drop for ExecutionStack {
    fn drop(&mut self) {
        if (self.pending.get() || self.condition_transfer) && !self.bytes.is_empty() {
            let bytes = self.take();
            self.fault
                .borrow_mut()
                .push((self.address, bytes, self.lease.clone()));
        }
    }
}
fn retained_overlap(
    semaphores: &crate::guest_semaphore::GuestSemaphore,
    callbacks: &crate::guest_callback::Registry,
    address: u64,
    length: usize,
) -> bool {
    semaphores.has_pending_overlap(address, length) || callbacks.overlaps(address, length)
}
const ERROR_TEXT: u64 = 0x7fff00000000;
// Disjoint from the 128-byte ERROR_TEXT buffer. Owned per executor; future
// multi-thread guest execution will need per-guest-thread network errno slots.
const NET_ERRNO: u64 = 0x7fff00000100;
const LOCALE_NAME: u64 = 0x7fff00000200;
const GDTOA_POOL: u64 = 0x7fff00002000;
pub const MAIN_GUEST_THREAD: u64 = 1;
fn semaphore_storage_owned(
    memory: &Memory<'_>,
    address: u64,
    images: &[(u64, usize)],
    stacks: &[StackIdentity],
) -> Result<bool> {
    let length = crate::guest_semaphore::SIZE;
    let end = address
        .checked_add(length as u64)
        .ok_or("Semaphore storage overflow")?;
    if memory.owned_allocation_contains(address, length)
        || images
            .iter()
            .any(|&(base, size)| address >= base && end <= base + size as u64)
    {
        return Ok(true);
    }
    let backing = memory.read(address, length)?.as_ptr() as usize;
    Ok(stacks.iter().any(|s| {
        s.lease.strong_count() != 0
            && address >= s.address
            && end <= s.address + s.length as u64
            && backing == s.backing + (address - s.address) as usize
    }))
}
fn force_abort_default(
    abort: &mut crate::guest_abort::Abort,
    actions: &mut crate::guest_signal_action::Store,
    tls: &mut crate::guest_tls::GuestTls,
    thread: u64,
) -> Result<i32> {
    if abort.phase() != crate::guest_abort::Phase::ForceDefault {
        return Err("Abort forced default at wrong phase".into());
    }
    let snapshot = actions.snapshot(crate::guest_abort::SIGABRT)?;
    let action = actions.prepare_snapshot(&snapshot, Some(crate::guest_abort::forced_default()))?;
    tls.replace_signal_mask(thread, crate::guest_abort::abort_mask())?;
    action.commit();
    abort
        .default_installed()
        .expect("Validated forced-default phase");
    let snapshot = actions
        .snapshot(crate::guest_abort::SIGABRT)
        .expect("Valid fixed signal");
    let decision = abort
        .prepare_raise(&snapshot)
        .expect("Committed default second raise")
        .commit();
    match decision {
        crate::guest_abort::Decision::Terminate { signal } => Ok(signal),
        _ => unreachable!("Second raise uses default disposition"),
    }
}
#[cfg(test)]
mod vasprintf_dispatch_tests {
    use super::*;
    const SP: u64 = 0x1010;
    const RET: u64 = 0x4455;
    const OUT: u64 = 0x1020;
    fn bytes(format: &[u8]) -> Vec<u8> {
        let mut data = vec![0; 4096];
        data[16..24].copy_from_slice(&RET.to_le_bytes());
        data[32..40].copy_from_slice(&0xaabbccu64.to_le_bytes());
        data[0x100..0x100 + format.len()].copy_from_slice(format);
        data[0x204..0x208].copy_from_slice(&48u32.to_le_bytes());
        data[0x210..0x218].copy_from_slice(&0x1300u64.to_le_bytes());
        data[0x300..0x308].copy_from_slice(&13u64.to_le_bytes());
        data[0x308..0x310].copy_from_slice(&0x1400u64.to_le_bytes());
        data[0x400..0x403].copy_from_slice(b"ok\0");
        data
    }
    #[test]
    fn success_does_not_require_errno_and_publishes_owned_output() {
        let mut data = bytes(b"%d/%s\0");
        let mut owner = crate::guest_memory::OwnedMappings::default();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        assert_eq!(
            vasprintf_service(&mut memory, [OUT, 0x1100, 0x1200], None, SP, RET).unwrap(),
            5
        );
        let allocation = memory.load(OUT, 8).unwrap();
        assert_eq!(memory.read(allocation, 6).unwrap(), b"13/ok\0");
        assert_eq!(memory.load(0x1200, 4).unwrap(), 16);
        assert_eq!(memory.load(SP, 8).unwrap(), RET);
        memory.free(allocation).unwrap();
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn hexadecimal_service_uses_protected_pool_and_guards_publication() {
        let mut data = bytes(b"%a\0");
        data[0x330..0x338].copy_from_slice(&1.5f64.to_bits().to_le_bytes());
        let mut allocations = crate::guest_memory::OwnedMappings::default();
        let mut store = crate::guest_gdtoa::Store::new(GDTOA_POOL).unwrap();
        let (pool, state) = store.split_mut();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.attach_owned(&mut allocations).unwrap();
        memory.add_runtime_data(GDTOA_POOL, pool, state.runtime_owner()).unwrap();
        let mut retained = crate::guest_gdtoa::Retained::default();
        let mut mxcsr = 0x1f80;
        let result = vasprintf_service_with_hex(&mut memory, [OUT, 0x1100, 0x1200], None, SP, RET,
            Some(crate::printf_render::HexContext { state, retained: &mut retained, mxcsr: &mut mxcsr })).unwrap();
        assert_eq!(result, 8);
        assert_eq!(mxcsr, 0x1f80);
        assert!(retained.current().is_none());
        let output = memory.load(OUT, 8).unwrap();
        assert_eq!(memory.read(output, 9).unwrap(), b"0x1.8p+0\0");
        memory.free(output).unwrap();
        assert!(memory.store(GDTOA_POOL, 0, 1).is_err());
        assert!(vasprintf_service_with_hex(&mut memory, [GDTOA_POOL, 0x1100, 0x1200], None, SP, RET,
            Some(crate::printf_render::HexContext { state, retained: &mut retained, mxcsr: &mut mxcsr })).is_err());
        assert_eq!(memory.load(SP, 8).unwrap(), RET);
        drop(memory);
        assert_eq!(allocations.bytes_allocated(), 0);
    }
    #[test]
    fn initial_allocation_failure_precedes_format_read_and_orders_outputs() {
        let mut data = bytes(b"\0");
        let mut memory = Memory::default(); // No attached allocator: real ENOMEM.
        memory.add(0x1000, &mut data, true, false).unwrap();
        assert_eq!(
            vasprintf_service(
                &mut memory,
                [OUT, u64::MAX, u64::MAX],
                Some(OUT + 4),
                SP,
                RET
            )
            .unwrap(),
            u32::MAX as u64
        );
        assert_eq!(memory.load(OUT, 8).unwrap(), 12u64 << 32);
        assert_eq!(memory.load(SP, 8).unwrap(), RET);
    }
    #[test]
    fn wide_encoding_failure_publishes_null_and_eilseq_after_consuming_argument() {
        let mut data = bytes(b"%lc\0");
        data[0x300..0x308].copy_from_slice(&256u64.to_le_bytes());
        let mut owner = crate::guest_memory::OwnedMappings::default();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        assert_eq!(
            vasprintf_service(&mut memory, [OUT, 0x1100, 0x1200], Some(OUT + 8), SP, RET).unwrap(),
            u32::MAX as u64
        );
        assert_eq!(memory.load(OUT, 8).unwrap(), 0);
        assert_eq!(memory.load(OUT + 8, 4).unwrap(), 86);
        assert_eq!(memory.load(0x1200, 4).unwrap(), 8);
        assert_eq!(memory.load(SP, 8).unwrap(), RET);
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn unsupported_format_frees_private_buffer_without_fabricating_errno() {
        let mut data = bytes(b"%f\0");
        let mut owner = crate::guest_memory::OwnedMappings::default();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        assert!(
            vasprintf_service(&mut memory, [OUT, 0x1100, 0x1200], None, SP, RET)
                .unwrap_err()
                .contains("floating")
        );
        assert_eq!(memory.load(OUT, 8).unwrap(), 0xaabbcc);
        assert_eq!(memory.load(0x1204, 4).unwrap(), 48);
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn output_or_count_write_cannot_replace_live_return_address() {
        let mut data = bytes(b"%n\0");
        data[0x300..0x308].copy_from_slice(&SP.to_le_bytes());
        let mut owner = crate::guest_memory::OwnedMappings::default();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.attach_owned(&mut owner).unwrap();
        assert!(vasprintf_service(&mut memory, [SP, 0x1100, 0x1200], None, SP, RET).is_err());
        assert_eq!(memory.load(0x1200, 4).unwrap(), 0);
        assert!(vasprintf_service(&mut memory, [OUT, 0x1100, 0x1200], None, SP, RET).is_err());
        assert_eq!(memory.load(0x1200, 4).unwrap(), 8); // Successful argument read is not rolled back.
        assert_eq!(memory.load(SP, 8).unwrap(), RET);
        drop(memory);
        assert_eq!(owner.bytes_allocated(), 0);
    }
}
#[cfg(test)]
fn vasprintf_service(memory: &mut Memory<'_>, arguments: [u64; 3], errno: Option<u64>, sp: u64, ret: u64) -> Result<u64> {
    vasprintf_service_with_hex(memory, arguments, errno, sp, ret, None)
}
fn vasprintf_service_with_hex(

    memory: &mut Memory<'_>,
    arguments: [u64; 3],
    errno: Option<u64>,
    sp: u64,
    ret: u64,
    hex: Option<crate::printf_render::HexContext<'_>>,
) -> Result<u64> {
    let [strp, format, va] = arguments;
    let guard = |memory: &Memory<'_>, address, length| {
        validate_net_output(memory, address, length, sp, ret)
    };
    let result = crate::printf_buffer::run(memory, strp, 65536, guard, |buffer| {
        let options = crate::printf_render::Options { errno_address: errno, ..Default::default() };
        if let Some(hex) = hex {
            crate::printf_render::render_with_hdtoa(buffer, format, va, options, hex, guard)
        } else {
            crate::printf_render::render(buffer, format, va, options, guard)
        }

    });
    match result {
        Ok(length) => Ok(length as u32 as u64),
        Err(error) => {
            if error.cleanup.is_none() && error.output_cleanup.is_none() {
                if let crate::printf_buffer::Kind::Allocation(code)
                | crate::printf_buffer::Kind::Libc(code) = error.kind {
                    let errno =
                        errno.ok_or("Missing vasprintf failure errno context")?;
                    let value =
                        crate::printf_buffer::publish_failure(memory, strp, errno, code, guard)?;
                    return Ok(value as u32 as u64);
                }
            }
            Err(format!("Guest vasprintf fault: {error:?}"))
        }
    }
}
fn strdup_service(
    memory: &mut Memory<'_>,
    source: u64,
    errno: Option<u64>,
    sp: u64,
    ret: u64,
) -> Result<u64> {
    match crate::libc_memory::strdup(memory, source)? {
        Ok(address) => Ok(address),
        Err(error) => {
            let errno = errno.ok_or("Missing strdup errno context")?;
            validate_net_output(memory, errno, 4, sp, ret)?;
            memory.validate_write(errno, 4)?;
            memory.store(errno, error.into(), 4)?;
            Ok(0)
        }
    }
}
fn multibyte_service(
    memory: &mut Memory<'_>,
    plan: crate::guest_multibyte::Plan,
    errno: Option<u64>,
    sp: u64,
    ret: u64,
) -> Result<u64> {
    for (address, length) in plan.write_ranges() {
        validate_net_output(memory, address, length, sp, ret)?;
    }
    plan.validate(memory)?;
    let error_output = if let Some(error) = plan.errno {
        let address = errno.ok_or("Missing multibyte errno context")?;
        validate_net_output(memory, address, 4, sp, ret)?;
        memory.validate_write(address, 4)?;
        Some((address, error))
    } else {
        None
    };
    plan.commit(memory)?;
    if let Some((address, error)) = error_output {
        memory.store(address, u64::from(error), 4)?;
    }
    Ok(plan.value)
}
fn callback_stack_slot(
    sp: u64,
    stack_address: u64,
    array: u64,
    length: usize,
    key: Option<u64>,
) -> Result<u64> {
    let callback_sp = (sp.checked_sub(144).ok_or("Comparator stack underflow")? & !15)
        .checked_sub(8)
        .ok_or("Comparator alignment underflow")?;
    let low = callback_sp
        .checked_sub(128)
        .filter(|&low| low >= stack_address)
        .ok_or("Comparator red zone exceeds owned stack")?;
    let array_end = array
        .checked_add(length as u64)
        .ok_or("Comparator array overflow")?;
    if (array < sp && low < array_end) || key.is_some_and(|key| key >= low && key < sp) {
        return Err("Comparator entry/red zone overlaps array or key storage".into());
    }
    Ok(callback_sp)
}
fn callback_storage(
    memory: &Memory<'_>,
    address: u64,
    length: usize,
    images: &[(u64, usize)],
    stacks: &[StackIdentity],
) -> Result<(u64, usize)> {
    memory.read(address, length)?;
    let end = address
        .checked_add(length as u64)
        .ok_or("Comparator backing overflow")?;
    if let Some((base, size)) = memory.owned_region_range(address) {
        if end <= base + size as u64 {
            return Ok((base, size));
        }
    }
    if let Some(&(base, size)) = images
        .iter()
        .find(|&&(base, size)| address >= base && end <= base + size as u64)
    {
        return Ok((base, size));
    }
    let backing = memory.read(address, length)?.as_ptr() as usize;
    if let Some(stack) = stacks.iter().find(|s| {
        s.lease.strong_count() != 0
            && address >= s.address
            && end <= s.address + s.length as u64
            && backing == s.backing + (address - s.address) as usize
    }) {
        return Ok((stack.address, stack.length));
    }
    Err("Comparator requires owned allocation, retained stack or retained image backing".into())
}
/// Conditions may suspend with slots in their own retained continuation stack,
/// process-owned allocations, or host-retained images. A foreign stack identity
/// lease does not own its Vec backing, so accepting it would allow another host
/// context to drop the bytes while this wait remains live. Shared stack backing
/// ownership is required before foreign-stack condition/mutex slots are supported.
fn condition_storage(
    memory: &Memory<'_>,
    address: u64,
    images: &[(u64, usize)],
    stacks: &[StackIdentity],
    current_stack: &Rc<()>,
) -> Result<(u64, usize)> {
    let identity = Rc::downgrade(current_stack);
    let own_stack = stacks.iter().find(|s| Weak::ptr_eq(&s.lease, &identity));
    callback_storage(memory, address, 8, images, own_stack.map(std::slice::from_ref).unwrap_or_default())
        .map_err(|error| format!("Condition storage requires owned allocation, retained image or the waiting thread's own stack; foreign-stack storage unsupported: {error}"))
}
fn guard_semaphore_plan(
    memory: &Memory<'_>,
    plan: &crate::guest_semaphore::Plan,
    value_output: Option<u64>,
    errno: u64,
    sp: u64,
    ret: u64,
) -> Result<()> {
    use crate::guest_semaphore::Outcome;
    if memory.load(sp, 8)? != ret {
        return Err("Semaphore return frame changed".into());
    }
    if let Some((address, length)) = plan.write_range() {
        validate_net_output(memory, address, length, sp, ret)?;
        memory.validate_write(address, length)?;
    }
    let output = match plan.outcome() {
        Outcome::Errno(_) => Some(errno),
        Outcome::Value(_) => value_output,
        _ => None,
    };
    if let Some(output) = output {
        validate_net_output(memory, output, 4, sp, ret)?;
        memory.validate_write(output, 4)?;
        if let Some((address, length)) = plan.object_range() {
            if output < address + length as u64 && address < output + 4 {
                return Err("Semaphore output aliases its inline object".into());
            }
        }
    }
    Ok(())
}
fn finish_semaphore_outcome(
    memory: &mut Memory<'_>,
    outcome: crate::guest_semaphore::Outcome,
    output: Option<u64>,
    errno: u64,
) -> Result<Option<u64>> {
    use crate::guest_semaphore::Outcome;
    match outcome {
        Outcome::Success => Ok(Some(0)),
        Outcome::Value(value) => {
            memory.store(
                output.ok_or("Missing semaphore value output")?,
                value.into(),
                4,
            )?;
            Ok(Some(0))
        }
        Outcome::Errno(error) => {
            memory.store(errno, error.into(), 4)?;
            Ok(Some(u32::MAX as u64))
        }
        Outcome::Pending(_) => Ok(None),
    }
}
fn validate_net_output(
    memory: &Memory<'_>,
    output: u64,
    length: usize,
    sp: u64,
    ret: u64,
) -> Result<()> {
    let end = output
        .checked_add(length as u64)
        .ok_or("Network output overflow")?;
    let frame_end = sp.checked_add(8).ok_or("Network return frame overflow")?;
    if memory.load(sp, 8)? != ret || (output < frame_end && sp < end) {
        return Err("Network output overlaps or changes its live return frame".into());
    }
    Ok(())
}
fn validate_video_attribute_return(
    memory: &Memory<'_>,
    output: u64,
    sp: u64,
    ret: u64,
) -> Result<()> {
    if memory.load(sp, 8)? != ret {
        return Err("Video attribute return frame changed before write".into());
    }
    let end = output
        .checked_add(80)
        .ok_or("Video attribute range overflow")?;
    let frame_end = sp.checked_add(8).ok_or("Video return frame overflow")?;
    if output < frame_end && sp < end {
        return Err("Video attribute overlaps its live return frame".into());
    }
    Ok(())
}
fn video_stack_arguments(memory: &Memory<'_>, sp: u64) -> Result<(u64, u64)> {
    Ok((
        memory.load(sp.checked_add(8).ok_or("Video argument stack overflow")?, 8)?,
        memory.load(
            sp.checked_add(16).ok_or("Video argument stack overflow")?,
            8,
        )?,
    ))
}
/// Check the live return frame before a queue can publish output or consume an
/// event. The generic post-service check is too late for failure atomicity.
fn validate_equeue_return(
    memory: &Memory<'_>,
    outputs: (u64, i32, u64),
    frame: (u64, u64),
    terminal: bool,
    registered_return: bool,
) -> Result<()> {
    let (events, maximum, count) = outputs;
    let (sp, ret) = frame;
    if memory.load(sp, 8)? != ret {
        return Err("Event wait return frame changed before delivery".into());
    }
    if (terminal && ret != u64::MAX)
        || (!terminal && (!registered_return || !memory.executable(ret)))
    {
        return Err("Invalid event wait return target before delivery".into());
    }
    if !(1..=crate::equeue::MAX_EVENTS as i32).contains(&maximum) {
        return Err("Invalid event queue capacity".into());
    }
    let frame_end = sp
        .checked_add(8)
        .ok_or("Event wait return frame overflow")?;
    for (start, length) in [
        (events, maximum as u64 * crate::equeue::EVENT_SIZE as u64),
        (count, 4),
    ] {
        let end = start
            .checked_add(length)
            .ok_or("Event output range overflow")?;
        if start < frame_end && sp < end {
            return Err("Event wait outputs overlap its live return frame".into());
        }
    }
    Ok(())
}
pub const MEMORY_FIXTURE_CODE: &[u8] = &[
    0x55, 0x48, 0x89, 0xe5, 0x48, 0x89, 0x37, 0x48, 0x8b, 0x0f, 0x31, 0xc0, 0x5d, 0xc3,
];

#[cfg(test)]
mod equeue_return_tests {
    use super::*;
    #[test]
    fn consuming_bad_resume_retains_all_pending_stack_backings() {
        fn pending(native: &mut NativeExecutor, address: u64, thread: u64) -> Continuation {
            let mut stack = vec![0; 4096];
            let mut memory = Memory::default();
            memory.add(address, &mut stack, true, false).unwrap();
            let p = native.semaphores.init(&memory, address, 0, 0).unwrap();
            native.semaphores.commit(&mut memory, p).unwrap();
            let p = native
                .semaphores
                .wait(&memory, address, thread, 0, || panic!("untimed"))
                .unwrap();
            assert!(matches!(
                native.semaphores.commit(&mut memory, p).unwrap(),
                crate::guest_semaphore::Outcome::Pending(_)
            ));
            drop(memory);
            let stack_lease = Rc::new(());
            native
                .register_stack(address, &stack, &stack_lease)
                .unwrap();
            Continuation {
                callbacks: None,
                stack_lease,
                quantum: None,
                return_convention: ReturnConvention::WorkerPointer,
                guest_thread: thread,
                cpu: Cpu::default(),
                stack,
                stack_address: address,
                remaining: 10,
                caller_bias: 0x100000,
            }
        }
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let first = pending(&mut native, 0x900000, 1);
        let second = pending(&mut native, 0x901000, 2);
        let pointers = [
            first.stack.as_ptr() as usize,
            second.stack.as_ptr() as usize,
        ];
        let mut wrong = Image::load(&memory_fixture(), 0x200000, 65536).unwrap();
        assert!(
            native
                .resume_with_data(&mut wrong, &mut [], first, 0, &mut [])
                .err()
                .unwrap()
                .contains("different mappings")
        );
        assert!(
            native
                .resume_with_data(&mut wrong, &mut [], second, 0, &mut [])
                .err()
                .unwrap()
                .contains("fault pinned")
        );
        assert_eq!(native.semaphores.pending_count(), 2);
        let held = native.fault_stack.borrow();
        assert_eq!(held.len(), 2);
        for (i, (_, bytes, _)) in held.iter().enumerate() {
            assert_eq!(bytes.as_ptr() as usize, pointers[i]);
            assert_eq!(&bytes[..4], &crate::guest_semaphore::MAGIC.to_le_bytes());
        }
    }
    #[test]
    fn pending_semaphore_provenance_tracks_live_backing_not_equal_bytes() {
        let mut owned = crate::guest_memory::OwnedMappings::default();
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let lease = Rc::new(());
        let mut real = vec![0; 64];
        native.register_stack(0x1000, &real, &lease).unwrap();
        let mut copy = real.clone();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut copy, true, false).unwrap();
        assert!(!semaphore_storage_owned(&memory, 0x1010, &[], &native.stack_identities).unwrap());
        drop(memory);
        let mut memory = Memory::default();
        memory.add(0x1000, &mut real, true, false).unwrap();
        assert!(semaphore_storage_owned(&memory, 0x1010, &[], &native.stack_identities).unwrap());
        drop(lease);
        assert!(!semaphore_storage_owned(&memory, 0x1010, &[], &native.stack_identities).unwrap());
        memory.attach_owned(&mut owned).unwrap();
        let heap = memory.malloc(32).unwrap();
        assert!(
            semaphore_storage_owned(&memory, heap + 16, &[], &native.stack_identities).unwrap()
        );
        assert!(!memory.owned_allocation_contains(heap + 17, 16));
        drop(memory);
    }
    #[test]
    fn pending_stack_error_retains_actual_bytes_but_suspension_transfers_them() {
        let fault = Rc::new(RefCell::new(Vec::new()));
        let pending = Rc::new(Cell::new(false));
        {
            let mut stack = ExecutionStack {
                condition_transfer: false,
                lease: Rc::new(()),
                bytes: vec![0; 64],
                address: 0x9000,
                pending: pending.clone(),
                fault: fault.clone(),
            };
            stack[17] = 0xab;
            pending.set(true); // a guest wait committed while this stack was mapped
        }
        let (address, bytes, lease) = fault.borrow_mut().pop().unwrap();
        assert_eq!(address, 0x9000);
        assert_eq!(bytes[17], 0xab);
        let mut stack = ExecutionStack {
            condition_transfer: false,
            lease,
            bytes,
            address,
            pending,
            fault: fault.clone(),
        };
        let transferred = stack.take();
        drop(stack);
        assert!(fault.borrow().is_empty());
        assert_eq!(transferred[17], 0xab);
    }
    #[test]
    fn semaphore_output_guards_precede_state_and_errno_commit() {
        use crate::guest_semaphore::{GuestSemaphore, Outcome};
        let mut bytes = [0; 128];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.store(0x1060, 0x4000, 8).unwrap();
        let mut semaphores = GuestSemaphore::default();
        let init = semaphores.init(&memory, 0x1000, 0, 0).unwrap();
        semaphores.commit(&mut memory, init).unwrap();
        let plan = semaphores.getvalue(&memory, 0x1000).unwrap();
        assert!(
            guard_semaphore_plan(&memory, &plan, Some(0x100d), 0x1050, 0x1060, 0x4000).is_err()
        );
        let plan = semaphores.trywait(&memory, 0x1000).unwrap();
        assert!(matches!(plan.outcome(), Outcome::Errno(35)));
        assert!(guard_semaphore_plan(&memory, &plan, None, 0x1060, 0x1060, 0x4000).is_err());
        assert_eq!(memory.load(0x1060, 8).unwrap(), 0x4000);
        assert_eq!(memory.load(0x1004, 4).unwrap(), 0);
    }
    #[test]
    fn network_outputs_cannot_overwrite_or_wrap_return_frame() {
        let mut bytes = [0xa5; 128];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.store(0x1040, 0x4000, 8).unwrap();
        let before = memory.read(0x1000, 128).unwrap().to_vec();
        for (output, length) in [(0x1040, 4), (0x103d, 4), (0x1047, 32), (u64::MAX - 1, 4)] {
            assert!(validate_net_output(&memory, output, length, 0x1040, 0x4000).is_err());
        }
        assert!(validate_net_output(&memory, 0x1000, 4, 0x1040, 0x4001).is_err());
        validate_net_output(&memory, 0x103c, 4, 0x1040, 0x4000).unwrap();
        validate_net_output(&memory, 0x1048, 32, 0x1040, 0x4000).unwrap();
        assert_eq!(memory.read(0x1000, 128).unwrap(), before);
        const {
            assert!(NET_ERRNO >= ERROR_TEXT + 128);
        }
    }
    #[test]
    fn video_stack_arguments_and_attribute_frame_are_guarded() {
        let mut bytes = [0xa5; 256];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.store(0x1080, 0x4000, 8).unwrap();
        memory.store(0x1088, 7, 8).unwrap();
        memory.store(0x1090, 8, 8).unwrap();
        assert_eq!(video_stack_arguments(&memory, 0x1080).unwrap(), (7, 8));
        assert!(video_stack_arguments(&memory, 0x10f0).is_err());
        assert!(video_stack_arguments(&memory, u64::MAX - 8).is_err());
        let before = memory.read(0x1000, 256).unwrap().to_vec();
        for output in [0x1031, 0x1080, 0x1087, u64::MAX - 40] {
            assert!(validate_video_attribute_return(&memory, output, 0x1080, 0x4000).is_err());
        }
        assert!(validate_video_attribute_return(&memory, 0x1000, 0x1080, 0x4001).is_err());
        validate_video_attribute_return(&memory, 0x1030, 0x1080, 0x4000).unwrap();
        validate_video_attribute_return(&memory, 0x1088, 0x1080, 0x4000).unwrap();
        assert_eq!(memory.read(0x1000, 256).unwrap(), before);
    }
    #[test]
    fn invalid_return_or_overlapping_outputs_preserve_real_queued_event() {
        let mut backing = [0xa5; 512];
        let mut executable = [0xc3];
        let mut memory = Memory::default();
        memory.add(0x2000, &mut backing, true, false).unwrap();
        memory.add(0x4000, &mut executable, false, true).unwrap();
        memory.write(0x2110, b"flip\0").unwrap();
        memory.store(0x2080, 0x4000, 8).unwrap();
        let mut queues = crate::equeue::Equeues::default();
        queues.create(&mut memory, 0x2100, 0x2110).unwrap();
        let handle = memory.load(0x2100, 8).unwrap();
        let event = crate::equeue::Event {
            ident: 7,
            filter: -1,
            flags: 0,
            fflags: 0,
            data: 9,
            udata: 0,
        };
        queues.enqueue(handle, event).unwrap();
        let before = memory.read(0x2000, 512).unwrap().to_vec();
        for (events, count, ret, registered) in [
            (0x2080, 0x2040, 0x4000, true),  // event overwrites return
            (0x2061, 0x2040, 0x4000, true),  // one-byte event overlap
            (0x2000, 0x207d, 0x4000, true),  // one-byte count overlap
            (0x2000, 0x2084, 0x4000, true),  // count inside return
            (0x2000, 0x2040, 0x4001, true),  // changed return word
            (0x2000, 0x2040, 0x4000, false), // executable but unregistered
        ] {
            let outcome = validate_equeue_return(
                &memory,
                (events, 1, count),
                (0x2080, ret),
                false,
                registered,
            )
            .and_then(|()| queues.wait(&mut memory, handle, events, 1, count, 0));
            assert!(outcome.is_err());
            assert_eq!(queues.pending_count(handle).unwrap(), 1);
            assert_eq!(memory.read(0x2000, 512).unwrap(), before);
        }
        memory.store(0x2080, 0x2000, 8).unwrap();
        assert!(
            validate_equeue_return(&memory, (0x2000, 1, 0x2040), (0x2080, 0x2000), false, true)
                .is_err()
        );
        assert_eq!(queues.pending_count(handle).unwrap(), 1);
        memory.store(0x2080, 0x4000, 8).unwrap();
        // Adjacent outputs are allowed, and a genuine registered return permits
        // exactly one delivery after every precommit check succeeds.
        validate_equeue_return(&memory, (0x2060, 1, 0x2088), (0x2080, 0x4000), false, true)
            .unwrap();
        assert_eq!(
            queues
                .wait(&mut memory, handle, 0x2060, 1, 0x2088, 0)
                .unwrap(),
            crate::equeue::WaitOutcome::Ready(0)
        );
        assert_eq!(memory.load(0x2080, 8).unwrap(), 0x4000);
        assert_eq!(memory.read(0x2060, 32).unwrap(), event.encode());
        assert_eq!(queues.pending_count(handle).unwrap(), 0);
        memory.store(0x2080, u64::MAX, 8).unwrap();
        validate_equeue_return(
            &memory,
            (0x2000, 1, 0x2040),
            (0x2080, u64::MAX),
            true,
            false,
        )
        .unwrap();
    }
}
pub const LOOP_FIXTURE_BLOCKS: &[(u64, &[u8], &str)] = &[
    (
        0x100,
        &[0x48, 0x89, 0xf1, 0x31, 0xc0, 0x48, 0x85, 0xc9, 0x74, 0x16],
        "rust_module_loop_entry",
    ),
    (
        0x10a,
        &[0x83, 0xc0, 0x01, 0x48, 0x83, 0xe9, 0x01, 0x75, 0xf7],
        "rust_module_loop_body",
    ),
    (
        0x113,
        &[0x48, 0x89, 0x07, 0x31, 0xc0, 0xc3],
        "rust_module_loop_exit",
    ),
    (
        0x120,
        &[0x48, 0x89, 0x07, 0x31, 0xc0, 0xc3],
        "rust_module_loop_zero",
    ),
];
pub const SHIFT_FIXTURE_CODE: &[u8] = &[
    0x48, 0x89, 0xf0, // mov rax,rsi
    0x48, 0xc1, 0xe0, 0x07, // shl rax,7
    0x48, 0xc1, 0xc8, 0x05, // ror rax,5
    0x48, 0x89, 0x07, // mov [rdi],rax
    0x31, 0xc0, 0xc3,
];
pub const VECTOR_FIXTURE_CODE: &[u8] = &[
    0xc5, 0xfc, 0x10, 0x06, // vmovups ymm0,[rsi]
    0xc5, 0xfc, 0x57, 0x46, 0x20, // vxorps ymm0,ymm0,[rsi+32]
    0xc5, 0xfc, 0x11, 0x07, // vmovups [rdi],ymm0
    0x31, 0xc0, 0xc3,
];
pub const SERVICE_CALL_CODE: &[u8] = &[
    0xba, 3, 0, 0, 0, // mov edx,3
    0x48, 0xb8, 0, 0, 0x70, 0, 0, 0, 0, 0, // mov rax,0x700000
    0xff, 0xd0, // call rax
];
pub const SERVICE_RETURN_CODE: &[u8] = &[0x48, 0x89, 0x47, 0x40, 0x31, 0xc0, 0xc3];
pub const IMPORT_CALL_CODE: &[u8] = &[0xba, 3, 0, 0, 0, 0xff, 0x15, 0x75, 0x04, 0, 0];
/// A real undefined strcpy symbol and RELA-patched call slot, not a fixed thunk.
pub fn imported_service_fixture() -> Vec<u8> {
    let mut file = memory_fixture();
    file.resize(0x900, 0);
    fn put(file: &mut [u8], p: usize, value: u64) {
        file[p..p + 8].copy_from_slice(&value.to_le_bytes());
    }
    file[56..58].copy_from_slice(&3u16.to_le_bytes());
    for (p, value) in [(96, 0x500), (104, 0x500), (152, 0x400), (160, 0x400)] {
        put(&mut file, p, value);
    }
    for (i, value) in [0x400000002, 0x300, 0x300, 0, 160, 160, 8]
        .into_iter()
        .enumerate()
    {
        put(&mut file, 176 + i * 8, value);
    }
    let strings = b"\0strcpy\0libstrings.prx\0";
    file[0x450..0x450 + strings.len()].copy_from_slice(strings);
    for (i, (tag, value)) in [
        (6, 0x400),
        (11, 24),
        (5, 0x450),
        (10, strings.len() as u64),
        (7, 0x480),
        (8, 24),
        (9, 24),
        (1, 8),
        (4, 0x4c0),
        (0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        put(&mut file, 0x300 + i * 16, tag);
        put(&mut file, 0x308 + i * 16, value);
    }
    file[0x418..0x41c].copy_from_slice(&1u32.to_le_bytes());
    file[0x41c] = 0x12;
    for (i, value) in [1u32, 2, 1, 0, 0].into_iter().enumerate() {
        file[0x4c0 + i * 4..0x4c4 + i * 4].copy_from_slice(&value.to_le_bytes());
    }
    put(&mut file, 0x480, 0x580);
    put(&mut file, 0x488, (1 << 32) | 7);
    file[0x100..0x10b].copy_from_slice(IMPORT_CALL_CODE);
    file[0x10b..0x112].copy_from_slice(SERVICE_RETURN_CODE);
    file[0x520..0x527].copy_from_slice(b"linked\0");
    file
}
pub fn memory_fixture() -> Vec<u8> {
    let mut file = crate::modules::fixture();
    file.resize(0x600, 0);
    file[56..58].copy_from_slice(&2u16.to_le_bytes());
    for (i, value) in [0x600000001u64, 0x500, 0x500, 0, 0x100, 0x100, 1]
        .into_iter()
        .enumerate()
    {
        file[120 + i * 8..128 + i * 8].copy_from_slice(&value.to_le_bytes());
    }
    file[0x100..0x100 + MEMORY_FIXTURE_CODE.len()].copy_from_slice(MEMORY_FIXTURE_CODE);
    file
}
struct Block {
    start: u64,
    bytes: Rc<[u8]>,
    function: unsafe extern "C" fn(*mut Cpu),
    _library: Option<Rc<crate::module_compiler::Library>>,
}
/// Explicit and opt-in automatic rebuilds share a finite attempt allowance.
pub const MAX_TRANSLATION_REBUILDS: usize = 16;
const MAX_TRANSLATION_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const MAX_TRANSLATION_SOURCES: usize = 256;
struct TranslationSource {
    file: Rc<[u8]>,
    roots: std::collections::BTreeSet<u64>,
    instruction_limit: usize,
    rebuilds: usize,
    archive: std::path::PathBuf,
    directory: std::path::PathBuf,
}
#[derive(Clone, Copy)]
pub enum Service {
    Getenv,
    Getcwd,
    Mkdir,
    Setenv,
    Wcslen,
    Wcstombs,
    Mbrtowc,
    Wcrtomb,
    Mbsrtowcs,
    Qsort,
    Bsearch,
    Wcsstr,
    Wcscmp,
    Wcscoll,
    Wcsncmp,
    Strlcpy,
    Strlcat,
    Strchr,
    Strrchr,
    Strstr,
    Atoi,
    Strtol,
    Strtoul,
    Strtoll,
    Strtoull,
    Atof,
    Strtod,
    Strtof,
    Strtold,
    Vsscanf,
    Gettimeofday,
    File(crate::stdio_file::Operation),
    Fgets,
    Fstat,
    Stat,
    Sysconf,
    Sysctl,
    SysctlByName,
    Sigaction,
    Signal,
    Nanosleep,
    SemInit,
    SemDestroy,
    SemPost,
    SemGetValue,
    SemTryWait,
    SemWait,
    SemTimedWait,
    Malloc,
    Realloc,
    Calloc,
    Free,
    Strerror,
    InternalStrerror,
    Getargc,
    Getargv,
    SystemHideSplashScreen,
    SystemLaunchWebBrowser,
    UserInitialize,
    UserGetForeground,
    UserGetLoginList,
    UserGetName,
    PadInitialize,
    PadOpen,
    PadClose,
    PadReadState,
    PadSetLightBar,
    PadSetVibration,
    PadSetVibrationMode,
    AudioInitialize,
    AudioOpen,
    AudioOutput,
    AudioClose,
    DirectAllocate,
    DirectMap,
    DirectRelease,
    EqueueCreate,
    EqueueDelete,
    EqueueWait,
    VideoOpen,
    VideoClose,
    VideoAddFlipEvent,
    VideoDeleteFlipEvent,
    VideoSetFlipRate,
    VideoSetBufferAttribute,
    VideoRegisterBuffers,
    VideoSubmitFlip,
    KeyboardInitialize,
    KeyboardOpen,
    KeyboardReadState,
    KeyboardClose,
    ImeInitialize,
    ImeGetStatus,
    ImeGetResult,
    NetErrnoLoc,
    NetPoolCreate,
    NetPoolDestroy,
    NetResolverCreate,
    NetResolverDestroy,
    NetResolverStartAton,
    NetResolverStartNtoa,
    ImeTerminate,
    Sprintf,
    Snprintf,
    Vsnprintf,
    Vasprintf,
    Errno,
    Getpid,
    Syscall,
    Dlsym,
    LoadStartModule,
    StopUnloadModule,
    SysmoduleLoadInternal,
    Memcpy,
    Memmove,
    Memcmp,
    Strdup,
    Memset,
    Setlocale,
    Puts,
    Fprintf,
    Fflush,
    ThreadAttrInit,
    ThreadAttrDestroy,
    ThreadAttrSetDetachState,
    ThreadAttrSetStackSize,
    ThreadCreate,
    ThreadSelf,
    ThreadSetName,
    Sigemptyset,
    Sigaddset,
    ThreadSigmask,
    ThreadSetCancelType,
    ThreadGetschedparam,
    ThreadSetschedparam,
    SchedPriorityMin,
    SchedPriorityMax,
    ThreadJoin,
    ThreadDetach,
    CondInit,
    CondDestroy,
    CondSignal,
    CondBroadcast,
    CondWait,
    CondTimedWait,
    MutexAttrInit,
    MutexAttrSetType,
    MutexAttrDestroy,
    MutexInit,
    MutexDestroy,
    MutexLock,
    MutexTryLock,
    MutexUnlock,
    TlsKeyCreate,
    TlsKeyDelete,
    TlsGetSpecific,
    TlsSetSpecific,
    Exit,
    ImmediateExit,
    Abort,
    Strcpy,
    Strcat,
    Strcmp,
    Strcoll,
    Strxfrm,
    Strftime,
    Strncmp,
    Strlen,
    Ctype(crate::c_locale::Ctype),
    Strncasecmp,
}
impl Service {
    fn operation(self) -> u64 {
        match self {
            Self::Getenv
            | Self::Memcpy
            | Self::Memmove
            | Self::Memcmp
            | Self::Ctype(_)
            | Self::Strncasecmp
            | Self::Strdup
            | Self::Memset
            | Self::Setlocale
            | Self::Puts
            | Self::Fprintf
            | Self::Fflush
            | Self::ThreadAttrInit
            | Self::ThreadAttrDestroy
            | Self::ThreadAttrSetDetachState
            | Self::ThreadAttrSetStackSize
            | Self::ThreadCreate
            | Self::ThreadSelf
            | Self::ThreadSetName
            | Self::Sigemptyset
            | Self::Sigaddset
            | Self::Getcwd
            | Self::Mkdir
            | Self::Setenv
            | Self::Wcslen
            | Self::Wcstombs
            | Self::Mbrtowc
            | Self::Wcrtomb
            | Self::Mbsrtowcs
            | Self::Qsort
            | Self::Bsearch
            | Self::Wcsstr
            | Self::Wcscmp
            | Self::Wcscoll
            | Self::Wcsncmp
            | Self::Strlcpy
            | Self::Strlcat
            | Self::Strchr
            | Self::Strrchr
            | Self::Strstr
            | Self::Atoi
            | Self::Strtol
            | Self::Strtoul
            | Self::Strtoll
            | Self::Strtoull
            | Self::Atof
            | Self::Strtod
            | Self::Strtof
            | Self::Strtold
            | Self::Vsscanf
            | Self::Gettimeofday
            | Self::File(_)
            | Self::Fgets
            | Self::Fstat
            | Self::Stat
            | Self::Sysconf
            | Self::Sysctl
            | Self::SysctlByName
            | Self::Sigaction
            | Self::Signal
            | Self::Nanosleep
            | Self::SemInit
            | Self::SemDestroy
            | Self::SemPost
            | Self::SemGetValue
            | Self::SemTryWait
            | Self::SemWait
            | Self::SemTimedWait
            | Self::ThreadSigmask
            | Self::ThreadSetCancelType
            | Self::ThreadGetschedparam
            | Self::ThreadSetschedparam
            | Self::SchedPriorityMin
            | Self::SchedPriorityMax
            | Self::ThreadJoin
            | Self::ThreadDetach
            | Self::CondInit
            | Self::CondDestroy
            | Self::CondSignal
            | Self::CondBroadcast
            | Self::CondWait
            | Self::CondTimedWait
            | Self::MutexAttrInit
            | Self::MutexAttrSetType
            | Self::MutexAttrDestroy
            | Self::MutexInit
            | Self::MutexDestroy
            | Self::MutexLock
            | Self::MutexTryLock
            | Self::MutexUnlock
            | Self::TlsKeyCreate
            | Self::TlsKeyDelete
            | Self::TlsGetSpecific
            | Self::TlsSetSpecific
            | Self::Exit
            | Self::ImmediateExit
            | Self::Abort
            | Self::Malloc
            | Self::Realloc
            | Self::Calloc
            | Self::Free
            | Self::Strerror
            | Self::InternalStrerror
            | Self::Getargc
            | Self::Getargv
            | Self::SystemHideSplashScreen
            | Self::SystemLaunchWebBrowser
            | Self::UserInitialize
            | Self::UserGetForeground
            | Self::UserGetLoginList
            | Self::UserGetName
            | Self::PadInitialize
            | Self::PadOpen
            | Self::PadClose
            | Self::PadReadState
            | Self::PadSetLightBar
            | Self::PadSetVibration
            | Self::PadSetVibrationMode
            | Self::AudioInitialize
            | Self::AudioOpen
            | Self::AudioOutput
            | Self::AudioClose
            | Self::DirectAllocate
            | Self::DirectMap
            | Self::DirectRelease
            | Self::EqueueCreate
            | Self::EqueueDelete
            | Self::EqueueWait
            | Self::VideoOpen
            | Self::VideoClose
            | Self::VideoAddFlipEvent
            | Self::VideoDeleteFlipEvent
            | Self::VideoSetFlipRate
            | Self::VideoSetBufferAttribute
            | Self::VideoRegisterBuffers
            | Self::VideoSubmitFlip
            | Self::KeyboardInitialize
            | Self::KeyboardOpen
            | Self::KeyboardReadState
            | Self::KeyboardClose
            | Self::ImeInitialize
            | Self::ImeGetStatus
            | Self::ImeGetResult
            | Self::NetErrnoLoc
            | Self::NetPoolCreate
            | Self::NetPoolDestroy
            | Self::NetResolverCreate
            | Self::NetResolverDestroy
            | Self::NetResolverStartAton
            | Self::NetResolverStartNtoa
            | Self::ImeTerminate
            | Self::Sprintf
            | Self::Snprintf
            | Self::Vsnprintf
            | Self::Vasprintf
            | Self::Strcoll
            | Self::Strxfrm
            | Self::Strftime
            | Self::Errno
            | Self::Getpid
            | Self::Syscall
            | Self::Dlsym
            | Self::LoadStartModule
            | Self::StopUnloadModule => {
                unreachable!("loader services yield to the runtime")
            }
            Self::SysmoduleLoadInternal => unreachable!("sysmodule service yields to the runtime"),
            Self::Strcpy => 0,
            Self::Strcat => 1,
            Self::Strcmp => 2,
            Self::Strncmp => 3,
            Self::Strlen => 4,
        }
    }
}
/// Owned state at a host-service boundary; contains no borrowed guest mappings.
#[derive(Clone, Copy)]
enum ReturnConvention {
    ModuleStatus,
    WorkerPointer,
}
impl ReturnConvention {
    fn completed(self, value: u64) -> Execution {
        match self {
            Self::ModuleStatus => Execution::Returned(value as u32 as i32),
            Self::WorkerPointer => Execution::WorkerReturned(value),
        }
    }
}
pub struct Continuation {
    callbacks: Option<crate::guest_callback::Context>,
    stack_lease: Rc<()>,
    quantum: Option<u64>,
    return_convention: ReturnConvention,
    guest_thread: u64,
    cpu: Cpu,
    stack: Vec<u8>,
    stack_address: u64,
    remaining: u64,
    caller_bias: u64,
}
impl Drop for Continuation {
    fn drop(&mut self) {
        if let Some(context) = &self.callbacks {
            if !context.abort_frames.is_empty() && !self.stack.is_empty() {
                context.retain_abort_stack(
                    self.stack_address,
                    std::mem::take(&mut self.stack),
                    self.stack_lease.clone(),
                );
            }
        }
    }
}
impl Continuation {
    pub fn guest_thread_id(&self) -> u64 {
        self.guest_thread
    }
    pub fn stack_region(&mut self) -> DataRegion<'_> {
        DataRegion {
            address: self.stack_address,
            bytes: &mut self.stack,
            writable: true,
        }
    }
    /// Temporarily map suspended storage for host service argument/result access.
    /// The callback cannot retain mapped references beyond this call. The runtime
    /// must keep the caller/dependency images alive until the continuation ends.
    pub fn with_memory<T>(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_memory_data(image, dependencies, &mut [], operation)
    }
    pub fn with_memory_data<T>(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_memory_context(image, dependencies, data, None, operation)
    }
    fn with_memory_context<T>(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        allocations: Option<&mut crate::guest_memory::OwnedMappings>,
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        if image.bias != self.caller_bias {
            return Err("Continuation belongs to different caller".into());
        }
        let mut memory = Memory::default();
        for mapped in std::iter::once(image).chain(dependencies.iter_mut().map(|i| &mut **i)) {
            for segment in &mut mapped.segments {
                memory.add(
                    segment.address,
                    &mut segment.bytes,
                    segment.flags & 2 != 0,
                    segment.flags & 1 != 0,
                )?;
            }
        }
        memory.add(self.stack_address, &mut self.stack, true, false)?;
        for region in data {
            memory.add(region.address, region.bytes, region.writable, false)?;
        }
        if let Some(owner) = allocations {
            memory.attach_owned(owner)?;
        }
        if let Some(callbacks) = &self.callbacks {
            memory.set_write_barriers(callbacks.registry.barriers());
        }
        operation(&mut memory)
    }
}
/// Validated input for the supported zero-flags, null-options loader subset.
/// Addresses stay guest addresses; the runtime must map caller storage while
/// executing the new module's start routine and writing its optional result.
pub struct LoadRequest {
    pub path: String,
    pub arguments: Arguments,
    pub result_address: u64,
}
impl LoadRequest {
    pub fn read(memory: &Memory<'_>, arguments: [u64; 6]) -> Result<Self> {
        let [path, size, pointer, flags, options, result_address] = arguments;
        if path == 0 {
            return Err("Null module path".into());
        }
        if flags != 0 || options != 0 {
            return Err("Module load flags/options are unsupported".into());
        }
        let mut bytes = Vec::new();
        for offset in 0..=1024u64 {
            let byte =
                memory.load(path.checked_add(offset).ok_or("Module path overflow")?, 1)? as u8;
            if byte == 0 {
                let path = String::from_utf8(bytes).map_err(|_| "Invalid module path encoding")?;
                if !crate::module_files::valid_path(&path) {
                    return Err("Invalid guest module path".into());
                }
                if size != 0 {
                    memory.read(
                        pointer,
                        usize::try_from(size).map_err(|_| "Module argument size overflow")?,
                    )?;
                }
                if result_address != 0 {
                    memory.validate_write(result_address, 4)?;
                }
                return Ok(Self {
                    path,
                    arguments: Arguments { size, pointer },
                    result_address,
                });
            }
            bytes.push(byte);
        }
        Err("Module path exceeds 1024 bytes".into())
    }
}
/// A termination request, not proof of libc cleanup or host resource teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    /// exit(): atexit callbacks and stdio finalization remain pending.
    Normal,
    /// _exit(): bypass libc callbacks; host resource teardown remains pending.
    Immediate,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitRequest {
    pub status: i32,
    pub kind: ExitKind,
}
/// An unacquired lock request. Dropping it cancels the request; it never unlocks
/// an owned mutex. Its continuation cannot be passed to generic service resume.
pub struct MutexWait {
    owner: Rc<()>,
    continuation: Option<Continuation>,
    ticket: crate::guest_mutex::LockTicket,
    ret: u64,
    terminal: bool,
}
impl MutexWait {
    pub(crate) fn stack_region(&mut self) -> Result<DataRegion<'_>> {
        Ok(self
            .continuation
            .as_mut()
            .ok_or("Mutex wait already consumed")?
            .stack_region())
    }
    pub fn guest_thread_id(&self) -> u64 {
        self.ticket.thread_id()
    }
    pub(crate) fn pending_owner(&self, native: &NativeExecutor) -> Option<u64> {
        native.mutex.pending_owner(self.ticket)
    }
    pub fn slot_address(&self) -> u64 {
        self.ticket.slot_address()
    }
    pub fn is_consumed(&self) -> bool {
        self.continuation.is_none()
    }
}
/// One-shot scheduling boundary; never accepts a manufactured service result.
pub struct QuantumYield {
    owner: Rc<()>,
    continuation: Option<Continuation>,
}
impl QuantumYield {
    pub fn guest_thread_id(&self) -> Option<u64> {
        self.continuation.as_ref().map(|c| c.guest_thread)
    }
    pub fn is_consumed(&self) -> bool {
        self.continuation.is_none()
    }
    pub fn stack_region(&mut self) -> Result<DataRegion<'_>> {
        Ok(self
            .continuation
            .as_mut()
            .ok_or("Quantum already consumed")?
            .stack_region())
    }
}
#[derive(Clone, Debug)]
pub enum ThreadOperation {
    ConditionWait {
        ticket: crate::guest_cond::WaitTicket,
        mutex_ticket: Rc<Option<crate::guest_mutex::CondMutexTicket>>,
        return_sp: u64,
        return_address: u64,
    },
    SemaphoreWait {
        ticket: crate::guest_semaphore::WaitTicket,
        return_sp: u64,
        return_address: u64,
        errno: u64,
    },
    Sleep {
        deadline: crate::guest_sleep::Deadline,
    },
    Completed(u64),
    Create {
        output: u64,
        entry: u64,
        argument: u64,
        options: crate::guest_thread_attr::ThreadOptions,
    },
    Join {
        thread: u64,
        output: u64,
    },
    Detach {
        thread: u64,
    },
}
/// Runtime inheritance policy: preserve the x87 environment and register stack
/// together. Exact PS5 kernel inheritance remains to be independently verified.
#[derive(Clone)]
pub struct X87Environment {
    control: u16,
    status: u16,
    stack: crate::x87_state::Stack,
}
pub struct ThreadRequest {
    owner: Rc<()>,
    continuation: Option<Continuation>,
    operation: ThreadOperation,
    condition_faults: Option<Rc<RefCell<Vec<Continuation>>>>,
}
impl Drop for ThreadRequest {
    fn drop(&mut self) {
        if let Some(faults) = &self.condition_faults {
            if let Some(continuation) = self.continuation.take() {
                faults.borrow_mut().push(continuation);
            }
        }
    }
}
impl ThreadRequest {
    pub(crate) fn set_quantum(&mut self, quantum: u64) -> Result<()> {
        self.continuation
            .as_mut()
            .ok_or("Thread request consumed")?
            .quantum = Some(quantum);
        Ok(())
    }
    pub(crate) fn belongs_to(&self, native: &NativeExecutor) -> bool {
        Rc::ptr_eq(&self.owner, &native.mutex_wait_owner)
    }
    pub(crate) fn complete(&mut self, result: u64) {
        self.operation = ThreadOperation::Completed(result);
    }
    pub(crate) fn stack_region(&mut self) -> Result<DataRegion<'_>> {
        Ok(self
            .continuation
            .as_mut()
            .ok_or("Thread request consumed")?
            .stack_region())
    }
    pub fn operation(&self) -> ThreadOperation {
        self.operation.clone()
    }
    pub fn guest_thread_id(&self) -> Option<u64> {
        self.continuation.as_ref().map(|c| c.guest_thread)
    }
    pub fn mxcsr(&self) -> Result<u32> {
        Ok(self
            .continuation
            .as_ref()
            .ok_or("Thread request consumed")?
            .cpu
            .mxcsr)
    }
    pub fn x87_environment(&self) -> Result<X87Environment> {
        let cpu = &self
            .continuation
            .as_ref()
            .ok_or("Thread request already consumed")?
            .cpu;
        Ok(X87Environment {
            control: cpu.x87_control,
            status: cpu.x87_status,
            stack: cpu.x87.clone(),
        })
    }
}
pub enum Execution {
    SignalTerminated {
        signal: i32,
    },
    ThreadRequest(Box<ThreadRequest>),
    QuantumYield(Box<QuantumYield>),
    /// Full-width pointer return from an explicitly invoked worker entry.
    WorkerReturned(u64),
    MutexWait(Box<MutexWait>),
    ExitRequested(ExitRequest),
    SysmoduleLoadInternal {
        arguments: [u64; 6],
        continuation: Box<Continuation>,
    },
    Dlsym {
        arguments: [u64; 6],
        continuation: Box<Continuation>,
    },
    Returned(i32),
    StopUnloadModule {
        arguments: [u64; 6],
        continuation: Box<Continuation>,
    },
    LoadStartModule {
        /// SysV arguments: path, size, args, flags, options, result pointer.
        arguments: [u64; 6],
        continuation: Box<Continuation>,
    },
}
impl Execution {
    /// Lower a suspended context's lifetime instruction allowance. Never grants
    /// extra fuel and never changes CPU registers or execution position.
    pub fn cap_instruction_fuel(&mut self, limit: u64) {
        let continuation = match self {
            Self::ThreadRequest(request) => request.continuation.as_mut(),
            Self::QuantumYield(wait) => wait.continuation.as_mut(),
            Self::MutexWait(wait) => wait.continuation.as_mut(),
            Self::Dlsym { continuation, .. }
            | Self::SysmoduleLoadInternal { continuation, .. }
            | Self::StopUnloadModule { continuation, .. }
            | Self::LoadStartModule { continuation, .. } => Some(&mut **continuation),
            _ => None,
        };
        if let Some(state) = continuation {
            state.cpu.fuel = state.cpu.fuel.min(limit);
        }
    }
    /// Borrow the owned stack of suspended execution while another worker runs.
    /// The scheduler must map every live stack to preserve shared stack arguments.
    pub fn stack_region(&mut self) -> Result<Option<DataRegion<'_>>> {
        match self {
            Self::ThreadRequest(request) => Ok(Some(
                request
                    .continuation
                    .as_mut()
                    .ok_or("Thread request consumed")?
                    .stack_region(),
            )),
            Self::QuantumYield(wait) => wait.stack_region().map(Some),
            Self::MutexWait(wait) => Ok(Some(
                wait.continuation
                    .as_mut()
                    .ok_or("Mutex wait already consumed")?
                    .stack_region(),
            )),
            Self::Dlsym { continuation, .. }
            | Self::SysmoduleLoadInternal { continuation, .. }
            | Self::StopUnloadModule { continuation, .. }
            | Self::LoadStartModule { continuation, .. } => Ok(Some(continuation.stack_region())),
            Self::WorkerReturned(_)
            | Self::Returned(_)
            | Self::ExitRequested(_)
            | Self::SignalTerminated { .. } => Ok(None),
        }
    }
}
enum Flow {
    Finished(Execution),
    EnvironmentSet {
        continuation: Box<Continuation>,
        imported: Option<crate::environment::Environment>,
        name: Vec<u8>,
        value: Vec<u8>,
        overwrite: i32,
        errno: u64,
        terminal: bool,
    },
    TranslationMiss {
        bias: u64,
        target: u64,
        continuation: Box<Continuation>,
    },
}
type EnvironmentCompletion = Option<(u64, Option<i32>, bool)>;
pub struct NativeExecutor {
    callback_ranges: crate::guest_callback::Registry,
    callback_faults: crate::guest_callback::Faults,
    abort_faults: crate::guest_callback::AbortFaults,
    signal_termination: Option<i32>,
    system_profile: Option<crate::guest_system::GuestSystemProfile>,
    system_profile_locked: bool,
    signal_actions: crate::guest_signal_action::Store,
    consumed_instruction_fuel: u128,
    worker_quantum: Option<u64>,
    mutex_wait_owner: Rc<()>,
    pub thread_attributes: crate::guest_thread_attr::GuestThreadAttributes,
    pub mutex: crate::guest_mutex::GuestMutex,
    pub conditions: crate::guest_cond::GuestCond,
    condition_leases: Vec<(u64, Rc<()>)>,
    condition_transfer_active: bool,
    condition_faults: Rc<RefCell<Vec<Continuation>>>,
    pub tls: crate::guest_tls::GuestTls,
    selected_guest_thread: u64,
    exit_request: Option<ExitRequest>,
    // Kept owned for future cleanup and diagnostics; never an ordinary resumable call.
    exit_context: Option<Box<Continuation>>,
    fault_stack: FaultStack,
    stack_identities: Vec<StackIdentity>,
    pub user_service: crate::user_service::UserService,
    pub pad: crate::pad::Pad,
    pub keyboard: crate::keyboard::Keyboard,
    pub ime_dialog: crate::ime_dialog::ImeDialog,
    /// Host DNS is disabled initially. Configure before creating live resources.
    pub network: crate::net_resolver::Resolver<crate::net_dns::DnsBackend>,
    network_errno: [u8; 4],
    pub locale: crate::guest_locale::GuestLocale,
    pub guest_time: crate::guest_time::GuestTime,
    gdtoa: crate::guest_gdtoa::Store,
    pub semaphores: crate::guest_semaphore::GuestSemaphore,
    sleep_clock: crate::guest_sleep::Clock,
    pub stdout: crate::captured_stdout::CapturedStdout,
    pub stdio: crate::guest_stdio::GuestStdio,
    file_streams: crate::stdio_file::Store,
    file_metadata: Vec<u8>,
    stdin_metadata: Option<[u8; crate::stdio_file::SLOT_SIZE]>,
    stdin_pointer: [u8; 8],
    stderr_file: [u8; 312],
    stderr_pointer: [u8; 8],
    locale_name: [u8; 2],
    pub audio: crate::audio_out::AudioOut<crate::audio_queue::AudioQueueBackend>,
    pub direct_memory: crate::direct_memory::DirectMemory,
    pub equeues: crate::equeue::Equeues,
    pub video_out: crate::video_out::VideoOut<crate::video_out::MetalBackend>,
    pub system_service: crate::system_service::SystemService,
    /// Explicit guest argc/argv; no host command-line data is inherited.
    pub process_arguments: Option<(u32, u64)>,
    /// Guest NULL-terminated environment table; zero explicitly means empty.
    pub environment: u64,
    owned_environment: Option<crate::environment::Environment>,
    pub environment_budget: usize,
    error_text: [u8; 128],
    allocations: crate::guest_memory::OwnedMappings,
    pub kernel: Option<crate::kernel::Kernel>,
    blocks: BTreeMap<u64, Block>,
    image_blocks: BTreeMap<(u64, u64), Block>,
    translation_sources: BTreeMap<u64, TranslationSource>,
    translation_source_bytes: usize,
    automatic_translation: bool,
    services: BTreeMap<u64, Service>,
    pub fuel: u64,
    pub transitions: u64,
    stack_address: u64,
    stack_size: usize,
}
impl NativeExecutor {
    fn register_stack(&mut self, address: u64, bytes: &[u8], lease: &Rc<()>) -> Result<()> {
        self.stack_identities
            .retain(|s| s.lease.strong_count() != 0);
        if let Some(existing) = self
            .stack_identities
            .iter()
            .find(|s| Weak::ptr_eq(&s.lease, &Rc::downgrade(lease)))
        {
            if existing.address != address
                || existing.length != bytes.len()
                || existing.backing != bytes.as_ptr() as usize
            {
                return Err("Retained stack backing changed".into());
            }
            return Ok(());
        }
        if self.stack_identities.len() >= 128 {
            return Err("Retained stack identity limit".into());
        }
        self.stack_identities
            .try_reserve(1)
            .map_err(|_| "Cannot reserve stack identity")?;
        self.stack_identities.push(StackIdentity {
            address,
            length: bytes.len(),
            backing: bytes.as_ptr() as usize,
            lease: Rc::downgrade(lease),
        });
        Ok(())
    }
    fn protect_stack(&self, address: u64, bytes: Vec<u8>, lease: Rc<()>) -> ExecutionStack {
        let pending = Rc::new(Cell::new(retained_overlap(
            &self.semaphores,
            &self.callback_ranges,
            address,
            bytes.len(),
        )));
        ExecutionStack {
            condition_transfer: self.condition_transfer_active,
            lease,
            bytes,
            address,
            pending,
            fault: self.fault_stack.clone(),
        }
    }
    pub fn semaphore_realtime(&self) -> Result<crate::guest_semaphore::Realtime> {
        let (seconds, nanoseconds) = self.guest_time.realtime_parts()?;
        Ok(crate::guest_semaphore::Realtime {
            seconds,
            nanoseconds,
        })
    }
    /// Retry a retained semaphore service without executing or charging another
    /// guest instruction. Guard failures retain both ticket and continuation.
    pub fn resume_semaphore_request(
        &mut self,
        request: &mut ThreadRequest,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
    ) -> Result<Option<u64>> {
        self.resume_semaphore_at(request, image, dependencies, data, None)
    }
    pub(crate) fn resume_semaphore_at(
        &mut self,
        request: &mut ThreadRequest,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        sampled: Option<crate::guest_semaphore::Realtime>,
    ) -> Result<Option<u64>> {
        self.require_running()?;
        let thread = request
            .guest_thread_id()
            .ok_or("Consumed semaphore request")?;
        if !request.belongs_to(self)
            || thread != self.selected_guest_thread
            || !self.tls.contains_thread(thread)
        {
            return Err("Foreign or retired semaphore request".into());
        }
        let ThreadOperation::SemaphoreWait {
            ticket,
            return_sp,
            return_address,
            errno,
        } = request.operation()
        else {
            return Err("Not a pending semaphore request".into());
        };
        let now = if self.semaphores.needs_realtime(&ticket, thread)? {
            Some(match sampled {
                Some(now) => now,
                None => self.semaphore_realtime()?,
            })
        } else {
            None
        };
        if !self.semaphores.ready(&ticket, thread, now)? {
            return Ok(None);
        }
        let saved = request
            .continuation
            .as_ref()
            .ok_or("Consumed semaphore request")?;
        if saved.caller_bias != image.bias {
            return Err("Semaphore request belongs to different image".into());
        }
        let terminal = saved.cpu.stop == 3;
        let target = if terminal {
            if return_address != u64::MAX
                || !(saved
                    .callbacks
                    .as_ref()
                    .is_some_and(|context| context.authentic_return(return_sp, return_address))
                    || (return_sp == saved.stack_address + saved.stack.len() as u64 - 8
                        && saved
                            .callbacks
                            .as_ref()
                            .is_none_or(|context| context.is_empty())))
                || saved.cpu.registers[4] != return_sp
            {
                return Err("Semaphore terminal return changed".into());
            }
            None
        } else {
            if saved.cpu.bias.checked_add(saved.cpu.next_rip) != Some(return_address)
                || return_sp.checked_add(8) != Some(saved.cpu.registers[4])
            {
                return Err("Semaphore return target changed".into());
            }
            let block = self
                .image_blocks
                .get(&(saved.cpu.bias, saved.cpu.next_rip))
                .or_else(|| self.blocks.get(&saved.cpu.next_rip))
                .ok_or("Semaphore return target no longer registered")?;
            Some((
                saved
                    .cpu
                    .bias
                    .checked_add(block.start)
                    .ok_or("Semaphore return code overflow")?,
                block.bytes.clone(),
            ))
        };
        let mut semaphores = std::mem::take(&mut self.semaphores);
        let result =
            self.with_thread_request_memory(request, image, dependencies, data, |memory| {
                if let Some((source, bytes)) = &target {
                    if !memory.executable(return_address)
                        || memory.read(*source, bytes.len())? != &**bytes
                    {
                        return Err("Semaphore return code changed before acquisition".into());
                    }
                }
                let plan = semaphores.resume(memory, &ticket, thread, now)?;
                guard_semaphore_plan(memory, &plan, None, errno, return_sp, return_address)?;
                let outcome = semaphores.commit(memory, plan)?;
                finish_semaphore_outcome(memory, outcome, None, errno)
            });
        self.semaphores = semaphores;
        if let Ok(Some(value)) = result {
            request.complete(value);
        }
        result
    }
    pub(crate) fn condition_ready(
        &self,
        request: &ThreadRequest,
        now: crate::guest_cond::Realtime,
    ) -> Result<bool> {
        if !request.belongs_to(self) {
            return Err("Foreign condition request".into());
        }
        let ThreadOperation::ConditionWait {
            ticket,
            mutex_ticket,
            ..
        } = &request.operation
        else {
            return Err("Not a condition wait".into());
        };
        let thread = request
            .guest_thread_id()
            .ok_or("Consumed condition request")?;
        match self.conditions.phase(ticket, thread)? {
            crate::guest_cond::Phase::Waiting => {
                Ok(self.conditions.delay_ns(ticket, thread, now)? == Some(0))
            }
            crate::guest_cond::Phase::Reacquiring(_) => Ok(self
                .mutex
                .cond_owner_hint(
                    mutex_ticket
                        .as_ref()
                        .as_ref()
                        .ok_or("Unpublished condition mutex token")?,
                )?
                .is_none()),
        }
    }
    pub(crate) fn resume_condition_at(
        &mut self,
        request: &mut ThreadRequest,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        sampled: Option<crate::guest_cond::Realtime>,
    ) -> Result<Option<u64>> {
        self.require_running()?;
        let thread = request
            .guest_thread_id()
            .ok_or("Consumed condition request")?;
        if !request.belongs_to(self)
            || thread != self.selected_guest_thread
            || !self.tls.contains_thread(thread)
        {
            return Err("Foreign or retired condition request".into());
        }
        let ThreadOperation::ConditionWait {
            ticket,
            return_sp,
            return_address,
            mutex_ticket,
        } = request.operation()
        else {
            return Err("Not a pending condition request".into());
        };
        let needs_realtime = matches!(
            self.conditions.phase(&ticket, thread)?,
            crate::guest_cond::Phase::Waiting
        ) && self
            .conditions
            .delay_ns(
                &ticket,
                thread,
                crate::guest_cond::Realtime {
                    seconds: 0,
                    nanoseconds: 0,
                },
            )?
            .is_some();
        let now = if needs_realtime {
            Some(match sampled {
                Some(now) => now,
                None => {
                    let (seconds, nanoseconds) = self.guest_time.realtime_parts()?;
                    crate::guest_cond::Realtime {
                        seconds,
                        nanoseconds,
                    }
                }
            })
        } else {
            None
        };
        let saved = request
            .continuation
            .as_ref()
            .ok_or("Consumed condition request")?;
        if saved.caller_bias != image.bias {
            return Err("Condition request belongs to different image".into());
        }
        let terminal = saved.cpu.stop == 3;
        let target = if terminal {
            if return_address != u64::MAX
                || !(saved
                    .callbacks
                    .as_ref()
                    .is_some_and(|context| context.authentic_return(return_sp, return_address))
                    || (return_sp == saved.stack_address + saved.stack.len() as u64 - 8
                        && saved
                            .callbacks
                            .as_ref()
                            .is_none_or(|context| context.is_empty())))
                || saved.cpu.registers[4] != return_sp
            {
                return Err("Condition terminal return changed".into());
            }
            None
        } else {
            if saved.cpu.bias.checked_add(saved.cpu.next_rip) != Some(return_address)
                || return_sp.checked_add(8) != Some(saved.cpu.registers[4])
            {
                return Err("Condition return target changed".into());
            }
            let block = self
                .image_blocks
                .get(&(saved.cpu.bias, saved.cpu.next_rip))
                .or_else(|| self.blocks.get(&saved.cpu.next_rip))
                .ok_or("Condition return target no longer registered")?;
            Some((
                saved
                    .cpu
                    .bias
                    .checked_add(block.start)
                    .ok_or("Condition return code overflow")?,
                block.bytes.clone(),
            ))
        };
        let mut conditions = std::mem::take(&mut self.conditions);
        let result = self.with_suspended_runtime(
            request
                .continuation
                .as_mut()
                .ok_or("Consumed condition request")?,
            image,
            dependencies,
            data,
            |memory, mutex| {
                if memory.load(return_sp, 8)? != return_address {
                    return Err("Condition return frame changed before reacquisition".into());
                }
                if let Some((source, bytes)) = &target {
                    if !memory.executable(return_address)
                        || memory.read(*source, bytes.len())? != &**bytes
                    {
                        return Err("Condition return code changed before reacquisition".into());
                    }
                }
                let select = conditions.select(memory, &ticket, thread, now)?;
                if let crate::guest_cond::Outcome::Pending(_) = conditions.commit(memory, select)? {
                    return Ok(None);
                }
                let finish = conditions.finish(memory, &ticket, thread)?;
                let mutex_ticket = mutex_ticket
                    .as_ref()
                    .as_ref()
                    .ok_or("Unpublished condition mutex token")?;
                let Some(reacquire) = mutex.prepare_cond_reacquire(memory, mutex_ticket)? else {
                    return Ok(None);
                };
                let outcome = conditions.commit(memory, finish)?;
                reacquire.commit();
                match outcome {
                    crate::guest_cond::Outcome::Status(status) => Ok(Some(status as u32 as u64)),
                    _ => unreachable!("Condition finish returns its selected status"),
                }
            },
        );
        self.conditions = conditions;
        if let Ok(Some(value)) = result {
            request.complete(value);
            self.condition_leases.retain(|(owner, _)| *owner != thread);
        }
        result
    }
    pub fn configure_manual_sleep_clock(&mut self, now: u128) -> Result<()> {
        self.sleep_clock.configure_manual(now)
    }
    pub fn advance_sleep_clock(&mut self, now: u128) -> Result<()> {
        self.sleep_clock.advance_manual(now)
    }
    pub fn sleep_clock_now(&self) -> u128 {
        self.sleep_clock.now()
    }
    pub(crate) fn with_thread_request_memory<T>(
        &mut self,
        request: &mut ThreadRequest,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        if !Rc::ptr_eq(&request.owner, &self.mutex_wait_owner) {
            return Err("Foreign thread request".into());
        }
        let continuation = request
            .continuation
            .as_mut()
            .ok_or("Thread request consumed")?;
        self.with_suspended_runtime(continuation, image, dependencies, data, |memory, _| {
            operation(memory)
        })
    }
    pub(crate) fn finish_thread_request(
        &mut self,
        request: &mut ThreadRequest,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        result: u64,
    ) -> Result<Execution> {
        self.require_running()?;
        if !Rc::ptr_eq(&request.owner, &self.mutex_wait_owner)
            || request.guest_thread_id() != Some(self.selected_guest_thread)
            || !self.tls.contains_thread(self.selected_guest_thread)
        {
            return Err("Foreign or retired thread request".into());
        }
        let saved = request
            .continuation
            .as_ref()
            .ok_or("Thread request consumed")?;
        self.validate_resume_mapping(saved, image, saved.stack.len())?;
        let condition_completion = request.condition_faults.is_some();
        if condition_completion {
            if !matches!(request.operation, ThreadOperation::Completed(_)) {
                return Err("Condition wait has not reacquired its mutex".into());
            }
            // Validate every supplied mapping while ownership stays in request.
            self.with_thread_request_memory(request, image, dependencies, data, |_| Ok(()))?;
            self.fault_stack
                .borrow_mut()
                .try_reserve(1)
                .map_err(|_| "Cannot reserve condition transfer fault backing")?;
        }
        let continuation = request
            .continuation
            .take()
            .ok_or("Thread request consumed")?;
        self.condition_transfer_active = condition_completion;
        let outcome = self.resume_with_data(image, dependencies, continuation, result, data);
        self.condition_transfer_active = false;
        outcome
    }
    fn validate_resume_mapping(
        &self,
        continuation: &Continuation,
        image: &Image,
        stack_length: usize,
    ) -> Result<()> {
        if continuation.caller_bias != image.bias
            || continuation.stack_address < self.stack_address
            || stack_length != self.stack_size
            || (continuation.stack_address - self.stack_address) % self.stack_size as u64 != 0
        {
            return Err("Continuation belongs to different mappings".into());
        }
        Ok(())
    }
    /// Reserve and validate a child before publishing its guest handle. No child
    /// instruction executes here. The scheduler registers TLS after preparation.
    pub(crate) fn prepare_worker(
        &mut self,
        image: &Image,
        dependencies: &[&mut Image],
        setup: (u64, u64, u64, u32, u64, X87Environment),
        data: &mut [DataRegion<'_>],
    ) -> Result<QuantumYield> {
        let (thread, entry, argument, mxcsr, stack_size, x87) = setup;
        if stack_size != self.stack_size as u64 {
            return Err("Requested thread stack size is unsupported by configured executor".into());
        }
        let owner = std::iter::once(image)
            .chain(dependencies.iter().map(|i| &**i))
            .find(|mapped| {
                mapped.segments.iter().any(|segment| {
                    segment.flags & 1 != 0
                        && entry >= segment.address
                        && entry - segment.address < (segment.bytes.len() as u64)
                })
            })
            .ok_or("Worker entry is not executable")?;
        if !self
            .image_blocks
            .contains_key(&(owner.bias, entry - owner.bias))
            && !self.blocks.contains_key(&(entry - owner.bias))
        {
            let source = self
                .translation_sources
                .get(&owner.bias)
                .ok_or("Worker entry has no translation owner")?;
            let archive = source.archive.clone();
            let directory = source.directory.clone();
            // SAFETY: the source archive inherits compile_image's trusted contract.
            unsafe {
                self.extend_image_translation(owner, entry, &archive, &directory)?;
            }
        }
        let mut address = self.stack_address;
        let mut selected = None;
        for _ in 0..128 {
            let end = address
                .checked_add(stack_size)
                .ok_or("Worker stack overflow")?;
            let data_overlap = data.iter().any(|region| {
                address < region.address.saturating_add(region.bytes.len() as u64)
                    && region.address < end
            });
            let image_overlap = std::iter::once(image)
                .chain(dependencies.iter().map(|i| &**i))
                .any(|mapped| {
                    mapped.segments.iter().any(|segment| {
                        address < segment.address.saturating_add(segment.bytes.len() as u64)
                            && segment.address < end
                    })
                });
            if !data_overlap
                && !image_overlap
                && !self.callback_ranges.overlaps(address, self.stack_size)
            {
                selected = Some(address);
                break;
            }
            address = end;
        }
        let address = selected.ok_or("Worker stack slots exhausted")?;
        let mut stack = Vec::new();
        stack
            .try_reserve_exact(self.stack_size)
            .map_err(|_| "Cannot reserve worker stack")?;
        stack.resize(self.stack_size, 0);
        stack[self.stack_size - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
        let mut cpu = Cpu {
            bias: owner.bias,
            rip: entry - owner.bias,
            fuel: self.fuel,
            mxcsr,
            x87_control: x87.control,
            x87_status: x87.status,
            x87: x87.stack,
            ..Cpu::default()
        };
        cpu.registers[4] = address + stack_size - 8;
        cpu.registers[7] = argument;
        let stack_lease = Rc::new(());
        self.register_stack(address, &stack, &stack_lease)?;
        Ok(QuantumYield {
            owner: self.mutex_wait_owner.clone(),
            continuation: Some(Continuation {
                callbacks: None,
                stack_lease,
                quantum: self.worker_quantum,
                return_convention: ReturnConvention::WorkerPointer,
                guest_thread: thread,
                cpu,
                stack,
                stack_address: address,
                remaining: self.transitions,
                caller_bias: image.bias,
            }),
        })
    }
    pub(crate) fn execution_identity(&self) -> Rc<()> {
        self.mutex_wait_owner.clone()
    }
    /// Monotonic count of translated instructions and charged service dispatches.
    pub fn consumed_instruction_fuel(&self) -> u128 {
        self.consumed_instruction_fuel
    }
    /// Bound future worker slices; lifetime instruction/transition budgets remain
    /// independent. Module entries retain their existing synchronous behavior.
    pub fn set_worker_quantum(&mut self, quantum: Option<u64>) -> Result<()> {
        if quantum == Some(0) {
            return Err("Worker quantum must be positive".into());
        }
        self.worker_quantum = quantum;
        Ok(())
    }
    pub fn resume_quantum(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        wait: &mut QuantumYield,
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        self.require_running()?;
        let state = wait
            .continuation
            .as_ref()
            .ok_or("Quantum already consumed")?;
        if !Rc::ptr_eq(&wait.owner, &self.mutex_wait_owner)
            || state.guest_thread != self.selected_guest_thread
            || !self.tls.contains_thread(state.guest_thread)
            || state.caller_bias != image.bias
            || state.stack.len() != self.stack_size
            || state.stack_address < self.stack_address
            || (state.stack_address - self.stack_address) % self.stack_size as u64 != 0
        {
            return Err("Quantum belongs to different executor, thread or mappings".into());
        }
        let state = wait.continuation.take().unwrap();
        self.execute_flow(
            image,
            0,
            Arguments::default(),
            dependencies,
            Some(state),
            data,
        )
    }
    /// Identity used by new guest entries. This is never a host thread ID.
    pub fn guest_thread_id(&self) -> u64 {
        self.selected_guest_thread
    }
    /// Select an explicitly registered guest context for a new entry. This
    /// creates no worker or scheduler. Resumes retain their captured identity.
    pub fn select_guest_thread(&mut self, thread: u64) -> Result<()> {
        self.require_running()?;
        if !self.tls.contains_thread(thread) {
            return Err("Guest TLS thread identity is not registered".into());
        }
        self.selected_guest_thread = thread;
        Ok(())
    }
    /// Once requested, ordinary guest execution cannot resume. This does not
    /// mean callbacks, buffered output, or native resource disposal completed.
    pub fn exit_request(&self) -> Option<ExitRequest> {
        self.exit_request
    }
    pub fn exit_remaining_fuel(&self) -> Option<u64> {
        self.exit_context.as_ref().map(|context| context.cpu.fuel)
    }
    pub fn signal_termination(&self) -> Option<i32> {
        self.signal_termination
    }
    pub(crate) fn require_running(&self) -> Result<()> {
        if let Some(signal) = self.signal_termination {
            return Err(format!(
                "Guest terminated by signal {signal}; teardown remains pending"
            ));
        }
        if !self.fault_stack.borrow().is_empty()
            || !self.callback_faults.borrow().is_empty()
            || !self.condition_faults.borrow().is_empty()
            || !self.abort_faults.borrow().is_empty()
        {
            return Err("Guest stack or callback retirement fault pinned".into());
        }
        if let Some(request) = self.exit_request {
            return Err(format!(
                "Guest exit requested ({:?}, status {}); cleanup remains pending",
                request.kind, request.status
            ));
        }
        Ok(())
    }
    /// Inspect or update a suspended call with this process's owned allocations.
    /// Storage is returned to its owner even when the callback fails.
    pub fn with_suspended_memory<T>(
        &mut self,
        continuation: &mut Continuation,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_suspended_runtime(continuation, image, dependencies, data, |memory, _| {
            operation(memory)
        })
    }
    fn with_suspended_runtime<T>(
        &mut self,
        continuation: &mut Continuation,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        operation: impl FnOnce(&mut Memory<'_>, &mut crate::guest_mutex::GuestMutex) -> Result<T>,
    ) -> Result<T> {
        let reserved: Vec<_> = self.services.keys().copied().collect();
        let mut mapped_data = data
            .iter_mut()
            .map(|region| DataRegion {
                address: region.address,
                bytes: &mut *region.bytes,
                writable: region.writable,
            })
            .collect::<Vec<_>>();
        if self.services.values().any(|service| matches!(service, Service::Vasprintf)) {
            let (pool, _) = self.gdtoa.split_mut();
            mapped_data.push(DataRegion { address: GDTOA_POOL, bytes: pool, writable: false });
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::Strerror | Service::InternalStrerror))
        {
            mapped_data.push(DataRegion {
                address: ERROR_TEXT,
                bytes: &mut self.error_text,
                writable: false,
            });
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::NetErrnoLoc))
        {
            mapped_data.push(DataRegion {
                address: NET_ERRNO,
                bytes: &mut self.network_errno,
                writable: true,
            });
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::Setlocale))
        {
            mapped_data.push(DataRegion {
                address: LOCALE_NAME,
                bytes: &mut self.locale_name,
                writable: false,
            });
        }
        if self.services.values().any(|service| {
            matches!(
                service,
                Service::Fprintf | Service::Fflush | Service::File(_) | Service::Abort
            )
        }) {
            mapped_data.push(DataRegion {
                address: crate::guest_stdio::FILE_ADDR,
                bytes: &mut self.stderr_file,
                writable: true,
            });
            mapped_data.push(DataRegion {
                address: crate::guest_stdio::STDERR_PTR_ADDR,
                bytes: &mut self.stderr_pointer,
                writable: false,
            });
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::File(_) | Service::Fgets | Service::Abort))
        {
            mapped_data.push(DataRegion {
                address: crate::stdio_file::BASE,
                bytes: &mut self.file_metadata,
                writable: true,
            });
        }
        if let Some(metadata) = self.stdin_metadata.as_mut() {
            mapped_data.push(DataRegion {
                address: crate::stdio_file::STDIN_FILE_ADDR,
                bytes: metadata,
                writable: true,
            });
            mapped_data.push(DataRegion {
                address: crate::stdio_file::STDIN_PTR_ADDR,
                bytes: &mut self.stdin_pointer,
                writable: false,
            });
        }
        continuation.with_memory_context(
            image,
            dependencies,
            &mut mapped_data,
            Some(&mut self.allocations),
            |memory| {
                for address in reserved {
                    memory.reserve_service(address);
                }
                operation(memory, &mut self.mutex)
            },
        )
    }
    pub fn bytes_allocated(&self) -> usize {
        self.allocations.bytes_allocated()
    }
    /// Atomically publish the implemented loader thunk under its kernel name/NID.
    /// Exports load/start, stop/unload and dlsym, not a complete libkernel.
    pub fn register_loader_module(
        &mut self,
        modules: &mut crate::modules::Modules,
        name: &str,
        address: u64,
    ) -> Result<u64> {
        let dlsym = address
            .checked_add(32)
            .ok_or("Loader thunk address overflow")?;
        if dlsym == u64::MAX || self.services.contains_key(&dlsym) {
            return Err("Invalid or duplicate resolver thunk".into());
        }
        let stop = address
            .checked_add(16)
            .ok_or("Loader thunk address overflow")?;
        if address == 0
            || stop == u64::MAX
            || self.services.contains_key(&address)
            || self.services.contains_key(&stop)
        {
            return Err("Invalid or duplicate loader thunk".into());
        }
        let handle = modules.register_native(
            name,
            &[
                ("sceKernelLoadStartModule", address),
                ("sceKernelStopUnloadModule", stop),
                ("sceKernelDlsym", dlsym),
            ],
        )?;
        self.services.insert(address, Service::LoadStartModule);
        self.services.insert(stop, Service::StopUnloadModule);
        self.services.insert(dlsym, Service::Dlsym);
        Ok(handle)
    }
    /// Discover and compile supplied ELF code using a matching trusted runtime
    /// archive. Publication is all-or-nothing; existing image entries reject it.
    /// # Safety
    /// The archive must be this exact runtime build for this target. Its helpers
    /// must obey the current Cpu/Memory ABI and contain no untrusted host code.
    pub unsafe fn compile_image(
        &mut self,
        file: &[u8],
        bias: u64,
        roots: std::collections::BTreeSet<u64>,
        limit: usize,
        archive: &std::path::Path,
        directory: &std::path::Path,
    ) -> Result<crate::discover::Manifest> {
        if self
            .image_blocks
            .keys()
            .any(|&(existing, _)| existing == bias)
        {
            return Err("Image translations already registered".into());
        }
        if self.translation_sources.len() >= MAX_TRANSLATION_SOURCES
            || file.len() > MAX_TRANSLATION_SOURCE_BYTES - self.translation_source_bytes
        {
            return Err("Immutable translation source budget exceeded".into());
        }
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(file.len())
            .map_err(|_| "Cannot allocate immutable translation source")?;
        snapshot.extend_from_slice(file);
        let compiled =
            crate::module_compiler::compile(file, roots.clone(), limit, archive, directory)?;
        let archive = archive.canonicalize().map_err(|e| e.to_string())?;
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
        for entry in &compiled.entries {
            bias.checked_add(entry.pc)
                .ok_or("Compiled image address overflow")?;
        }
        for entry in compiled.entries {
            self.image_blocks.insert(
                (bias, entry.pc),
                Block {
                    start: entry.start,
                    bytes: entry.bytes,
                    function: entry.function,
                    _library: Some(compiled.library.clone()),
                },
            );
        }
        self.translation_source_bytes += snapshot.len();
        self.translation_sources.insert(
            bias,
            TranslationSource {
                file: snapshot.into(),
                roots,
                instruction_limit: limit,
                rebuilds: 0,
                archive,
                directory,
            },
        );
        Ok(compiled.manifest)
    }
    /// Opt in to observed-target compilation while this executor is quiescent.
    /// # Safety
    /// Every archive supplied to `compile_image` must remain the exact trusted
    /// runtime build for the entire time this option is enabled. Compilation
    /// directories must remain host-controlled. Guest data must not select them.
    pub unsafe fn set_automatic_translation(&mut self, enabled: bool) {
        self.automatic_translation = enabled;
    }
    pub fn translation_rebuilds(&self, bias: u64) -> Option<usize> {
        self.translation_sources
            .get(&bias)
            .map(|source| source.rebuilds)
    }
    pub fn translation_roots(&self, bias: u64) -> Option<&std::collections::BTreeSet<u64>> {
        self.translation_sources
            .get(&bias)
            .map(|source| &source.roots)
    }
    /// Recompile the immutable original ELF with one observed executable target
    /// added to its root set. The same instruction limit applies to the complete
    /// union. All old translations survive every rejected/failed replacement.
    /// Call only while guest execution is quiescent; this does not resume a miss.
    /// Writable/self-modified executable bytes are not reinterpreted as code.
    /// # Safety
    /// The archive must remain the matching trusted runtime described by
    /// `compile_image`; no generated guest code may be executing during this call.
    pub unsafe fn extend_image_translation(
        &mut self,
        image: &Image,
        target: u64,
        archive: &std::path::Path,
        directory: &std::path::Path,
    ) -> Result<crate::discover::Manifest> {
        let source = self
            .translation_sources
            .get(&image.bias)
            .ok_or("Image has no immutable compilation source")?;
        if source.file.as_ref() != image.file.as_slice() {
            return Err("Translation extension image identity differs".into());
        }
        let pc = target
            .checked_sub(image.bias)
            .ok_or("Translation target below image")?;
        if self.image_blocks.contains_key(&(image.bias, pc)) {
            return Err("Translation target is already registered".into());
        }
        if !image.segments.iter().any(|segment| {
            segment.flags & 1 != 0
                && target >= segment.address
                && target - segment.address < segment.bytes.len() as u64
        }) {
            return Err("Translation target is not executable image memory".into());
        }
        if source.rebuilds >= MAX_TRANSLATION_REBUILDS {
            return Err("Translation rebuild budget exhausted".into());
        }
        let file = source.file.clone();
        let mut roots = source.roots.clone();
        roots.insert(pc);
        let limit = source.instruction_limit;
        // Charge attempts, including unsupported/overlapping code or compiler
        // failures, so repeated misses cannot cause unbounded host compilation.
        self.translation_sources
            .get_mut(&image.bias)
            .unwrap()
            .rebuilds += 1;
        let compiled =
            crate::module_compiler::compile(&file, roots.clone(), limit, archive, directory)?;
        if !compiled.entries.iter().any(|entry| entry.pc == pc) {
            let reason = compiled
                .manifest
                .untranslated
                .iter()
                .find(|failure| failure.pc == pc)
                .map(|failure| failure.reason.as_str())
                .unwrap_or("no supported translation");
            return Err(format!("Observed target {pc:#x} is unsupported: {reason}"));
        }
        let mut replacement = BTreeMap::new();
        for entry in compiled.entries {
            image
                .bias
                .checked_add(entry.pc)
                .ok_or("Compiled image address overflow")?;
            let address = image
                .bias
                .checked_add(entry.start)
                .ok_or("Compiled block address overflow")?;
            let matches = image.segments.iter().any(|segment| {
                if segment.flags & 1 == 0 || address < segment.address {
                    return false;
                }
                let Ok(offset) = usize::try_from(address - segment.address) else {
                    return false;
                };
                offset
                    .checked_add(entry.bytes.len())
                    .and_then(|end| segment.bytes.get(offset..end))
                    == Some(entry.bytes.as_ref())
            });
            if !matches {
                return Err("Live executable bytes differ from immutable ELF translation".into());
            }
            replacement.insert(
                (image.bias, entry.pc),
                Block {
                    start: entry.start,
                    bytes: entry.bytes,
                    function: entry.function,
                    _library: Some(compiled.library.clone()),
                },
            );
        }
        // No fallible work remains. Existing libraries stay pinned until the
        // replacement has been compiled, loaded and validated in its entirety.
        self.image_blocks.retain(|&(bias, _), _| bias != image.bias);
        self.image_blocks.append(&mut replacement);
        self.translation_sources.get_mut(&image.bias).unwrap().roots = roots;
        Ok(compiled.manifest)
    }
    /// Call only after guest execution is quiescent; later calls fail until rebuilt.
    pub fn remove_image_translations(&mut self, bias: u64) {
        self.image_blocks.retain(|&(owner, _), _| owner != bias);
        if let Some(source) = self.translation_sources.remove(&bias) {
            self.translation_source_bytes -= source.file.len();
        }
    }
    /// Publish working string thunks and matching name/NID exports together.
    pub fn register_string_module(
        &mut self,
        modules: &mut crate::modules::Modules,
        name: &str,
        base: u64,
    ) -> Result<u64> {
        let mut services = self.services.clone();
        let mut exports = Vec::new();
        for (i, service) in [
            Service::Strcpy,
            Service::Strcat,
            Service::Strcmp,
            Service::Strncmp,
            Service::Strlen,
        ]
        .into_iter()
        .enumerate()
        {
            let address = base
                .checked_add(i as u64 * 16)
                .ok_or("String thunk address overflow")?;
            if address == 0 || address == u64::MAX || services.contains_key(&address) {
                return Err("String thunk address conflict".into());
            }
            services.insert(address, service);
            exports.push((crate::strings::NAMES[i], address));
        }
        let handle = modules.register_native(name, &exports)?;
        self.services = services;
        Ok(handle)
    }
    /// Configure reported hardware before the first guest execution attempt.
    /// This does not change domain allocation limits or enforce a host RAM budget.
    pub fn configure_system_profile(
        &mut self,
        profile: crate::guest_system::GuestSystemProfile,
    ) -> Result<()> {
        if self.system_profile_locked {
            return Err("Guest system profile is frozen after execution starts".into());
        }
        self.system_profile = Some(profile);
        Ok(())
    }
    /// Bind stdin to an explicitly registered guest file before any execution attempt.
    /// The exported __stdinp cell is read-only; its FILE has real shared fd0 state.
    pub fn configure_stdin(&mut self, path: &[u8]) -> Result<()> {
        if self.system_profile_locked {
            return Err("Guest stdin is frozen after execution starts".into());
        }
        if self.stdin_metadata.is_some() {
            return Err("Guest stdin is already configured".into());
        }
        let metadata = self.file_streams.configure_stdin(
            self.kernel.as_mut().ok_or("Missing stdin kernel context")?,
            path,
        )?;
        self.stdin_metadata = Some(metadata);
        Ok(())
    }
    /// Export this address as the data symbol __stdinp, not as a callable service.
    pub fn stdin_pointer_address(&self) -> Result<u64> {
        if self.stdin_metadata.is_none() {
            return Err("Guest stdin source has not been configured".into());
        }
        Ok(crate::stdio_file::STDIN_PTR_ADDR)
    }
    pub fn system_profile(&self) -> Option<&crate::guest_system::GuestSystemProfile> {
        self.system_profile.as_ref()
    }
    pub fn new(stack_address: u64, stack_size: usize, fuel: u64, transitions: u64) -> Result<Self> {
        if !(4096..=16 * 1024 * 1024).contains(&stack_size)
            || stack_size % 16 != 0
            || stack_address % 16 != 0
            || stack_address.checked_add(stack_size as u64).is_none()
        {
            return Err("Invalid module stack".into());
        }
        let mut tls = crate::guest_tls::GuestTls::default();
        tls.register_thread(MAIN_GUEST_THREAD)?;
        Ok(Self {
            callback_ranges: crate::guest_callback::Registry::default(),
            callback_faults: Rc::new(RefCell::new(Vec::new())),
            abort_faults: Rc::new(RefCell::new(Vec::new())),
            signal_termination: None,
            system_profile: None,
            system_profile_locked: false,
            signal_actions: crate::guest_signal_action::Store::default(),
            consumed_instruction_fuel: 0,
            worker_quantum: None,
            mutex_wait_owner: Rc::new(()),
            thread_attributes: crate::guest_thread_attr::GuestThreadAttributes::default(),
            mutex: crate::guest_mutex::GuestMutex::default(),
            conditions: crate::guest_cond::GuestCond::default(),
            condition_leases: Vec::new(),
            condition_transfer_active: false,
            condition_faults: Rc::new(RefCell::new(Vec::new())),
            tls,
            selected_guest_thread: MAIN_GUEST_THREAD,
            exit_request: None,
            exit_context: None,
            fault_stack: Rc::new(RefCell::new(Vec::new())),
            stack_identities: Vec::new(),
            user_service: crate::user_service::UserService::default(),
            pad: crate::pad::Pad::default(),
            audio: crate::audio_out::AudioOut::new(crate::audio_queue::AudioQueueBackend),
            equeues: crate::equeue::Equeues::default(),
            video_out: crate::video_out::VideoOut::new(crate::video_out::MetalBackend, None),
            keyboard: crate::keyboard::Keyboard::default(),
            ime_dialog: crate::ime_dialog::ImeDialog::default(),
            network: crate::net_resolver::Resolver::new(crate::net_dns::DnsBackend::default()),
            network_errno: [0; 4],
            locale: crate::guest_locale::GuestLocale::default(),
            guest_time: crate::guest_time::GuestTime::default(),
            gdtoa: crate::guest_gdtoa::Store::new(GDTOA_POOL).map_err(|error| format!("Cannot create gdtoa pool: {error:?}"))?,
            semaphores: crate::guest_semaphore::GuestSemaphore::default(),
            sleep_clock: crate::guest_sleep::Clock::default(),
            stdout: crate::captured_stdout::CapturedStdout::default(),
            stdio: crate::guest_stdio::GuestStdio::default(),
            file_streams: crate::stdio_file::Store::default(),
            file_metadata: vec![0; crate::stdio_file::ARENA_SIZE],
            stdin_metadata: None,
            stdin_pointer: crate::stdio_file::stdin_pointer_bytes(),
            stderr_file: crate::guest_stdio::stderr_file_bytes(),
            stderr_pointer: crate::guest_stdio::stderr_pointer_bytes(),
            locale_name: crate::guest_locale::C_NAME,
            direct_memory: crate::direct_memory::DirectMemory::default(),
            system_service: crate::system_service::SystemService::default(),
            environment: 0,
            owned_environment: None,
            environment_budget: 1024 * 1024,
            error_text: [0; 128],
            process_arguments: None,
            allocations: crate::guest_memory::OwnedMappings::default(),
            kernel: None,
            blocks: BTreeMap::new(),
            image_blocks: BTreeMap::new(),
            translation_sources: BTreeMap::new(),
            translation_source_bytes: 0,
            automatic_translation: false,
            services: BTreeMap::new(),
            fuel,
            transitions,
            stack_address,
            stack_size,
        })
    }
    /// Explicit guest thunk address. Thunks must not overlap any mapped guest bytes.
    pub fn register_service(&mut self, address: u64, service: Service) -> Result<()> {
        if address == 0 || address == u64::MAX || self.services.contains_key(&address) {
            return Err("Invalid or duplicate module service".into());
        }
        self.services.insert(address, service);
        Ok(())
    }
    /// # Safety
    /// Function must be ARM64 code emitted for these guest bytes/offset using
    /// translate_rust_memory. It must respect Cpu and the synchronous Memory ABI,
    /// never retain pointers or unwind, and stop at a supported dispatcher boundary.
    pub unsafe fn register(
        &mut self,
        offset: u64,
        bytes: &[u8],
        function: unsafe extern "C" fn(*mut Cpu),
    ) -> Result<()> {
        if bytes.is_empty() || self.blocks.contains_key(&offset) {
            return Err("Invalid translated registration".into());
        }
        self.blocks.insert(
            offset,
            Block {
                start: offset,
                bytes: bytes.into(),
                function,
                _library: None,
            },
        );
        Ok(())
    }
    /// # Safety
    /// Same generated-code contract as register, scoped to one image load bias.
    pub unsafe fn register_in_image(
        &mut self,
        bias: u64,
        offset: u64,
        bytes: &[u8],
        function: unsafe extern "C" fn(*mut Cpu),
    ) -> Result<()> {
        if bytes.is_empty()
            || bias.checked_add(offset).is_none()
            || self.image_blocks.contains_key(&(bias, offset))
            || self.translation_sources.contains_key(&bias)
        {
            return Err("Invalid image translation registration".into());
        }
        self.image_blocks.insert(
            (bias, offset),
            Block {
                start: offset,
                bytes: bytes.into(),
                function,
                _library: None,
            },
        );
        Ok(())
    }
    fn execute(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
    ) -> Result<i32> {
        match self.run(image, address, arguments, dependencies)? {
            Execution::SignalTerminated { signal } => {
                Err(format!("Guest terminated by signal {signal}"))
            }
            Execution::ThreadRequest(_) => {
                Err("Thread request requires scheduler orchestration".into())
            }
            Execution::QuantumYield(_) => {
                Err("Quantum requires resumable worker orchestration".into())
            }
            Execution::MutexWait(_) => {
                Err("Mutex wait requires resumable runtime orchestration".into())
            }
            Execution::WorkerReturned(_) => {
                Err("Unexpected worker return in module status call".into())
            }
            Execution::Returned(value) => Ok(value),
            Execution::ExitRequested(request) => Err(format!(
                "Guest exit requested ({:?}, status {}); cleanup remains pending",
                request.kind, request.status
            )),
            Execution::Dlsym { .. }
            | Execution::SysmoduleLoadInternal { .. }
            | Execution::LoadStartModule { .. }
            | Execution::StopUnloadModule { .. } => {
                Err("Module loader requires resumable runtime orchestration".into())
            }
        }
    }
    /// Run until return or a loader request. All image borrows end on return.
    pub fn run(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
    ) -> Result<Execution> {
        self.run_with_data(image, address, arguments, dependencies, &mut [])
    }
    pub fn run_with_data(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        self.execute_flow(image, address, arguments, dependencies, None, data)
    }
    /// Invoke one worker entry with an opaque RDI argument and full-width RAX
    /// result. This does not create a thread, reserve a scheduler stack, or join.
    /// Caller must retain/map other live stacks and select a registered identity.
    pub fn run_worker(
        &mut self,
        image: &mut Image,
        address: u64,
        argument: u64,
        dependencies: &mut [&mut Image],
    ) -> Result<Execution> {
        self.run_worker_with_data(image, address, argument, dependencies, &mut [])
    }
    pub fn run_worker_with_data(
        &mut self,
        image: &mut Image,
        address: u64,
        argument: u64,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        self.execute_flow_entry(
            image,
            (
                address,
                Arguments {
                    size: argument,
                    pointer: 0,
                },
                ReturnConvention::WorkerPointer,
            ),
            dependencies,
            None,
            data,
        )
    }
    /// Resume after servicing a request, preserving stack, registers and budgets.
    /// The runtime must retain the suspended images and supply their current mappings.
    pub fn resume(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        continuation: Continuation,
        result: u64,
    ) -> Result<Execution> {
        self.resume_with_data(image, dependencies, continuation, result, &mut [])
    }
    pub fn resume_with_data(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        mut continuation: Continuation,
        result: u64,
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        let mut retained = self.protect_stack(
            continuation.stack_address,
            std::mem::take(&mut continuation.stack),
            continuation.stack_lease.clone(),
        );
        self.require_running()?;
        self.validate_resume_mapping(&continuation, image, retained.len())?;
        continuation.cpu.registers[0] = result;
        if continuation.cpu.stop != 3 {
            continuation.cpu.rip = continuation.cpu.next_rip;
            continuation.cpu.stop = 0;
        }
        continuation.stack = retained.take();
        self.execute_flow(
            image,
            0,
            Arguments::default(),
            dependencies,
            Some(continuation),
            data,
        )
    }
    /// Retry a lock without manufacturing a guest return value. Pending and
    /// precommit failures retain the request. Once acquired, the request is
    /// consumed even if subsequent guest execution faults.
    pub fn resume_mutex_with_data(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        wait: &mut MutexWait,
        data: &mut [DataRegion<'_>],
    ) -> Result<Option<Execution>> {
        self.require_running()?;
        if !Rc::ptr_eq(&self.mutex_wait_owner, &wait.owner) {
            return Err("Mutex wait belongs to another executor".into());
        }
        let continuation = wait
            .continuation
            .as_mut()
            .ok_or("Mutex wait already consumed")?;
        if continuation.caller_bias != image.bias
            || continuation.stack_address < self.stack_address
            || continuation.stack.len() != self.stack_size
            || (continuation.stack_address - self.stack_address) % self.stack_size as u64 != 0
            || continuation.guest_thread != wait.ticket.thread_id()
            || !self.tls.contains_thread(continuation.guest_thread)
        {
            return Err("Mutex wait belongs to different mappings or thread".into());
        }
        if continuation.remaining == 0 {
            return Err("Mutex retry budget exhausted".into());
        }
        let cpu = &continuation.cpu;
        let sp = cpu.registers[4];
        let popped = sp.checked_add(8).ok_or("Mutex return stack overflow")?;
        let callback_terminal = continuation
            .callbacks
            .as_ref()
            .is_some_and(|context| context.authentic_return(sp, wait.ret));
        let target = if wait.terminal {
            if wait.ret != u64::MAX
                || (!callback_terminal
                    && (sp != continuation.stack_address + continuation.stack.len() as u64 - 8
                        || continuation
                            .callbacks
                            .as_ref()
                            .is_some_and(|context| !context.is_empty())))
            {
                return Err("Mutex terminal return changed".into());
            }
            None
        } else {
            let block = self
                .image_blocks
                .get(&(cpu.bias, cpu.next_rip))
                .or_else(|| self.blocks.get(&cpu.next_rip))
                .ok_or("Mutex return target no longer registered")?;
            if cpu.bias.checked_add(cpu.next_rip) != Some(wait.ret) {
                return Err("Mutex return target changed".into());
            }
            Some((
                cpu.bias
                    .checked_add(block.start)
                    .ok_or("Mutex code address overflow")?,
                block.bytes.clone(),
            ))
        };
        let ret = wait.ret;
        let terminal = wait.terminal;
        let ticket = wait.ticket;
        let outcome = self.with_suspended_runtime(
            continuation,
            image,
            dependencies,
            data,
            |memory, mutex| {
                if memory.load(sp, 8)? != ret {
                    return Err("Mutex return frame changed".into());
                }
                if terminal && ret != u64::MAX {
                    return Err("Mutex terminal return invalid".into());
                }
                if let Some((source, bytes)) = target {
                    if !memory.executable(ret) || memory.read(source, bytes.len())? != &*bytes {
                        return Err("Mutex return code changed before acquisition".into());
                    }
                }
                mutex.resume_lock(memory, ticket)
            },
        )?;
        continuation.remaining -= 1;
        let value = match outcome {
            crate::guest_mutex::LockOutcome::Pending(ticket) => {
                wait.ticket = ticket;
                return Ok(None);
            }
            crate::guest_mutex::LockOutcome::Acquired => 0,
            crate::guest_mutex::LockOutcome::Status(status) => status,
        };
        let mut continuation = wait.continuation.take().unwrap();
        continuation.cpu.registers[4] = popped;
        continuation.cpu.registers[0] = value as u32 as u64;
        if terminal && callback_terminal {
            continuation.cpu.registers[4] = sp;
            continuation.cpu.stop = 3;
        } else if terminal {
            return Ok(Some(
                continuation
                    .return_convention
                    .completed(value as u32 as u64),
            ));
        }
        if !callback_terminal {
            continuation.cpu.rip = continuation.cpu.next_rip;
            continuation.cpu.stop = 0;
        }
        self.execute_flow(
            image,
            0,
            Arguments::default(),
            dependencies,
            Some(continuation),
            data,
        )
        .map(Some)
    }
    pub fn resume_mutex(
        &mut self,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        wait: &mut MutexWait,
    ) -> Result<Option<Execution>> {
        self.resume_mutex_with_data(image, dependencies, wait, &mut [])
    }
    pub fn with_mutex_wait_memory<T>(
        &mut self,
        wait: &mut MutexWait,
        image: &mut Image,
        dependencies: &mut [&mut Image],
        data: &mut [DataRegion<'_>],
        operation: impl FnOnce(&mut Memory<'_>) -> Result<T>,
    ) -> Result<T> {
        self.with_suspended_memory(
            wait.continuation
                .as_mut()
                .ok_or("Mutex wait already consumed")?,
            image,
            dependencies,
            data,
            operation,
        )
    }
    fn execute_flow(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
        continuation: Option<Continuation>,
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        self.execute_flow_entry(
            image,
            (address, arguments, ReturnConvention::ModuleStatus),
            dependencies,
            continuation,
            data,
        )
    }
    fn execute_flow_entry(
        &mut self,
        image: &mut Image,
        entry: (u64, Arguments, ReturnConvention),
        dependencies: &mut [&mut Image],
        mut continuation: Option<Continuation>,
        data: &mut [DataRegion<'_>],
    ) -> Result<Execution> {
        self.system_profile_locked = true;
        let mut retained = continuation.as_mut().map(|saved| {
            self.protect_stack(
                saved.stack_address,
                std::mem::take(&mut saved.stack),
                saved.stack_lease.clone(),
            )
        });
        self.require_running()?;
        if let (Some(saved), Some(retained)) = (continuation.as_mut(), retained.as_mut()) {
            saved.stack = retained.take();
        }
        let quantum = continuation.as_ref().map(|c| c.quantum).unwrap_or_else(|| {
            if matches!(entry.2, ReturnConvention::WorkerPointer) {
                self.worker_quantum
            } else {
                None
            }
        });
        let mut slice = (quantum, quantum.unwrap_or(u64::MAX));
        let mut environment_completion = None;
        loop {
            match self.execute_step(
                image,
                entry,
                dependencies,
                (continuation, environment_completion.take()),
                data,
                &mut slice,
            )? {
                Flow::Finished(execution) => return Ok(execution),
                Flow::EnvironmentSet {
                    continuation: mut saved,
                    imported,
                    name,
                    value,
                    overwrite,
                    errno,
                    terminal,
                } => {
                    // All borrowed mappings are gone. Guest addresses stay fixed;
                    // no host reference survives the owned arena mutation.
                    let mut retained = self.protect_stack(
                        saved.stack_address,
                        std::mem::take(&mut saved.stack),
                        saved.stack_lease.clone(),
                    );
                    let result = if let Some(mut environment) = imported {
                        let result = environment.set(&name, &value, overwrite);
                        if result.is_ok() {
                            self.environment = environment.table_address();
                            self.owned_environment = Some(environment);
                        }
                        result
                    } else {
                        self.owned_environment
                            .as_mut()
                            .ok_or("Environment owner disappeared")?
                            .set(&name, &value, overwrite)
                    };
                    if matches!(result, Err(crate::environment::EnvError::Corrupt)) {
                        return Err("Unsupported malformed direct environment mutation".into());
                    }
                    saved.cpu.registers[0] = if result.is_ok() { 0 } else { u32::MAX as u64 };
                    environment_completion =
                        Some((errno, result.err().map(|e| e.errno()), terminal));
                    saved.stack = retained.take();
                    continuation = Some(*saved);
                }
                Flow::TranslationMiss {
                    bias,
                    target,
                    continuation: mut saved,
                } => {
                    // execute_step dropped every borrowed mapping before yielding;
                    // compilation cannot hold a stale guest/stack pointer or invoke
                    // an old native block while its library is being replaced.
                    let mut retained = self.protect_stack(
                        saved.stack_address,
                        std::mem::take(&mut saved.stack),
                        saved.stack_lease.clone(),
                    );
                    let source = self
                        .translation_sources
                        .get(&bias)
                        .ok_or("Translation miss lost its compilation owner")?;
                    let archive = source.archive.clone();
                    let directory = source.directory.clone();
                    let owner = if image.bias == bias {
                        &*image
                    } else {
                        dependencies
                            .iter()
                            .find(|dependency| dependency.bias == bias)
                            .map(|dependency| &**dependency)
                            .ok_or("Translation miss owner is not mapped")?
                    };
                    // SAFETY: opt-in's contract pins the matching trusted archive;
                    // execution is suspended with no native block on the stack.
                    unsafe { self.extend_image_translation(owner, target, &archive, &directory) }
                        .map_err(|error| {
                        format!("Translation miss at {target:#x} (bias {bias:#x}): {error}")
                    })?;
                    // Unlike a host-service resume, do not replace RAX, consume a
                    // return frame or advance RIP. Retry the exact saved phase.
                    saved.stack = retained.take();
                    continuation = Some(*saved);
                }
            }
        }
    }
    fn execute_step(
        &mut self,
        image: &mut Image,
        entry: (u64, Arguments, ReturnConvention),
        dependencies: &mut [&mut Image],
        resume: (Option<Continuation>, EnvironmentCompletion),
        data: &mut [DataRegion<'_>],
        slice: &mut (Option<u64>, u64),
    ) -> Result<Flow> {
        let (mut continuation, environment_completion) = resume;
        let mut callbacks = continuation
            .as_mut()
            .and_then(|saved| saved.callbacks.take())
            .unwrap_or_else(|| {
                crate::guest_callback::Context::new(
                    self.callback_ranges.clone(),
                    self.callback_faults.clone(),
                )
                .with_abort_faults(self.abort_faults.clone(), self.fault_stack.clone())
            });
        let (address, arguments, initial_convention) = entry;
        let caller_bias = image.bias;
        let (
            saved_cpu,
            stack,
            remaining,
            stack_address,
            guest_thread,
            return_convention,
            stack_lease,
        ) = match continuation {
            Some(mut state) => (
                Some(std::mem::take(&mut state.cpu)),
                std::mem::take(&mut state.stack),
                state.remaining,
                state.stack_address,
                state.guest_thread,
                state.return_convention,
                state.stack_lease.clone(),
            ),
            None => {
                let mut candidate = self.stack_address;
                let mut selected = None;
                for _ in 0..128 {
                    let end = candidate
                        .checked_add(self.stack_size as u64)
                        .ok_or("Guest stack address overflow")?;
                    let mut overlap = self.callback_ranges.overlaps(candidate, self.stack_size);
                    for region in data.iter() {
                        let region_end = region
                            .address
                            .checked_add(region.bytes.len() as u64)
                            .ok_or("Guest data mapping overflow")?;
                        overlap |= candidate < region_end && region.address < end;
                    }
                    if !overlap {
                        selected = Some(candidate);
                        break;
                    }
                    candidate = end;
                }
                (
                    None,
                    vec![0; self.stack_size],
                    self.transitions,
                    selected.ok_or("Guest stack slots exhausted")?,
                    self.selected_guest_thread,
                    initial_convention,
                    Rc::new(()),
                )
            }
        };
        let stack_length = stack.len();
        let pending_stack = Rc::new(Cell::new(retained_overlap(
            &self.semaphores,
            &self.callback_ranges,
            stack_address,
            stack_length,
        )));
        let mut stack = ExecutionStack {
            condition_transfer: self.condition_transfer_active,
            lease: stack_lease.clone(),
            bytes: stack,
            address: stack_address,
            pending: pending_stack.clone(),
            fault: self.fault_stack.clone(),
        };
        self.register_stack(stack_address, &stack, &stack_lease)?;
        if !self.tls.contains_thread(guest_thread) {
            return Err("Guest execution thread identity is not registered".into());
        }
        let top = stack_address + stack.len() as u64 - 8;
        let mut ranges = Vec::new();
        let mut image_storage = Vec::new();
        for mapped in std::iter::once(&*image).chain(dependencies.iter().map(|i| &**i)) {
            for segment in &mapped.segments {
                image_storage.push((segment.address, segment.bytes.len()));
                if segment.flags & 1 != 0 {
                    ranges.push((
                        segment.address,
                        segment.address + segment.bytes.len() as u64,
                        mapped.bias,
                    ));
                }
            }
        }
        let owner = |address| {
            ranges
                .iter()
                .find(|&&(begin, end, _)| address >= begin && address < end)
                .map(|&(_, _, bias)| bias)
                .ok_or("Target outside executable module mappings")
        };
        let block_at = |bias, offset| {
            self.image_blocks
                .get(&(bias, offset))
                .or_else(|| self.blocks.get(&offset))
        };
        let has_owned_environment = self.owned_environment.is_some();
        if let Some(environment) = self.owned_environment.as_ref() {
            if self.environment != environment.table_address() {
                return Err(
                    "Direct replacement of the owned environment table is unsupported".into(),
                );
            }
            environment
                .validate_contents()
                .map_err(|e| format!("Corrupt guest environment: {e:?}"))?;
        }
        let (gdtoa_pool, gdtoa_state) = self.gdtoa.split_mut();
        let mut memory = Memory::default();
        for mapped in std::iter::once(&mut *image).chain(dependencies.iter_mut().map(|i| &mut **i))
        {
            for segment in &mut mapped.segments {
                memory.add(
                    segment.address,
                    &mut segment.bytes,
                    segment.flags & 2 != 0,
                    segment.flags & 1 != 0,
                )?;
            }
        }
        memory.add(stack_address, &mut stack, true, false)?;
        for region in data {
            memory.add(region.address, region.bytes, region.writable, false)?;
        }
        memory.attach_owned(&mut self.allocations)?;
        if self.services.values().any(|service| matches!(service, Service::Vasprintf)) {
            memory.add_runtime_data(GDTOA_POOL, gdtoa_pool, gdtoa_state.runtime_owner())?;
        }

        if let Some(environment) = self.owned_environment.as_mut() {
            for (index, (address, bytes)) in environment.mappings().into_iter().enumerate() {
                if !bytes.is_empty() {
                    memory.add(address, bytes, index != 0, false)?;
                }
            }
        }
        if self
            .services
            .values()
            .any(|s| matches!(s, Service::Strerror | Service::InternalStrerror))
        {
            memory.add_service_data(ERROR_TEXT, &mut self.error_text)?;
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::NetErrnoLoc))
        {
            memory.add(NET_ERRNO, &mut self.network_errno, true, false)?;
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::Setlocale))
        {
            memory.add_service_data(LOCALE_NAME, &mut self.locale_name)?;
        }
        if self.services.values().any(|service| {
            matches!(
                service,
                Service::Fprintf | Service::Fflush | Service::File(_) | Service::Abort
            )
        }) {
            memory.add(
                crate::guest_stdio::FILE_ADDR,
                &mut self.stderr_file,
                true,
                false,
            )?;
            memory.add_service_data(
                crate::guest_stdio::STDERR_PTR_ADDR,
                &mut self.stderr_pointer,
            )?;
        }
        if self
            .services
            .values()
            .any(|service| matches!(service, Service::File(_) | Service::Fgets | Service::Abort))
        {
            memory.add(
                crate::stdio_file::BASE,
                &mut self.file_metadata,
                true,
                false,
            )?;
        }
        if let Some(metadata) = self.stdin_metadata.as_mut() {
            memory.add(crate::stdio_file::STDIN_FILE_ADDR, metadata, true, false)?;
            memory.add_service_data(crate::stdio_file::STDIN_PTR_ADDR, &mut self.stdin_pointer)?;
        }
        for &address in self.services.keys() {
            memory.reserve_service(address);
        }
        if self
            .services
            .keys()
            .any(|&address| memory.read(address, 1).is_ok())
        {
            return Err("Service thunk overlaps mapped guest bytes".into());
        }
        memory.set_write_barriers(callbacks.registry.barriers());
        let mut cpu = if let Some(cpu) = saved_cpu {
            cpu
        } else {
            memory.store(top, u64::MAX, 8)?;
            let mut cpu = Cpu {
                bias: owner(address)?,
                rip: address
                    .checked_sub(owner(address)?)
                    .ok_or("Entry below image")?,
                fuel: self.fuel,
                ..Cpu::default()
            };
            cpu.registers[4] = top;
            cpu.registers[7] = arguments.size;
            cpu.registers[6] = arguments.pointer;
            cpu
        };
        memory.set_write_barriers(callbacks.registry.barriers());
        macro_rules! completed {
            ($value:expr) => {{
                let value = $value;
                if !callbacks.is_empty() { return Err("Process return bypasses active comparator frame".into()); }
                drop(memory);
                let pinned = retained_overlap(&self.semaphores, &self.callback_ranges, stack_address, stack.len());
                pending_stack.set(pinned);
                if pinned {
                    pending_stack.set(true);
                    return Err("Guest returned while its stack contains a pending semaphore; process fault pinned".into());
                }
                stack.condition_transfer = false;
                return Ok(Flow::Finished(return_convention.completed(value)));
            }};
        }
        macro_rules! signal_terminated {
            ($signal:expr, $remaining:expr) => {{
                let signal = $signal;
                cpu.memory = 0;
                drop(memory);
                self.exit_context = Some(Box::new(Continuation {
                    callbacks: Some(callbacks),
                    stack_lease: stack_lease.clone(),
                    quantum: slice.0,
                    return_convention,
                    guest_thread,
                    cpu,
                    stack: stack.take(),
                    stack_address,
                    remaining: $remaining,
                    caller_bias,
                }));
                self.signal_termination = Some(signal);
                return Ok(Flow::Finished(Execution::SignalTerminated { signal }));
            }};
        }
        if let Some((errno, error, terminal)) = environment_completion {
            if let Some(error) = error {
                memory.store(errno, error as u64, 4)?;
            }
            if terminal && !callbacks.authentic_return(cpu.registers[4], u64::MAX) {
                completed!(cpu.registers[0]);
            }
        }
        for transition in 0..remaining {
            memory.set_write_barriers(callbacks.registry.barriers());
            if slice.0.is_some() && slice.1 == 0 {
                if cpu.fault != 0 {
                    return Err(format!(
                        "Module guest fault {} at {:#x}",
                        cpu.fault, cpu.rip
                    ));
                }
                if cpu.stop == 3 && cpu.registers[4] == top && memory.load(top, 8)? == u64::MAX {
                    completed!(cpu.registers[0]);
                }
                if cpu.fuel == 0 {
                    return Err("Module fuel exhausted".into());
                }
                cpu.memory = 0;
                drop(memory);
                return Ok(Flow::Finished(Execution::QuantumYield(Box::new(
                    QuantumYield {
                        owner: self.mutex_wait_owner.clone(),
                        continuation: Some(Continuation {
                            callbacks: Some(callbacks),
                            stack_lease: stack_lease.clone(),
                            quantum: slice.0,
                            return_convention,
                            guest_thread,
                            cpu,
                            stack: stack.take(),
                            stack_address,
                            remaining: remaining - transition,
                            caller_bias,
                        }),
                    },
                ))));
            }
            // Invoke only before effects associated with the pending dispatch.
            // Every retry consumes transition budget, but no guest instruction
            // or service is replayed and instruction fuel is unchanged.
            macro_rules! translation_miss {
                ($bias:expr, $offset:expr) => {{
                    let missing_bias = $bias;
                    let missing_target = missing_bias
                        .checked_add($offset)
                        .ok_or("Translation target overflow")?;
                    if self.automatic_translation
                        && cpu.fuel != 0
                        && self.translation_sources.contains_key(&missing_bias)
                        && memory.executable(missing_target)
                    {
                        drop(memory);
                        return Ok(Flow::TranslationMiss {
                            bias: missing_bias,
                            target: missing_target,
                            continuation: Box::new(Continuation {
                                callbacks: Some(callbacks),
                                stack_lease: stack_lease.clone(),
                                quantum: slice.0,
                                return_convention,
                                guest_thread,
                                cpu,
                                stack: stack.take(),
                                stack_address,
                                remaining: remaining - transition - 1,
                                caller_bias,
                            }),
                        });
                    }
                }};
            }
            if callbacks.frames.last().is_some_and(|frame| !frame.waiting) {
                use crate::guest_compare::Event;
                let frame = callbacks.frames.last_mut().unwrap();
                if frame.pending.is_none() {
                    frame.pending = Some(frame.engine.next()?);
                }
                match frame.pending.as_ref().unwrap() {
                    Event::Compare { left, right, .. } => {
                        let (left, right, comparator, callback_sp) =
                            (*left, *right, frame.comparator, frame.callback_sp);
                        let target_bias = owner(comparator)?;
                        let target = comparator
                            .checked_sub(target_bias)
                            .ok_or("Comparator below image")?;
                        if block_at(target_bias, target).is_none() {
                            translation_miss!(target_bias, target);
                        }
                        if !memory.executable(comparator) || block_at(target_bias, target).is_none()
                        {
                            return Err("Unregistered comparator target".into());
                        }
                        if memory.load(callback_sp, 8)? != u64::MAX {
                            return Err("Comparator return token changed".into());
                        }
                        if cpu.fuel == 0 {
                            return Err("Comparator dispatch fuel exhausted".into());
                        }
                        cpu.fuel -= 1;
                        self.consumed_instruction_fuel += 1;
                        slice.1 = slice.1.saturating_sub(1);
                        callbacks.frames.last_mut().unwrap().waiting = true;
                        cpu.registers[7] = left;
                        cpu.registers[6] = right;
                        cpu.registers[4] = callback_sp;
                        cpu.bias = target_bias;
                        cpu.rip = target;
                        cpu.stop = 0;
                        continue;
                    }
                    Event::Swap {
                        request,
                        left,
                        right,
                        size,
                    } => {
                        frame.engine.validate_swap(request)?;
                        let (left, right, size) = (*left, *right, *size as usize);
                        memory.validate_write(left, size)?;
                        memory.validate_write(right, size)?;
                        if cpu.fuel == 0 {
                            return Err("Comparator swap fuel exhausted".into());
                        }
                        let mut first = Vec::new();
                        let mut second = Vec::new();
                        first
                            .try_reserve_exact(size)
                            .map_err(|_| "Cannot reserve first comparator swap snapshot")?;
                        second
                            .try_reserve_exact(size)
                            .map_err(|_| "Cannot reserve second comparator swap snapshot")?;
                        first.extend_from_slice(memory.read(left, size)?);
                        second.extend_from_slice(memory.read(right, size)?);
                        memory.write(left, &second)?;
                        memory.write(right, &first)?;
                        let Some(Event::Swap { request, .. }) = frame.pending.take() else {
                            unreachable!()
                        };
                        frame.engine.swapped(request)?;
                        cpu.fuel -= 1;
                        self.consumed_instruction_fuel += 1;
                        slice.1 = slice.1.saturating_sub(1);
                        continue;
                    }
                    Event::Done(value) => {
                        let value = *value;
                        if memory.load(frame.return_sp, 8)? != frame.return_address {
                            return Err("Comparator parent return changed".into());
                        }
                        let frame = callbacks.frames.pop().unwrap();
                        cpu.registers = frame.registers;
                        cpu.registers[0] = value;
                        cpu.bias = frame.return_bias;
                        cpu.next_rip = frame.return_offset;
                        cpu.registers[4] = frame.return_sp;
                        if frame.terminal {
                            cpu.stop = 3;
                        } else {
                            cpu.registers[4] += 8;
                            cpu.rip = frame.return_offset;
                            cpu.stop = 0;
                        }
                        drop(frame);
                        continue;
                    }
                }
            }
            if cpu.fault != 0 {
                return Err(format!(
                    "Module guest fault {} at {:#x}",
                    cpu.fault, cpu.rip
                ));
            }
            if cpu.stop == 3 {
                let target = memory.load(cpu.registers[4], 8)?;
                if callbacks.authentic_abort_return(cpu.registers[4], target) {
                    let frame = callbacks
                        .abort_frames
                        .last_mut()
                        .ok_or("Missing abort handler frame")?;
                    if frame.thread != guest_thread {
                        return Err("Abort handler returned on another thread".into());
                    }
                    let returned = frame
                        .model
                        .prepare_handler_return(&frame.ticket, guest_thread)?;
                    // TLS validity is checked before publishing the model transition.
                    self.tls
                        .replace_signal_mask(guest_thread, frame.restore_mask)?;
                    returned.commit();
                    frame.restore_cpu(&mut cpu);
                    let signal = force_abort_default(
                        &mut frame.model,
                        &mut self.signal_actions,
                        &mut self.tls,
                        guest_thread,
                    )?;
                    signal_terminated!(signal, remaining - transition - 1);
                }
                if callbacks.authentic_return(cpu.registers[4], target) {
                    callbacks.accept_return(cpu.registers[0])?;
                    cpu.stop = 0;
                    continue;
                }
                if target == u64::MAX && cpu.registers[4] == top {
                    completed!(cpu.registers[0]);
                }
                if !memory.executable(target) {
                    return Err("Invalid guest return target".into());
                }
                let return_bias = owner(target)?;
                let return_offset = target
                    .checked_sub(return_bias)
                    .ok_or("Return below image")?;
                if block_at(return_bias, return_offset).is_none() {
                    translation_miss!(return_bias, return_offset);
                }
                cpu.bias = owner(target)?;
                cpu.rip = target.checked_sub(cpu.bias).ok_or("Return below image")?;
                cpu.registers[4] = cpu.registers[4]
                    .checked_add(8)
                    .ok_or("Return stack overflow")?;
            } else if cpu.stop == 1 || cpu.stop == 5 {
                let tail = cpu.stop == 5;
                if cpu.fuel == 0 {
                    return Err("Module fuel exhausted before call".into());
                }
                if let Some(service) = self.services.get(&cpu.target) {
                    // A noreturn service has no valid continuation to translate
                    // or pop. Real callers commonly put UD2 immediately after it.
                    if matches!(service, Service::Abort) {
                        // Cleanup is a metadata-checked flush of supported unbuffered
                        // FILEs. No host exit, atexit, fclose or fake resource teardown.
                        self.stdio.validate_cleanup(&memory)?;
                        self.file_streams.validate_cleanup(&memory)?;
                        self.tls.signal_mask(guest_thread)?;
                        let mut abort = crate::guest_abort::Abort::new(guest_thread)?;
                        abort.cleanup_validated()?;
                        let snapshot = self.signal_actions.snapshot(crate::guest_abort::SIGABRT)?;
                        let raise = abort.prepare_raise(&snapshot)?;
                        let handler_setup = if let crate::guest_abort::Decision::Handler {
                            address,
                            mask,
                            restore_mask,
                            reset_before_entry,
                            ..
                        } = raise.decision()
                        {
                            if callbacks.abort_frames.len() + callbacks.frames.len() >= 32 {
                                return Err("Abort/callback nesting budget exhausted".into());
                            }
                            let address = *address;
                            let target_bias = owner(address)?;
                            let target_offset = address
                                .checked_sub(target_bias)
                                .ok_or("Abort handler below image")?;
                            if block_at(target_bias, target_offset).is_none() {
                                translation_miss!(target_bias, target_offset);
                            }
                            if !memory.executable(address)
                                || block_at(target_bias, target_offset).is_none()
                            {
                                return Err("Unregistered abort handler".into());
                            }
                            let handler_sp =
                                callback_stack_slot(cpu.registers[4], stack_address, 0, 0, None)?;
                            memory.validate_write(handler_sp, 8)?;
                            callbacks
                                .abort_frames
                                .try_reserve(1)
                                .map_err(|_| "Cannot reserve abort handler frame")?;
                            self.abort_faults
                                .borrow_mut()
                                .try_reserve(callbacks.abort_frames.len() + 1)
                                .map_err(|_| "Cannot reserve abort fault ownership")?;
                            self.fault_stack
                                .borrow_mut()
                                .try_reserve(self.stack_identities.len() + 1)
                                .map_err(|_| "Cannot reserve abort stack fault ownership")?;
                            let mut ranges = Vec::new();
                            ranges
                                .try_reserve(3)
                                .map_err(|_| "Cannot reserve abort backing ranges")?;
                            ranges.push(condition_storage(
                                &memory,
                                handler_sp,
                                &image_storage,
                                &self.stack_identities,
                                &stack_lease,
                            )?);
                            ranges.push(callback_storage(
                                &memory,
                                address,
                                1,
                                &image_storage,
                                &self.stack_identities,
                            )?);
                            let interrupted = cpu
                                .bias
                                .checked_add(cpu.rip)
                                .ok_or("Abort interrupted address overflow")?;
                            ranges.push(callback_storage(
                                &memory,
                                interrupted,
                                1,
                                &image_storage,
                                &self.stack_identities,
                            )?);
                            let lease = self
                                .callback_ranges
                                .acquire(ranges, [handler_sp, handler_sp])?;
                            Some((
                                address,
                                target_bias,
                                target_offset,
                                handler_sp,
                                *mask,
                                *restore_mask,
                                *reset_before_entry,
                                lease,
                            ))
                        } else {
                            None
                        };
                        let replacement = handler_setup
                            .as_ref()
                            .and_then(|s| s.6.then(|| snapshot.reset_for_delivery()));
                        let action = self
                            .signal_actions
                            .prepare_snapshot(&snapshot, replacement)?;
                        let mask = handler_setup
                            .as_ref()
                            .map(|s| s.4)
                            .unwrap_or_else(crate::guest_abort::abort_mask);
                        // No state publication precedes frame/ownership preflight.
                        if let Some(setup) = &handler_setup {
                            memory.store(setup.3, u64::MAX, 8)?;
                        }
                        self.tls.replace_signal_mask(guest_thread, mask)?;
                        action.commit();
                        let decision = raise.commit();
                        cpu.fuel -= 1;
                        self.consumed_instruction_fuel += 1;
                        slice.1 = slice.1.saturating_sub(1);
                        match decision {
                            crate::guest_abort::Decision::Terminate { signal } => {
                                signal_terminated!(signal, remaining - transition - 1)
                            }
                            crate::guest_abort::Decision::ForceDefault => {
                                let signal = force_abort_default(
                                    &mut abort,
                                    &mut self.signal_actions,
                                    &mut self.tls,
                                    guest_thread,
                                )?;
                                signal_terminated!(signal, remaining - transition - 1);
                            }
                            crate::guest_abort::Decision::Handler { ticket, .. } => {
                                let (
                                    _,
                                    target_bias,
                                    target_offset,
                                    handler_sp,
                                    _,
                                    restore_mask,
                                    _,
                                    lease,
                                ) = handler_setup.expect("Prepared caught handler");
                                let mut saved_cpu = cpu.clone();
                                saved_cpu.memory = 0;
                                callbacks
                                    .abort_frames
                                    .push(crate::guest_callback::AbortFrame {
                                        model: abort,
                                        ticket,
                                        cpu: saved_cpu,
                                        restore_mask,
                                        thread: guest_thread,
                                        comparator_depth: callbacks.frames.len(),
                                        callback_sp: handler_sp,
                                        _lease: lease,
                                    });
                                pending_stack.set(true);
                                cpu.registers[7] = crate::guest_abort::SIGABRT as u64;
                                cpu.registers[4] = handler_sp;
                                cpu.bias = target_bias;
                                cpu.rip = target_offset;
                                cpu.stop = 0;
                                continue;
                            }
                        }
                    }
                    if matches!(service, Service::Exit | Service::ImmediateExit) {
                        let request = ExitRequest {
                            status: cpu.registers[7] as u32 as i32,
                            kind: if matches!(service, Service::Exit) {
                                ExitKind::Normal
                            } else {
                                ExitKind::Immediate
                            },
                        };
                        cpu.fuel -= 1;
                        self.consumed_instruction_fuel += 1;
                        drop(memory);
                        self.exit_context = Some(Box::new(Continuation {
                            callbacks: Some(callbacks),
                            stack_lease: stack_lease.clone(),
                            quantum: slice.0,
                            return_convention,
                            guest_thread,
                            cpu,
                            stack: stack.take(),
                            stack_address,
                            remaining: remaining - transition - 1,
                            caller_bias,
                        }));
                        self.exit_request = Some(request);
                        return Ok(Flow::Finished(Execution::ExitRequested(request)));
                    }
                    let ret = if tail {
                        memory.load(cpu.registers[4], 8)?
                    } else {
                        cpu.bias
                            .checked_add(cpu.next_rip)
                            .ok_or("Service return overflow")?
                    };
                    let terminal = tail
                        && ret == u64::MAX
                        && (callbacks.authentic_return(cpu.registers[4], ret)
                            || (cpu.registers[4] == top && callbacks.is_empty()));
                    if !terminal {
                        let return_bias = owner(ret)?;
                        let return_offset = ret
                            .checked_sub(return_bias)
                            .ok_or("Service return below image")?;
                        if block_at(return_bias, return_offset).is_none() {
                            translation_miss!(return_bias, return_offset);
                        }
                        if !memory.executable(ret) || block_at(return_bias, return_offset).is_none()
                        {
                            return Err("Unregistered service return".into());
                        }
                        cpu.bias = return_bias;
                        cpu.next_rip = return_offset;
                    }
                    let sp = if tail {
                        cpu.registers[4]
                    } else {
                        cpu.registers[4]
                            .checked_sub(8)
                            .ok_or("Service stack underflow")?
                    };
                    if !tail {
                        memory.store(sp, ret, 8)?;
                    }
                    cpu.registers[4] = sp;
                    cpu.fuel -= 1;
                    self.consumed_instruction_fuel += 1;
                    let mut condition_value = None;
                    if matches!(
                        service,
                        Service::CondInit
                            | Service::CondDestroy
                            | Service::CondSignal
                            | Service::CondBroadcast
                            | Service::CondWait
                            | Service::CondTimedWait
                    ) {
                        use crate::guest_cond::{Outcome, Realtime};
                        let slot = cpu.registers[7];
                        let mutex_slot = cpu.registers[6];
                        let plan = match service {
                            Service::CondInit => self.conditions.init(&memory, slot, mutex_slot)?,
                            Service::CondDestroy => self.conditions.destroy(&memory, slot)?,
                            Service::CondSignal | Service::CondBroadcast => self
                                .conditions
                                .signal(&memory, slot, matches!(service, Service::CondBroadcast))?,
                            _ => {
                                let deadline = if matches!(service, Service::CondTimedWait) {
                                    let bytes = memory.read(cpu.registers[2], 16)?;
                                    Some(Realtime {
                                        seconds: i64::from_le_bytes(bytes[..8].try_into().unwrap()),
                                        nanoseconds: i64::from_le_bytes(
                                            bytes[8..].try_into().unwrap(),
                                        ),
                                    })
                                } else {
                                    None
                                };
                                self.conditions.wait(
                                    &memory,
                                    slot,
                                    guest_thread,
                                    mutex_slot,
                                    deadline,
                                )?
                            }
                        };
                        if memory.load(sp, 8)? != ret {
                            return Err("Condition return frame changed".into());
                        }
                        if let Some((address, length)) = plan.write_range() {
                            validate_net_output(&memory, address, length, sp, ret)?;
                        }
                        if let Outcome::Pending(ticket) = plan.outcome() {
                            // Reject aliases even when the condition is already initialized:
                            // a retained mutex/condition object must not be a return slot.
                            validate_net_output(&memory, slot, 8, sp, ret)?;
                            validate_net_output(&memory, mutex_slot, 8, sp, ret)?;
                            let mut ranges = Vec::new();
                            ranges
                                .try_reserve(4)
                                .map_err(|_| "Cannot reserve condition backing ranges")?;
                            for address in [slot, mutex_slot, sp] {
                                ranges.push(condition_storage(
                                    &memory,
                                    address,
                                    &image_storage,
                                    &self.stack_identities,
                                    &stack_lease,
                                )?);
                            }
                            if !terminal {
                                ranges.push(callback_storage(
                                    &memory,
                                    ret,
                                    1,
                                    &image_storage,
                                    &self.stack_identities,
                                )?);
                            }
                            self.condition_leases
                                .try_reserve(1)
                                .map_err(|_| "Cannot reserve condition lifetime lease")?;
                            self.condition_faults
                                .borrow_mut()
                                .try_reserve(self.condition_leases.len() + 1)
                                .map_err(|_| "Cannot reserve abandoned condition continuations")?;
                            let lease = self.callback_ranges.acquire(ranges, [sp, sp])?;
                            // Allocate request/token storage before either model mutates.
                            let mut pending = Box::new(ThreadRequest {
                                owner: self.mutex_wait_owner.clone(),
                                continuation: None,
                                operation: ThreadOperation::Completed(0),
                                condition_faults: Some(self.condition_faults.clone()),
                            });
                            let ticket = ticket.clone();
                            let mut token_storage = Rc::new(None);
                            match self
                                .mutex
                                .prepare_cond_wait(&memory, mutex_slot, guest_thread)?
                            {
                                Err(status) => {
                                    condition_value = Some(status as u32 as u64);
                                }
                                Ok(detach) => {
                                    self.conditions.commit(&mut memory, plan)?;
                                    let mutex_ticket = detach.commit();
                                    *Rc::get_mut(&mut token_storage)
                                        .expect("Unpublished condition token") = Some(mutex_ticket);
                                    self.condition_leases.push((guest_thread, lease));
                                    pending.operation = ThreadOperation::ConditionWait {
                                        ticket,
                                        mutex_ticket: token_storage,
                                        return_sp: sp,
                                        return_address: ret,
                                    };
                                    pending_stack.set(true);
                                    if terminal {
                                        cpu.stop = 3;
                                    } else {
                                        cpu.registers[4] = sp + 8;
                                    }
                                    cpu.memory = 0;
                                    drop(memory);
                                    pending.continuation = Some(Continuation {
                                        callbacks: Some(callbacks),
                                        stack_lease: stack_lease.clone(),
                                        quantum: slice.0,
                                        return_convention,
                                        guest_thread,
                                        cpu,
                                        stack: stack.take(),
                                        stack_address,
                                        remaining: remaining - transition - 1,
                                        caller_bias,
                                    });
                                    return Ok(Flow::Finished(Execution::ThreadRequest(pending)));
                                }
                            }
                        } else {
                            let Outcome::Status(status) =
                                self.conditions.commit(&mut memory, plan)?
                            else {
                                return Err("Unexpected condition service phase".into());
                            };
                            condition_value = Some(status as u32 as u64);
                        }
                    }
                    let mut semaphore_value = None;
                    if matches!(
                        service,
                        Service::SemInit
                            | Service::SemDestroy
                            | Service::SemPost
                            | Service::SemGetValue
                            | Service::SemTryWait
                            | Service::SemWait
                            | Service::SemTimedWait
                    ) {
                        let address = cpu.registers[7];
                        let plan = match service {
                            Service::SemInit => self.semaphores.init(
                                &memory,
                                address,
                                cpu.registers[6] as i32,
                                cpu.registers[2] as u32,
                            )?,
                            Service::SemDestroy => self.semaphores.destroy(&memory, address)?,
                            Service::SemPost => self.semaphores.post(&memory, address)?,
                            Service::SemGetValue => self.semaphores.getvalue(&memory, address)?,
                            Service::SemTryWait => self.semaphores.trywait(&memory, address)?,
                            _ => self.semaphores.wait(
                                &memory,
                                address,
                                guest_thread,
                                if matches!(service, Service::SemTimedWait) {
                                    cpu.registers[6]
                                } else {
                                    0
                                },
                                || {
                                    let (seconds, nanoseconds) =
                                        self.guest_time.realtime_parts()?;
                                    Ok(crate::guest_semaphore::Realtime {
                                        seconds,
                                        nanoseconds,
                                    })
                                },
                            )?,
                        };
                        let errno = self
                            .kernel
                            .as_ref()
                            .ok_or("Missing semaphore errno context")?
                            .errno_address();
                        let output =
                            matches!(service, Service::SemGetValue).then_some(cpu.registers[6]);
                        guard_semaphore_plan(&memory, &plan, output, errno, sp, ret)?;
                        // Allocate retained ownership before committing HAS_WAITERS
                        // or a ticket. No fallible handoff follows the commit.
                        let pending = if let crate::guest_semaphore::Outcome::Pending(ticket) =
                            plan.outcome()
                        {
                            if !semaphore_storage_owned(
                                &memory,
                                address,
                                &image_storage,
                                &self.stack_identities,
                            )? {
                                return Err("Blocking semaphore requires owned allocation, live image, or retained stack backing".into());
                            }
                            Some(Box::new(ThreadRequest {
                                condition_faults: None,
                                owner: self.mutex_wait_owner.clone(),
                                continuation: None,
                                operation: ThreadOperation::SemaphoreWait {
                                    ticket: ticket.clone(),
                                    return_sp: sp,
                                    return_address: ret,
                                    errno,
                                },
                            }))
                        } else {
                            None
                        };
                        let outcome = self.semaphores.commit(&mut memory, plan)?;
                        pending_stack.set(retained_overlap(
                            &self.semaphores,
                            &self.callback_ranges,
                            stack_address,
                            stack_length,
                        ));
                        if let Some(mut request) = pending {
                            if terminal {
                                cpu.stop = 3;
                            } else {
                                cpu.registers[4] = sp + 8;
                            }
                            cpu.memory = 0;
                            drop(memory);
                            request.continuation = Some(Continuation {
                                callbacks: Some(callbacks),
                                stack_lease: stack_lease.clone(),
                                quantum: slice.0,
                                return_convention,
                                guest_thread,
                                cpu,
                                stack: stack.take(),
                                stack_address,
                                remaining: remaining - transition - 1,
                                caller_bias,
                            });
                            return Ok(Flow::Finished(Execution::ThreadRequest(request)));
                        }
                        semaphore_value =
                            finish_semaphore_outcome(&mut memory, outcome, output, errno)?;
                    }
                    let sleep_request = if matches!(service, Service::Nanosleep) {
                        Some(crate::guest_sleep::Request::decode(
                            &memory,
                            cpu.registers[7],
                        )?)
                    } else {
                        None
                    };
                    if matches!(
                        service,
                        Service::ThreadCreate | Service::ThreadJoin | Service::ThreadDetach
                    ) || matches!(
                        sleep_request,
                        Some(crate::guest_sleep::Request::Delay { .. })
                    ) {
                        let operation =
                            if let Some(crate::guest_sleep::Request::Delay { nanoseconds }) =
                                sleep_request
                            {
                                ThreadOperation::Sleep {
                                    deadline: self.sleep_clock.deadline(nanoseconds)?,
                                }
                            } else if matches!(service, Service::ThreadCreate) {
                                let output = cpu.registers[7];
                                memory.validate_write(output, 8)?;
                                if output < sp + 8
                                    && sp < output.checked_add(8).ok_or("Thread output overflow")?
                                {
                                    return Err("Thread output overlaps return frame".into());
                                }
                                let options = self
                                    .thread_attributes
                                    .snapshot(&memory, cpu.registers[6])?
                                    .ok_or("Invalid thread attributes")?;
                                let entry = cpu.registers[2];
                                if !memory.executable(entry) {
                                    return Err("Thread entry is not executable".into());
                                }
                                ThreadOperation::Create {
                                    output,
                                    entry,
                                    argument: cpu.registers[1],
                                    options,
                                }
                            } else if matches!(service, Service::ThreadJoin) {
                                let output = cpu.registers[6];
                                if output != 0 {
                                    memory.validate_write(output, 8)?;
                                    if output < sp + 8
                                        && sp
                                            < output.checked_add(8).ok_or("Join output overflow")?
                                    {
                                        return Err("Join output overlaps return frame".into());
                                    }
                                }
                                ThreadOperation::Join {
                                    thread: cpu.registers[7],
                                    output,
                                }
                            } else {
                                ThreadOperation::Detach {
                                    thread: cpu.registers[7],
                                }
                            };
                        if terminal {
                            cpu.stop = 3;
                        } else {
                            cpu.registers[4] = sp + 8;
                        }
                        cpu.memory = 0;
                        drop(memory);
                        return Ok(Flow::Finished(Execution::ThreadRequest(Box::new(
                            ThreadRequest {
                                condition_faults: None,
                                owner: self.mutex_wait_owner.clone(),
                                operation,
                                continuation: Some(Continuation {
                                    callbacks: Some(callbacks),
                                    stack_lease: stack_lease.clone(),
                                    quantum: slice.0,
                                    return_convention,
                                    guest_thread,
                                    cpu,
                                    stack: stack.take(),
                                    stack_address,
                                    remaining: remaining - transition - 1,
                                    caller_bias,
                                }),
                            },
                        ))));
                    }
                    if matches!(
                        service,
                        Service::Dlsym
                            | Service::LoadStartModule
                            | Service::StopUnloadModule
                            | Service::SysmoduleLoadInternal
                    ) {
                        // Complete the thunk return before yielding. No raw Memory
                        // pointer or borrow escapes; nested module initialization
                        // may use a fresh independent executor stack.
                        if terminal {
                            cpu.stop = 3;
                        } else {
                            cpu.registers[4] = sp + 8;
                        }
                        let arguments = [7, 6, 2, 1, 8, 9].map(|i| cpu.registers[i]);
                        drop(memory);
                        let continuation = Box::new(Continuation {
                            callbacks: Some(callbacks),
                            stack_lease: stack_lease.clone(),
                            quantum: slice.0,
                            return_convention,
                            guest_thread,
                            cpu,
                            stack: stack.take(),
                            stack_address,
                            remaining: remaining - transition - 1,
                            caller_bias,
                        });
                        return Ok(Flow::Finished(
                            if matches!(service, Service::SysmoduleLoadInternal) {
                                Execution::SysmoduleLoadInternal {
                                    arguments,
                                    continuation,
                                }
                            } else if matches!(service, Service::Dlsym) {
                                Execution::Dlsym {
                                    arguments,
                                    continuation,
                                }
                            } else if matches!(service, Service::StopUnloadModule) {
                                Execution::StopUnloadModule {
                                    arguments,
                                    continuation,
                                }
                            } else {
                                Execution::LoadStartModule {
                                    arguments,
                                    continuation,
                                }
                            },
                        ));
                    }
                    let value = if matches!(service, Service::Qsort | Service::Bsearch) {
                        let search = matches!(service, Service::Bsearch);
                        let (base, count, size, comparator, key) = if search {
                            (
                                cpu.registers[6],
                                cpu.registers[2],
                                cpu.registers[1],
                                cpu.registers[8],
                                cpu.registers[7],
                            )
                        } else {
                            (
                                cpu.registers[7],
                                cpu.registers[6],
                                cpu.registers[2],
                                cpu.registers[1],
                                0,
                            )
                        };
                        let engine = if search {
                            crate::guest_callback::Engine::Search(crate::guest_search::Search::new(
                                key, base, count, size,
                            )?)
                        } else {
                            crate::guest_callback::Engine::Sort(crate::guest_sort::Sort::new(
                                base, count, size,
                            )?)
                        };
                        if count == 0 || (!search && count == 1) {
                            0
                        } else {
                            if callbacks.frames.len() >= 32 {
                                return Err("Nested comparator depth exceeds32".into());
                            }
                            let length = count
                                .checked_mul(size)
                                .ok_or("Comparator extent overflow")?
                                as usize;
                            let array = callback_storage(
                                &memory,
                                base,
                                length,
                                &image_storage,
                                &self.stack_identities,
                            )?;
                            if !search {
                                validate_net_output(&memory, base, length, sp, ret)?;
                                memory.validate_write(base, length)?;
                            }
                            let mut ranges = vec![array, (stack_address, stack_length)];
                            if search {
                                ranges.push(callback_storage(
                                    &memory,
                                    key,
                                    1,
                                    &image_storage,
                                    &self.stack_identities,
                                )?);
                            }
                            if !memory.executable(comparator) {
                                return Err("Comparator is not executable guest code".into());
                            }
                            ranges.push(callback_storage(
                                &memory,
                                comparator,
                                1,
                                &image_storage,
                                &self.stack_identities,
                            )?);
                            let callback_sp = callback_stack_slot(
                                sp,
                                stack_address,
                                base,
                                length,
                                search.then_some(key),
                            )?;
                            memory.validate_write(callback_sp, 8)?;
                            callbacks
                                .frames
                                .try_reserve(1)
                                .map_err(|_| "Cannot reserve comparator frame")?;
                            // All fallible capacity reservations and overlap checks precede
                            // publishing bytes. Refresh barriers after the frame is installed.
                            let lease = callbacks.registry.acquire(ranges, [sp, callback_sp])?;
                            memory.store(callback_sp, u64::MAX, 8)?;
                            callbacks.frames.push(crate::guest_callback::Frame {
                                engine,
                                pending: None,
                                waiting: false,
                                comparator,
                                registers: cpu.registers,
                                return_sp: sp,
                                return_address: ret,
                                return_bias: cpu.bias,
                                return_offset: cpu.next_rip,
                                terminal,
                                callback_sp,
                                _lease: lease,
                            });
                            pending_stack.set(true);
                            continue;
                        }
                    } else if let Some(value) = condition_value {
                        value
                    } else if let Some(value) = semaphore_value {
                        value
                    } else if matches!(service, Service::ThreadSelf) {
                        guest_thread
                    } else if matches!(
                        service,
                        Service::SchedPriorityMin | Service::SchedPriorityMax
                    ) {
                        match crate::guest_sched::priority_limit(
                            cpu.registers[7] as u32 as i32,
                            matches!(service, Service::SchedPriorityMax),
                        ) {
                            Ok(value) => value as u64,
                            Err(error) => {
                                let errno = self
                                    .kernel
                                    .as_ref()
                                    .ok_or("Missing scheduling errno context")?
                                    .errno_address();
                                validate_net_output(&memory, errno, 4, sp, ret)?;
                                memory.store(errno, error as u64, 4)?;
                                u32::MAX as u64
                            }
                        }
                    } else if matches!(
                        service,
                        Service::ThreadGetschedparam | Service::ThreadSetschedparam
                    ) {
                        // Caller identity is the retained guest continuation; RDI names
                        // the queried target, which can be a different live guest thread.
                        self.tls.scheduling(guest_thread)?;
                        let target = cpu.registers[7];
                        if matches!(service, Service::ThreadGetschedparam) {
                            let policy = cpu.registers[6];
                            let parameter = cpu.registers[2];
                            if policy != 0
                                && parameter != 0
                                && crate::guest_sched::target_error(&self.tls, target).is_none()
                            {
                                validate_net_output(&memory, policy, 4, sp, ret)?;
                                validate_net_output(&memory, parameter, 4, sp, ret)?;
                            }
                            crate::guest_sched::get(
                                &self.tls,
                                &mut memory,
                                target,
                                policy,
                                parameter,
                            )? as u32 as u64
                        } else {
                            crate::guest_sched::set(
                                &mut self.tls,
                                &memory,
                                target,
                                cpu.registers[6] as u32 as i32,
                                cpu.registers[2],
                            )? as u32 as u64
                        }
                    } else if matches!(service, Service::ThreadSetCancelType) {
                        let kind = cpu.registers[7] as u32 as i32;
                        let previous = cpu.registers[6];
                        // FreeBSD validates the kind before touching oldtype.
                        // No request API can introduce pending cancellation yet.
                        if matches!(kind, 0 | 2) && previous != 0 {
                            validate_net_output(&memory, previous, 4, sp, ret)?;
                        }
                        self.tls
                            .set_cancel_type(guest_thread, kind, previous, &mut memory)?
                            as u32 as u64
                    } else if matches!(service, Service::ThreadSigmask) {
                        let how = cpu.registers[7] as u32 as i32;
                        let input = cpu.registers[6];
                        let output = cpu.registers[2];
                        // libthr copies nonNULL input even for invalid how;
                        // snapshot also preserves exact/partial set-output aliasing.
                        let input = if input == 0 {
                            None
                        } else {
                            Some(
                                memory
                                    .read(input, crate::guest_signal::SIZE)?
                                    .try_into()
                                    .unwrap(),
                            )
                        };
                        let old = self.tls.signal_mask(guest_thread)?;
                        match crate::guest_signal::transition(old, how, input) {
                            Err(errno_value) => {
                                // FreeBSD _pthread_sigmask returns errno without restoring it.
                                let errno = self
                                    .kernel
                                    .as_ref()
                                    .ok_or("Missing signal-mask errno context")?
                                    .errno_address();
                                validate_net_output(&memory, errno, 4, sp, ret)?;
                                memory.store(errno, errno_value.into(), 4)?;
                                errno_value as u64
                            }
                            Ok(mask) => {
                                // Prototype memory faults stop before mutation. This is not
                                // kernel copyout/EFAULT emulation (which may change the mask).
                                if output != 0 {
                                    validate_net_output(
                                        &memory,
                                        output,
                                        crate::guest_signal::SIZE,
                                        sp,
                                        ret,
                                    )?;
                                    memory.write(output, &old)?;
                                }
                                self.tls.replace_signal_mask(guest_thread, mask)?;
                                0
                            }
                        }
                    } else if matches!(service, Service::Sigemptyset | Service::Sigaddset) {
                        let output = cpu.registers[7];
                        let signum = cpu.registers[6] as u32 as i32;
                        if matches!(service, Service::Sigaddset)
                            && !crate::guest_signal::valid(signum)
                        {
                            let errno = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing signal-set errno context")?
                                .errno_address();
                            validate_net_output(&memory, errno, 4, sp, ret)?;
                            memory.store(errno, crate::guest_signal::EINVAL.into(), 4)?;
                            u32::MAX as u64
                        } else {
                            validate_net_output(
                                &memory,
                                output,
                                crate::guest_signal::SIZE,
                                sp,
                                ret,
                            )?;
                            if matches!(service, Service::Sigemptyset) {
                                crate::guest_signal::empty(&mut memory, output)?;
                            } else {
                                crate::guest_signal::add(&mut memory, output, signum)?
                                    .map_err(|_| "Validated signal became invalid")?;
                            }
                            0
                        }
                    } else if matches!(service, Service::ThreadSetName) {
                        self.tls
                            .set_name(&memory, cpu.registers[7], cpu.registers[6])?;
                        // SDK pthread_set_name_np is void, not a pthread errno
                        // result. Preserve the caller's RAX rather than invent one.
                        cpu.registers[0]
                    } else if matches!(service, Service::Getpid | Service::Syscall) {
                        let direct = matches!(service, Service::Getpid);
                        let number = if direct { 20 } else { cpu.registers[0] };
                        if number == 73 {
                            if let Ok(length) =
                                memory.anonymous_release_size(cpu.registers[7], cpu.registers[6])
                            {
                                if retained_overlap(
                                    &self.semaphores,
                                    &self.callback_ranges,
                                    cpu.registers[7],
                                    length,
                                ) {
                                    return Err("Cannot munmap storage retained by a semaphore or comparator".into());
                                }
                            }
                        }
                        if number != 20 && self.kernel.is_none() {
                            return Err(format!(
                                "Unsupported Rust kernel syscall {number} at {:#x}",
                                cpu.rip
                            ));
                        }
                        if direct {
                            cpu.registers[10] = cpu.registers[1];
                        }
                        cpu.registers[11] = cpu.flags;
                        cpu.registers[1] = cpu
                            .target
                            .checked_add(if direct { 12 } else { 2 })
                            .ok_or("Syscall continuation overflow")?;
                        let (value, error) = if number == 20 {
                            (u64::from(std::process::id()), false)
                        } else {
                            self.kernel.as_mut().unwrap().syscall(
                                &mut memory,
                                number,
                                [
                                    cpu.registers[7],
                                    cpu.registers[6],
                                    cpu.registers[2],
                                    cpu.registers[10],
                                    cpu.registers[8],
                                    cpu.registers[9],
                                ],
                            )?
                        };
                        cpu.flags = (cpu.flags & !1) | u64::from(error);
                        value
                    } else if matches!(
                        service,
                        Service::ThreadAttrInit
                            | Service::ThreadAttrDestroy
                            | Service::ThreadAttrSetDetachState
                            | Service::ThreadAttrSetStackSize
                    ) {
                        if memory.load(sp, 8)? != ret {
                            return Err(
                                "Thread attribute return frame changed before commit".into()
                            );
                        }
                        let slot = cpu.registers[7];
                        if matches!(
                            service,
                            Service::ThreadAttrInit | Service::ThreadAttrDestroy
                        ) {
                            validate_net_output(&memory, slot, 8, sp, ret)?;
                        }
                        (match service {
                            Service::ThreadAttrInit => {
                                self.thread_attributes.init(&mut memory, slot)?
                            }
                            Service::ThreadAttrDestroy => {
                                self.thread_attributes.destroy(&mut memory, slot)?
                            }
                            Service::ThreadAttrSetDetachState => self
                                .thread_attributes
                                .set_detach_state(&memory, slot, cpu.registers[6] as u32 as i32)?,
                            Service::ThreadAttrSetStackSize => self
                                .thread_attributes
                                .set_stack_size(&memory, slot, cpu.registers[6])?,
                            _ => unreachable!(),
                        }) as u32 as u64
                    } else if matches!(
                        service,
                        Service::MutexAttrInit
                            | Service::MutexAttrSetType
                            | Service::MutexAttrDestroy
                            | Service::MutexInit
                            | Service::MutexDestroy
                            | Service::MutexLock
                            | Service::MutexTryLock
                            | Service::MutexUnlock
                    ) {
                        if memory.load(sp, 8)? != ret {
                            return Err("Mutex return frame changed before commit".into());
                        }
                        let slot = cpu.registers[7];
                        if matches!(
                            service,
                            Service::MutexAttrInit
                                | Service::MutexAttrDestroy
                                | Service::MutexInit
                                | Service::MutexDestroy
                        ) {
                            validate_net_output(&memory, slot, 8, sp, ret)?;
                        }
                        let status = match service {
                            Service::MutexAttrInit => self.mutex.attr_init(&mut memory, slot)?,
                            Service::MutexAttrSetType => self.mutex.attr_settype(
                                &memory,
                                slot,
                                cpu.registers[6] as u32 as i32,
                            )?,
                            Service::MutexAttrDestroy => {
                                self.mutex.attr_destroy(&mut memory, slot)?
                            }
                            Service::MutexInit => {
                                self.mutex.init(&mut memory, slot, cpu.registers[6])?
                            }
                            Service::MutexDestroy => self.mutex.destroy(&mut memory, slot)?,
                            Service::MutexTryLock => {
                                self.mutex.try_lock(&memory, slot, guest_thread)?
                            }
                            Service::MutexUnlock => {
                                self.mutex.unlock(&memory, slot, guest_thread)?
                            }
                            Service::MutexLock => {
                                match self.mutex.lock(&memory, slot, guest_thread)? {
                                    crate::guest_mutex::LockOutcome::Acquired => 0,
                                    crate::guest_mutex::LockOutcome::Status(status) => status,
                                    crate::guest_mutex::LockOutcome::Pending(ticket) => {
                                        drop(memory);
                                        return Ok(Flow::Finished(Execution::MutexWait(Box::new(
                                            MutexWait {
                                                owner: self.mutex_wait_owner.clone(),
                                                continuation: Some(Continuation {
                                                    callbacks: Some(callbacks),
                                                    stack_lease: stack_lease.clone(),
                                                    quantum: slice.0,
                                                    return_convention,
                                                    guest_thread,
                                                    cpu,
                                                    stack: stack.take(),
                                                    stack_address,
                                                    remaining: remaining - transition - 1,
                                                    caller_bias,
                                                }),
                                                ticket,
                                                ret,
                                                terminal,
                                            },
                                        ))));
                                    }
                                }
                            }
                            _ => unreachable!(),
                        };
                        status as u32 as u64
                    } else if matches!(
                        service,
                        Service::TlsKeyCreate
                            | Service::TlsKeyDelete
                            | Service::TlsGetSpecific
                            | Service::TlsSetSpecific
                    ) {
                        if memory.load(sp, 8)? != ret {
                            return Err("TLS return frame changed before state commit".into());
                        }
                        match service {
                            Service::TlsKeyCreate => {
                                validate_net_output(&memory, cpu.registers[7], 4, sp, ret)?;
                                self.tls.key_create(
                                    &mut memory,
                                    cpu.registers[7],
                                    cpu.registers[6],
                                )? as u32 as u64
                            }
                            Service::TlsKeyDelete => {
                                self.tls.key_delete(cpu.registers[7] as u32 as i32) as u32 as u64
                            }
                            Service::TlsGetSpecific => self
                                .tls
                                .getspecific(guest_thread, cpu.registers[7] as u32 as i32)?,
                            Service::TlsSetSpecific => self.tls.setspecific(
                                guest_thread,
                                cpu.registers[7] as u32 as i32,
                                cpu.registers[6],
                            )? as u32 as u64,
                            _ => unreachable!(),
                        }
                    } else if matches!(service, Service::Fprintf | Service::Fflush) {
                        // Validate the live return frame before a sink commit;
                        // return-PC translation was already checked above.
                        if memory.load(sp, 8)? != ret {
                            return Err("Stdio return frame changed before output".into());
                        }
                        if matches!(service, Service::Fflush) {
                            let stream = cpu.registers[7];
                            if stream == crate::guest_stdio::FILE_ADDR
                                || (stream == 0 && self.stdio.configured())
                            {
                                self.stdio.fflush(&memory, stream)?;
                            }
                            if stream != crate::guest_stdio::FILE_ADDR {
                                if let Some(kernel) = &self.kernel {
                                    self.file_streams.flush(
                                        &kernel.descriptors,
                                        &memory,
                                        stream,
                                    )?;
                                } else if stream != 0 {
                                    return Err("Missing FILE kernel context".into());
                                }
                            }
                            0
                        } else {
                            validate_net_output(
                                &memory,
                                crate::guest_stdio::FILE_ADDR + 172,
                                4,
                                sp,
                                ret,
                            )?;
                            let registers = [
                                cpu.registers[2],
                                cpu.registers[1],
                                cpu.registers[8],
                                cpu.registers[9],
                            ];
                            let mut index = 0usize;
                            let mut next = |memory: &Memory<'_>| -> std::result::Result<u64, crate::strings::Fault> {
                                let current = index;
                                index = index.checked_add(1).ok_or(crate::strings::Fault::Limit)?;
                                if let Some(&value) = registers.get(current) {
                                    return Ok(value);
                                }
                                let offset = u64::try_from(current - registers.len()).map_err(|_| crate::strings::Fault::Read)?
                                    .checked_mul(8).and_then(|offset| offset.checked_add(8)).ok_or(crate::strings::Fault::Read)?;
                                let address = sp.checked_add(offset).ok_or(crate::strings::Fault::Read)?;
                                memory.load(address, 8).map_err(|_| crate::strings::Fault::Read)
                            };
                            self.stdio.fprintf(
                                &mut memory,
                                cpu.registers[7],
                                cpu.registers[6],
                                &mut next,
                            )? as u32 as u64
                        }
                    } else if matches!(service, Service::Puts) {
                        // The registered return target was validated before
                        // dispatch. Validate the live frame before sink commit.
                        if memory.load(sp, 8)? != ret {
                            return Err("Puts return frame changed before output".into());
                        }
                        self.stdout.puts(&memory, cpu.registers[7])? as u32 as u64
                    } else if matches!(service, Service::Setlocale) {
                        if self.locale.setlocale(
                            &memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                        )? {
                            LOCALE_NAME
                        } else {
                            0
                        }
                    } else if matches!(service, Service::Memmove) {
                        if cpu.registers[2] != 0 {
                            validate_net_output(
                                &memory,
                                cpu.registers[7],
                                usize::try_from(cpu.registers[2])
                                    .map_err(|_| "Memmove size overflow")?,
                                sp,
                                ret,
                            )?;
                        }
                        crate::libc_memory::memmove(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )?
                    } else if matches!(service, Service::Memcmp) {
                        crate::libc_memory::memcmp(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )? as u32 as u64
                    } else if matches!(service, Service::Memcpy) {
                        memory.memcpy(cpu.registers[7], cpu.registers[6], cpu.registers[2])?
                    } else if matches!(service, Service::Memset) {
                        if cpu.registers[2] != 0 {
                            validate_net_output(
                                &memory,
                                cpu.registers[7],
                                usize::try_from(cpu.registers[2])
                                    .map_err(|_| "Memset size overflow")?,
                                sp,
                                ret,
                            )?;
                        }
                        crate::libc_memory::memset(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )?
                    } else if matches!(service, Service::Strdup) {
                        strdup_service(
                            &mut memory,
                            cpu.registers[7],
                            self.kernel.as_ref().map(|kernel| kernel.errno_address()),
                            sp,
                            ret,
                        )?
                    } else if matches!(service, Service::Vasprintf) {
                        let mut retained = crate::guest_gdtoa::Retained::default();
                        vasprintf_service_with_hex(
                            &mut memory,
                            [cpu.registers[7], cpu.registers[6], cpu.registers[2]],
                            self.kernel.as_ref().map(|kernel| kernel.errno_address()),
                            sp,
                            ret,
                            Some(crate::printf_render::HexContext { state: gdtoa_state, retained: &mut retained, mxcsr: &mut cpu.mxcsr }),
                        )?
                    } else if matches!(
                        service,
                        Service::Sprintf | Service::Snprintf | Service::Vsnprintf
                    ) {
                        let mode = match service {
                            Service::Sprintf => 0,
                            Service::Snprintf => 1,
                            _ => 2,
                        };
                        let errno = self
                            .kernel
                            .as_ref()
                            .ok_or("Missing formatting errno context")?
                            .errno_address();
                        memory.validate_write(errno, 4)?;
                        match crate::format::formatted(
                            &mut memory,
                            crate::format::Arguments {
                                registers: [
                                    cpu.registers[7],
                                    cpu.registers[6],
                                    cpu.registers[2],
                                    cpu.registers[1],
                                    cpu.registers[8],
                                    cpu.registers[9],
                                ],
                                stack: sp,
                            },
                            mode,
                        ) {
                            Ok(value) => value,
                            Err(fault) => {
                                let error = match fault {
                                    crate::strings::Fault::Allocation => 12,
                                    crate::strings::Fault::Limit => 22,
                                    _ => 14,
                                };
                                memory.store(errno, error, 4)?;
                                u32::MAX.into()
                            }
                        }
                    } else if matches!(service, Service::Setenv) {
                        'setenv: {
                            let errno = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing setenv errno context")?
                                .errno_address();
                            let inputs = crate::environment::setenv_inputs(
                                &memory,
                                cpu.registers[7],
                                cpu.registers[6],
                            )
                            .map_err(|e| format!("Guest setenv input fault {e:?}"))?;
                            validate_net_output(&memory, errno, 4, sp, ret)?;
                            memory.validate_write(errno, 4)?;
                            match inputs {
                                Err(error) => {
                                    memory.store(errno, error.errno() as u64, 4)?;
                                    u32::MAX as u64
                                }
                                Ok((name, value)) => {
                                    let mut imported = if has_owned_environment {
                                        None
                                    } else {
                                        match crate::environment::Environment::import(
                                            &memory,
                                            self.environment,
                                            0x7ffe00000000,
                                            self.environment_budget,
                                        ) {
                                            Ok(environment) => Some(environment),
                                            Err(crate::strings::Fault::Allocation) => {
                                                memory.store(errno, 12, 4)?;
                                                break 'setenv u32::MAX as u64;
                                            }
                                            Err(error) => {
                                                return Err(format!(
                                                    "Guest environment import fault {error:?}"
                                                ));
                                            }
                                        }
                                    };
                                    if let Some(environment) = imported.as_mut() {
                                        // Reserve the entire future arena now, before publishing
                                        // storage or changing metadata, including service collisions.
                                        for (address, bytes) in environment.mappings() {
                                            if !bytes.is_empty() {
                                                memory.validate_unmapped(address, bytes.len())?;
                                            }
                                        }
                                    }
                                    // Complete the call exactly once before remapping: no repeat
                                    // stack push, service charge, or input pointer dereference.
                                    cpu.registers[4] =
                                        sp.checked_add(8).ok_or("Setenv stack overflow")?;
                                    cpu.rip = cpu.next_rip;
                                    cpu.stop = 0;
                                    if terminal {
                                        cpu.registers[4] = sp;
                                        cpu.stop = 3;
                                    }
                                    cpu.memory = 0;
                                    let overwrite = cpu.registers[2] as u32 as i32;
                                    drop(memory);
                                    return Ok(Flow::EnvironmentSet {
                                        continuation: Box::new(Continuation {
                                            callbacks: Some(callbacks),
                                            stack_lease: stack_lease.clone(),
                                            quantum: slice.0,
                                            return_convention,
                                            guest_thread,
                                            cpu,
                                            stack: stack.take(),
                                            stack_address,
                                            remaining: remaining - transition - 1,
                                            caller_bias,
                                        }),
                                        imported,
                                        name,
                                        value,
                                        overwrite,
                                        errno,
                                        terminal,
                                    });
                                }
                            }
                        }
                    } else if matches!(
                        service,
                        Service::Atoi
                            | Service::Strtol
                            | Service::Strtoul
                            | Service::Strtoll
                            | Service::Strtoull
                    ) {
                        // GuestLocale currently admits only C/POSIX. LP64 long
                        // and long long share the same64-bit parser; atoi follows
                        // FreeBSD's strtol(base10) then int cast, including errno.
                        let atoi = matches!(service, Service::Atoi);
                        let source = cpu.registers[7];
                        let endptr = if atoi { 0 } else { cpu.registers[6] };
                        let parsed = crate::integer_parse::parse(
                            &memory,
                            source,
                            if atoi {
                                10
                            } else {
                                cpu.registers[2] as u32 as i32
                            },
                            !matches!(service, Service::Strtoul | Service::Strtoull),
                        )?;
                        let end = source
                            .checked_add(parsed.end_offset)
                            .ok_or("Integer end pointer overflow")?;
                        // Both outputs are preguarded before either changes. The
                        // successful write order remains errno then endptr, even
                        // if their mappings alias, matching FreeBSD11.4.
                        let errno = if parsed.errno.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing integer conversion errno context")?
                                .errno_address();
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        if endptr != 0 {
                            validate_net_output(&memory, endptr, 8, sp, ret)?;
                            memory.validate_write(endptr, 8)?;
                        }
                        if let Some(address) = errno {
                            memory.store(
                                address,
                                parsed.errno.expect("Error output validated") as u64,
                                4,
                            )?;
                        }
                        if endptr != 0 {
                            memory.store(endptr, end, 8)?;
                        }
                        if atoi {
                            parsed.bits as u32 as u64
                        } else {
                            parsed.bits
                        }
                    } else if matches!(service, Service::Nanosleep) {
                        // FreeBSD validates nsec first, then negative seconds and
                        // zero duration complete immediately. Neither success path
                        // accesses the optional remaining pointer. No signal/EINTR
                        // delivery is fabricated by this bounded implementation.
                        if matches!(
                            sleep_request,
                            Some(crate::guest_sleep::Request::InvalidNanoseconds)
                        ) {
                            let errno = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing nanosleep errno context")?
                                .errno_address();
                            validate_net_output(&memory, errno, 4, sp, ret)?;
                            memory.validate_write(errno, 4)?;
                            memory.store(errno, 22, 4)?;
                            u32::MAX as u64
                        } else {
                            0
                        }
                    } else if matches!(
                        service,
                        Service::Mbrtowc | Service::Wcrtomb | Service::Mbsrtowcs
                    ) {
                        let args = [
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                            cpu.registers[1],
                        ];
                        let plan = match service {
                            Service::Mbrtowc => crate::guest_multibyte::mbrtowc(
                                &memory, args[0], args[1], args[2], args[3],
                            )?,
                            Service::Wcrtomb => {
                                crate::guest_multibyte::wcrtomb(args[0], args[1], args[2])?
                            }
                            Service::Mbsrtowcs => crate::guest_multibyte::mbsrtowcs(
                                &memory, args[0], args[1], args[2], args[3],
                            )?,
                            _ => unreachable!(),
                        };
                        multibyte_service(
                            &mut memory,
                            plan,
                            self.kernel.as_ref().map(|k| k.errno_address()),
                            sp,
                            ret,
                        )?
                    } else if matches!(service, Service::Wcstombs) {
                        // GuestLocale currently supports C/POSIX only. Conversion
                        // is stateless and uses guest unsigned16 code units.
                        let plan = crate::guest_wcstombs::plan(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )?;
                        if !plan.bytes.is_empty() {
                            validate_net_output(
                                &memory,
                                plan.destination,
                                plan.bytes.len(),
                                sp,
                                ret,
                            )?;
                            memory.validate_write(plan.destination, plan.bytes.len())?;
                        }
                        let errno = if plan.errno.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing wcstombs errno context")?
                                .errno_address();
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        plan.commit(&mut memory)?;
                        if let (Some(address), Some(error)) = (errno, plan.errno) {
                            memory.store(address, error as u64, 4)?;
                        }
                        plan.value
                    } else if matches!(service, Service::Sigaction | Service::Signal) {
                        let signum = cpu.registers[7] as u32 as i32;
                        let plan = if matches!(service, Service::Sigaction) {
                            self.signal_actions.plan(
                                &memory,
                                signum,
                                cpu.registers[6],
                                cpu.registers[2],
                            )?
                        } else {
                            self.signal_actions.plan_signal(signum, cpu.registers[6])
                        };
                        if let Some((address, length)) = plan.output_range() {
                            validate_net_output(&memory, address, length, sp, ret)?;
                            memory.validate_write(address, length)?;
                        }
                        let error = plan.errno();
                        let errno = if error.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing signal action errno context")?
                                .errno_address();
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        let value = self.signal_actions.commit(&mut memory, plan)?;
                        if let (Some(address), Some(error)) = (errno, error) {
                            memory.store(address, error as u64, 4)?;
                        }
                        value
                    } else if matches!(service, Service::Sysctl | Service::SysctlByName) {
                        let plan = if matches!(service, Service::Sysctl) {
                            crate::guest_sysctl::plan_numeric(
                                &memory,
                                self.system_profile.as_ref(),
                                [
                                    cpu.registers[7],
                                    cpu.registers[6],
                                    cpu.registers[2],
                                    cpu.registers[1],
                                    cpu.registers[8],
                                    cpu.registers[9],
                                ],
                            )?
                        } else {
                            crate::guest_sysctl::plan_named(
                                &memory,
                                self.system_profile.as_ref(),
                                [
                                    cpu.registers[7],
                                    cpu.registers[6],
                                    cpu.registers[2],
                                    cpu.registers[1],
                                    cpu.registers[8],
                                ],
                            )?
                        };
                        for write in &plan.writes {
                            validate_net_output(
                                &memory,
                                write.address,
                                write.bytes.len(),
                                sp,
                                ret,
                            )?;
                            memory.validate_write(write.address, write.bytes.len())?;
                        }
                        let errno = if plan.errno.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing sysctl errno context")?
                                .errno_address();
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        plan.commit(&mut memory)?;
                        if let (Some(address), Some(error)) = (errno, plan.errno) {
                            memory.store(address, error as u64, 4)?;
                        }
                        plan.return_value()
                    } else if matches!(service, Service::Sysconf) {
                        let selector = cpu.registers[7] as u32 as i32;
                        let query = match self.system_profile.as_ref() {
                            Some(profile) => profile.query(selector),
                            None => crate::guest_system::classify_unimplemented(selector),
                        };
                        match query {
                            crate::guest_system::Query::Value(value) => value as u64,
                            crate::guest_system::Query::Invalid => {
                                let errno = self
                                    .kernel
                                    .as_ref()
                                    .ok_or("Missing sysconf errno context")?
                                    .errno_address();
                                validate_net_output(&memory, errno, 4, sp, ret)?;
                                memory.validate_write(errno, 4)?;
                                memory.store(errno, 22, 4)?;
                                u64::MAX
                            }
                            crate::guest_system::Query::Unsupported => {
                                return Err(format!(
                                    "Unsupported sysconf selector {selector} or missing explicit guest system profile"
                                ));
                            }
                        }
                    } else if matches!(service, Service::Stat) {
                        let kernel = self.kernel.as_mut().ok_or("Missing stat kernel context")?;
                        validate_net_output(&memory, kernel.errno_address(), 4, sp, ret)?;
                        // Preserve pathname-error ordering: protect the destination
                        // only when the kernel would actually write a stat record.
                        // Guest execution cannot mutate the path between these reads.
                        if kernel.stat_will_write(&memory, cpu.registers[7]) {
                            validate_net_output(&memory, cpu.registers[6], 120, sp, ret)?;
                        }
                        let (value, _) = kernel.syscall(
                            &mut memory,
                            188,
                            [cpu.registers[7], cpu.registers[6], 0, 0, 0, 0],
                        )?;
                        value as u32 as u64
                    } else if matches!(service, Service::Fstat) {
                        let kernel = self.kernel.as_mut().ok_or("Missing fstat kernel context")?;
                        validate_net_output(&memory, kernel.errno_address(), 4, sp, ret)?;
                        let descriptor = cpu.registers[7] as u32 as u64;
                        if matches!(
                            kernel.descriptors.kind(descriptor),
                            Some(crate::descriptors::DescriptorKind::RegisteredFile)
                        ) {
                            validate_net_output(&memory, cpu.registers[6], 120, sp, ret)?;
                        }
                        let (value, _) = kernel.syscall(
                            &mut memory,
                            189,
                            [descriptor, cpu.registers[6], 0, 0, 0, 0],
                        )?;
                        value as u32 as u64
                    } else if matches!(service, Service::Fgets) {
                        self.file_streams.call(
                            self.kernel.as_mut().ok_or("Missing fgets kernel context")?,
                            &mut memory,
                            crate::stdio_file::Operation::ReadLine,
                            [
                                cpu.registers[7],
                                cpu.registers[6] as u32 as u64,
                                cpu.registers[2],
                                0,
                            ],
                            |memory, address, length| {
                                validate_net_output(memory, address, length, sp, ret)
                            },
                        )?
                    } else if let Service::File(operation) = service {
                        if matches!(operation, crate::stdio_file::Operation::Write)
                            && cpu.registers[1] == crate::guest_stdio::FILE_ADDR
                        {
                            self.stdio.fwrite(
                                &mut memory,
                                cpu.registers[7],
                                cpu.registers[6],
                                cpu.registers[2],
                                self.kernel
                                    .as_ref()
                                    .ok_or("Missing fwrite errno context")?
                                    .errno_address(),
                                |memory, address, length| {
                                    validate_net_output(memory, address, length, sp, ret)
                                },
                            )?
                        } else {
                            self.file_streams.call(
                                self.kernel.as_mut().ok_or("Missing FILE kernel context")?,
                                &mut memory,
                                *operation,
                                [
                                    cpu.registers[7],
                                    cpu.registers[6],
                                    cpu.registers[2],
                                    cpu.registers[1],
                                ],
                                |memory, address, length| {
                                    validate_net_output(memory, address, length, sp, ret)
                                },
                            )?
                        }
                    } else if matches!(service, Service::Strftime) {
                        let timezone = if self.guest_time.utc_configured() {
                            crate::guest_strftime::Timezone::Utc
                        } else {
                            crate::guest_strftime::Timezone::Unconfigured
                        };
                        let plan = crate::guest_strftime::plan(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                            cpu.registers[1],
                            crate::guest_strftime::Policy { timezone },
                        ).map_err(|fault| format!("Guest strftime fault: {fault:?}"))?;
                        if !plan.bytes.is_empty() {
                            validate_net_output(&memory, plan.destination, plan.bytes.len(), sp, ret)?;
                        }
                        plan.commit(&mut memory).map_err(|fault| format!("Guest strftime output fault: {fault:?}"))?;
                        match plan.completion {
                            crate::guest_strftime::Completion::Returned(value) => value,
                            crate::guest_strftime::Completion::Unsupported(reason) => {
                                return Err(format!("Guest strftime unsupported: {reason}"));
                            }
                        }
                    } else if matches!(service, Service::Gettimeofday) {
                        let plan = self.guest_time.plan(cpu.registers[7], cpu.registers[6])?;
                        for write in &plan.writes {
                            validate_net_output(
                                &memory,
                                write.address,
                                write.bytes.len(),
                                sp,
                                ret,
                            )?;
                            memory.validate_write(write.address, write.bytes.len())?;
                        }
                        // Valid aliases observe FreeBSD's timeval-then-timezone
                        // ordering. Invalid mappings stop before any output write.
                        for write in &plan.writes {
                            memory.write(write.address, &write.bytes)?;
                        }
                        0
                    } else if matches!(service, Service::Vsscanf) {
                        let plan = crate::scan::guest_plan_with_rounding(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                            crate::float_service::rounding(&cpu).ok(),
                        )?;
                        for write in &plan.writes {
                            validate_net_output(
                                &memory,
                                write.address,
                                write.bytes.len(),
                                sp,
                                ret,
                            )?;
                            memory.validate_write(write.address, write.bytes.len())?;
                        }
                        let errno = if plan.errno.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing scanf errno context")?
                                .errno_address();
                            let end = address
                                .checked_add(4)
                                .ok_or("Scanf errno address overflow")?;
                            for (start, length) in plan.protected_ranges.iter().copied().chain(
                                plan.writes
                                    .iter()
                                    .map(|write| (write.address, write.bytes.len())),
                            ) {
                                let other_end = start
                                    .checked_add(length as u64)
                                    .ok_or("Scanf protected range overflow")?;
                                if address < other_end && start < end {
                                    return Err(
                                        "Scanf errno aliases input, arguments or output".into()
                                    );
                                }
                            }
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        // Ordinary matching failure retains successful assignments
                        // in this plan. Guard failures commit nothing; sequential
                        // output aliases retain the plan's original write order.
                        for write in &plan.writes {
                            memory.write(write.address, &write.bytes)?;
                        }
                        if let Some(address) = errno {
                            memory.store(
                                address,
                                plan.errno.expect("Validated errno") as u64,
                                4,
                            )?;
                        }
                        cpu.mxcsr |= plan.mxcsr_flags;
                        plan.result as u32 as u64
                    } else if matches!(service, Service::Atof | Service::Strtod | Service::Strtof | Service::Strtold) {
                        let rounding = crate::float_service::rounding(&cpu)?;
                        let source = cpu.registers[7];
                        let input = crate::float_service::input(&memory, source)?;
                        let (bits, end_offset, errno_code, flags, width, extended) = if matches!(service, Service::Strtold) {
                            let effect = crate::float_parse::strtold_effects::parse_effects(&input, rounding)
                                .map_err(|error| format!("Unsupported strtold conversion: {error:?}"))?;
                            (0, effect.end, effect.errno.map(|code| code as i32), effect.mxcsr_flags, 0, Some(effect.bytes))
                        } else if matches!(service, Service::Strtof) {
                            let effect = crate::float_parse::strtof_effects::parse_effects(&input, rounding)
                                .map_err(|error| format!("Unsupported strtof conversion: {error:?}"))?;
                            (u64::from(effect.bits), effect.end, effect.errno.map(|code| code as i32), effect.mxcsr_flags, 4, None)
                        } else {
                            let effect = crate::float_parse::effects::parse_effects(&input, rounding)
                                .map_err(|error| format!("Unsupported float conversion: {error:?}"))?;
                            (effect.parsed.bits, effect.parsed.end, effect.parsed.errno, effect.mxcsr_flags, 8, None)
                        };
                        let endptr = if matches!(service, Service::Atof) {
                            0
                        } else {
                            cpu.registers[6]
                        };
                        let end = source
                            .checked_add(end_offset as u64)
                            .ok_or("Float end pointer overflow")?;
                        let errno = if errno_code.is_some() {
                            let address = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing float conversion errno context")?
                                .errno_address();
                            validate_net_output(&memory, address, 4, sp, ret)?;
                            memory.validate_write(address, 4)?;
                            Some(address)
                        } else {
                            None
                        };
                        if endptr != 0 {
                            validate_net_output(&memory, endptr, 8, sp, ret)?;
                            memory.validate_write(endptr, 8)?;
                        }
                        let x87_return = if let Some(value) = extended {
                            let mut stack = cpu.x87.clone();
                            let mut status = cpu.x87_status;
                            stack.push_raw(cpu.x87_control, &mut status, value)
                                .map_err(|error| format!("Unsupported strtold x87 return: {error:?}"))?;
                            Some((stack, status))
                        } else {
                            None
                        };
                        // All outputs are guarded before errno/endptr or FP state
                        // changes. Source was snapshotted, so output aliases are safe.
                        if let Some(address) = errno {
                            memory.store(
                                address,
                                errno_code.expect("Validated errno") as u32 as u64,
                                4,
                            )?;
                        }
                        if endptr != 0 {
                            memory.store(endptr, end, 8)?;
                        }
                        cpu.mxcsr |= flags;
                        if let Some((stack, status)) = x87_return {
                            cpu.x87 = stack;
                            cpu.x87_status = status;
                        } else {
                            cpu.vectors[0][..width].copy_from_slice(&bits.to_le_bytes()[..width]);
                        }
                        // SysV long double returns in ST(0), float/double in XMM0.
                        cpu.registers[0]
                    } else if matches!(service, Service::Strlcpy | Service::Strlcat) {
                        let plan = crate::narrow_strings::copy_plan(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                            matches!(service, Service::Strlcat),
                        )?;
                        if !plan.bytes.is_empty() {
                            validate_net_output(
                                &memory,
                                plan.destination,
                                plan.bytes.len(),
                                sp,
                                ret,
                            )?;
                        }
                        plan.commit(&mut memory)?
                    } else if matches!(service, Service::Strchr | Service::Strrchr) {
                        crate::narrow_strings::strchr(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            matches!(service, Service::Strrchr),
                        )?
                    } else if matches!(service, Service::Strstr) {
                        crate::narrow_strings::strstr(&memory, cpu.registers[7], cpu.registers[6])?
                    } else if let Service::Ctype(operation) = service {
                        // C int consumes the low32 bits; EOF is signed -1.
                        // Out-of-domain values stop explicitly. No errno writes.
                        crate::c_locale::apply(*operation, cpu.registers[7] as u32 as i32)? as u32
                            as u64
                    } else if matches!(service, Service::Strncasecmp) {
                        crate::c_locale::strncasecmp(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )? as u32 as u64
                    } else if matches!(service, Service::Wcslen) {
                        crate::wide_string::wcslen(&memory, cpu.registers[7])?
                    } else if matches!(service, Service::Wcsstr) {
                        crate::wide_string::wcsstr(&memory, cpu.registers[7], cpu.registers[6])?
                    } else if matches!(service, Service::Wcscmp | Service::Wcscoll) {
                        // C/POSIX wcscoll delegates to wcscmp. Use the guest's
                        // unsigned16 wchar profile, not host FreeBSD wchar32.
                        crate::wide_string::wcscmp(&memory, cpu.registers[7], cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::Wcsncmp) {
                        crate::wide_string::wcsncmp(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )? as u32 as u64
                    } else if matches!(service, Service::Mkdir) {
                        let kernel = self.kernel.as_mut().ok_or("Missing mkdir kernel context")?;
                        // Snapshot and resolve the pathname before publishing errno.
                        // Namespace preparation/commit errors do not create a node.
                        let outcome = match kernel.plan_mkdir(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6] as u16,
                        )? {
                            Ok(plan) => kernel.commit_mkdir(plan),
                            Err(errno) => Err(errno),
                        };
                        match outcome {
                            Ok(()) => 0,
                            Err(errno) => {
                                let address = kernel.errno_address();
                                validate_net_output(&memory, address, 4, sp, ret)?;
                                memory.store(address, errno as u64, 4)?;
                                u32::MAX as u64
                            }
                        }
                    } else if matches!(service, Service::Getcwd) {
                        crate::guest_getcwd::call(
                            &mut memory,
                            self.kernel
                                .as_ref()
                                .ok_or("Missing getcwd kernel context")?,
                            cpu.registers[7],
                            cpu.registers[6],
                            |memory, address, length| {
                                validate_net_output(memory, address, length, sp, ret)
                            },
                        )?
                    } else if matches!(service, Service::Getenv) {
                        crate::environment::getenv(&memory, self.environment, cpu.registers[7])
                            .map_err(|fault| format!("Guest getenv fault {fault:?}"))?
                    } else if matches!(service, Service::Realloc) {
                        let errno = self
                            .kernel
                            .as_ref()
                            .ok_or("Missing realloc errno context")?
                            .errno_address();
                        memory.validate_write(errno, 4)?;
                        validate_net_output(&memory, errno, 4, sp, ret)?;
                        if cpu.registers[7] != 0 {
                            // Realloc releases the old object on success. It
                            // must not revoke its own live guest return frame.
                            let length = memory.heap_allocation_size(cpu.registers[7])?;
                            if retained_overlap(
                                &self.semaphores,
                                &self.callback_ranges,
                                cpu.registers[7],
                                length,
                            ) {
                                return Err(
                                    "Cannot realloc storage retained by a semaphore or comparator"
                                        .into(),
                                );
                            }
                            validate_net_output(&memory, cpu.registers[7], length, sp, ret)?;
                        }
                        match crate::libc_memory::realloc(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                        )? {
                            Ok(address) => address,
                            Err(error) => {
                                memory.store(errno, error.into(), 4)?;
                                0
                            }
                        }
                    } else if matches!(service, Service::Malloc | Service::Calloc | Service::Free) {
                        if matches!(service, Service::Free) {
                            if cpu.registers[7] != 0
                                && retained_overlap(
                                    &self.semaphores,
                                    &self.callback_ranges,
                                    cpu.registers[7],
                                    memory.heap_allocation_size(cpu.registers[7])?,
                                )
                            {
                                return Err(
                                    "Cannot free storage retained by a semaphore or comparator"
                                        .into(),
                                );
                            }
                            memory.free(cpu.registers[7])?;
                            0
                        } else {
                            let errno = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing heap errno context")?
                                .errno_address();
                            memory.validate_write(errno, 4)?;
                            let result = if matches!(service, Service::Calloc) {
                                memory.calloc(cpu.registers[7], cpu.registers[6])
                            } else {
                                memory.malloc(cpu.registers[7])
                            };
                            match result {
                                Ok(address) => address,
                                Err(error) => {
                                    memory.store(errno, error.into(), 4)?;
                                    0
                                }
                            }
                        }
                    } else if matches!(service, Service::NetErrnoLoc) {
                        NET_ERRNO
                    } else if matches!(service, Service::NetPoolCreate) {
                        self.network.pool_create(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                        )? as u32 as u64
                    } else if matches!(service, Service::NetPoolDestroy) {
                        self.network.pool_destroy(cpu.registers[7] as i32)? as u32 as u64
                    } else if matches!(service, Service::NetResolverCreate) {
                        self.network.resolver_create(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                        )? as u32 as u64
                    } else if matches!(service, Service::NetResolverDestroy) {
                        self.network.resolver_destroy(cpu.registers[7] as i32)? as u32 as u64
                    } else if matches!(service, Service::NetResolverStartNtoa) {
                        // The common dispatch already validated the executable
                        // registered return target, or the terminal sentinel.
                        validate_net_output(&memory, cpu.registers[2], 4, sp, ret)?;
                        self.network.start_ntoa(
                            &mut memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                            cpu.registers[2],
                            cpu.registers[1] as i32,
                            cpu.registers[8] as i32,
                            cpu.registers[9] as i32,
                        )? as u32 as u64
                    } else if matches!(service, Service::NetResolverStartAton) {
                        let flags = memory.load(
                            sp.checked_add(8).ok_or("Network argument stack overflow")?,
                            8,
                        )? as i32;
                        let capacity = cpu.registers[1] as i32;
                        if !(1..=1025).contains(&capacity) {
                            return Err("Invalid reverse resolver capacity".into());
                        }
                        validate_net_output(&memory, cpu.registers[2], capacity as usize, sp, ret)?;
                        self.network.start_aton(
                            &mut memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                            cpu.registers[2],
                            capacity,
                            cpu.registers[8] as i32,
                            cpu.registers[9] as i32,
                            flags,
                        )? as u32 as u64
                    } else if matches!(service, Service::ImeInitialize) {
                        self.ime_dialog
                            .initialize(&memory, cpu.registers[7], cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::ImeGetStatus) {
                        self.ime_dialog.get_status()? as u32 as u64
                    } else if matches!(service, Service::ImeGetResult) {
                        let end = sp.checked_add(8).ok_or("IME return frame overflow")?;
                        if memory.load(sp, 8)? != ret {
                            return Err("IME return frame changed before result delivery".into());
                        }
                        for (address, length) in
                            self.ime_dialog.result_output_ranges(cpu.registers[7])?
                        {
                            let output_end = address
                                .checked_add(length as u64)
                                .ok_or("IME output overflow")?;
                            if address < end && sp < output_end {
                                return Err("IME output overlaps its live return frame".into());
                            }
                        }
                        self.ime_dialog.get_result(&mut memory, cpu.registers[7])? as u32 as u64
                    } else if matches!(service, Service::ImeTerminate) {
                        self.ime_dialog.terminate()? as u32 as u64
                    } else if matches!(service, Service::KeyboardInitialize) {
                        self.keyboard.initialize()? as u32 as u64
                    } else if matches!(service, Service::KeyboardOpen) {
                        self.keyboard.open(
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                        )? as u32 as u64
                    } else if matches!(service, Service::KeyboardReadState) {
                        let output = cpu.registers[6];
                        let output_end =
                            output.checked_add(96).ok_or("Keyboard output overflow")?;
                        let frame_end =
                            sp.checked_add(8).ok_or("Keyboard return frame overflow")?;
                        if memory.load(sp, 8)? != ret || (output < frame_end && sp < output_end) {
                            return Err(
                                "Keyboard output overlaps or changes its live return frame".into(),
                            );
                        }
                        self.keyboard.read_state(
                            &mut memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                        )? as u32 as u64
                    } else if matches!(service, Service::KeyboardClose) {
                        self.keyboard.close(cpu.registers[7] as i32)? as u32 as u64
                    } else if matches!(service, Service::VideoOpen) {
                        self.video_out.open(
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                        )? as u32 as u64
                    } else if matches!(service, Service::VideoClose) {
                        self.video_out
                            .close(&mut self.direct_memory, cpu.registers[7] as i32)?;
                        // The SDK declares close/set-attribute void. Completing
                        // their HLE thunk with RAX=0 is our explicit convention.
                        0
                    } else if matches!(service, Service::VideoAddFlipEvent) {
                        self.video_out.add_flip_event(
                            &self.equeues,
                            cpu.registers[7],
                            cpu.registers[6] as i32,
                            cpu.registers[2],
                        )? as u32 as u64
                    } else if matches!(service, Service::VideoDeleteFlipEvent) {
                        self.video_out
                            .delete_flip_event(cpu.registers[7], cpu.registers[6] as i32)?
                            as u32 as u64
                    } else if matches!(service, Service::VideoSetFlipRate) {
                        self.video_out
                            .set_flip_rate(cpu.registers[7] as i32, cpu.registers[6] as i32)?
                            as u32 as u64
                    } else if matches!(service, Service::VideoSetBufferAttribute) {
                        let (seventh, eighth) = video_stack_arguments(&memory, sp)?;
                        // Registered executable return targets (or the terminal
                        // sentinel) were checked before entering service dispatch.
                        validate_video_attribute_return(&memory, cpu.registers[7], sp, ret)?;
                        self.video_out.set_buffer_attribute(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2] as u32,
                            cpu.registers[1] as u32,
                            cpu.registers[8] as u32,
                            cpu.registers[9],
                            seventh as u32,
                            eighth,
                        )?;
                        0
                    } else if matches!(service, Service::VideoRegisterBuffers) {
                        let (seventh, eighth) = video_stack_arguments(&memory, sp)?;
                        self.video_out.register_buffers(
                            &memory,
                            &mut self.direct_memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                            cpu.registers[8] as i32,
                            cpu.registers[9],
                            seventh as i32,
                            eighth,
                        )? as u32 as u64
                    } else if matches!(service, Service::VideoSubmitFlip) {
                        self.video_out.submit_flip(
                            &memory,
                            &mut self.equeues,
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as u32,
                            cpu.registers[1] as i64,
                        )? as u32 as u64
                    } else if matches!(service, Service::EqueueCreate) {
                        self.equeues
                            .create(&mut memory, cpu.registers[7], cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::EqueueDelete) {
                        self.equeues.delete(cpu.registers[7])? as u32 as u64
                    } else if matches!(service, Service::EqueueWait) {
                        let registered_return = terminal
                            || owner(ret).is_ok_and(|bias| {
                                ret.checked_sub(bias)
                                    .is_some_and(|offset| block_at(bias, offset).is_some())
                            });
                        validate_equeue_return(
                            &memory,
                            (cpu.registers[6], cpu.registers[2] as i32, cpu.registers[1]),
                            (sp, ret),
                            terminal,
                            registered_return,
                        )?;
                        match self.equeues.wait(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                            cpu.registers[8],
                        )? {
                            crate::equeue::WaitOutcome::Ready(value) => value as u32 as u64,
                            // Existing resume APIs complete a yielded service with
                            // a caller-supplied result. An event wait needs its own
                            // continuation/revalidation before it can use those
                            // semantics; never turn Pending into successful zero.
                            crate::equeue::WaitOutcome::Pending => {
                                return Err(
                                    "sceKernelWaitEqueue: empty queue requires event suspension"
                                        .into(),
                                );
                            }
                        }
                    } else if matches!(service, Service::DirectAllocate) {
                        self.direct_memory.allocate(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                        )? as u32 as u64
                    } else if matches!(service, Service::DirectMap) {
                        self.direct_memory.map(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2] as i32,
                            cpu.registers[1] as i32,
                            cpu.registers[8],
                            cpu.registers[9],
                        )? as u32 as u64
                    } else if matches!(service, Service::DirectRelease) {
                        if let Some((address, length)) = self
                            .direct_memory
                            .release_mapping(cpu.registers[7], cpu.registers[6])?
                        {
                            if retained_overlap(
                                &self.semaphores,
                                &self.callback_ranges,
                                address,
                                length,
                            ) {
                                return Err("Cannot unmap a pending semaphore object".into());
                            }
                        }
                        self.direct_memory.release(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                        )? as u32 as u64
                    } else if matches!(service, Service::AudioInitialize) {
                        self.audio.initialize()? as u32 as u64
                    } else if matches!(service, Service::AudioOpen) {
                        self.audio.open(
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                            cpu.registers[1] as u32,
                            cpu.registers[8] as u32,
                            cpu.registers[9] as u32,
                        )? as u32 as u64
                    } else if matches!(service, Service::AudioOutput) {
                        self.audio
                            .output(&memory, cpu.registers[7] as i32, cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::AudioClose) {
                        self.audio.close(cpu.registers[7] as i32)? as u32 as u64
                    } else if matches!(service, Service::PadInitialize) {
                        self.pad.initialize()? as u32 as u64
                    } else if matches!(service, Service::PadOpen) {
                        self.pad.open(
                            cpu.registers[7] as i32,
                            cpu.registers[6] as i32,
                            cpu.registers[2] as i32,
                            cpu.registers[1],
                        )? as u32 as u64
                    } else if matches!(service, Service::PadClose) {
                        self.pad.close(cpu.registers[7] as i32)? as u32 as u64
                    } else if matches!(service, Service::PadReadState) {
                        self.pad.read_state(
                            &mut memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                        )? as u32 as u64
                    } else if matches!(service, Service::PadSetLightBar) {
                        self.pad
                            .set_light_bar(cpu.registers[7] as i32, cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::PadSetVibration) {
                        self.pad
                            .set_vibration(cpu.registers[7] as i32, cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::PadSetVibrationMode) {
                        self.pad
                            .set_vibration_mode(cpu.registers[7] as i32, cpu.registers[6] as i32)?
                            as u32 as u64
                    } else if matches!(service, Service::UserInitialize) {
                        self.user_service.initialize(cpu.registers[7])? as u32 as u64
                    } else if matches!(service, Service::UserGetForeground) {
                        self.user_service
                            .get_foreground_user(&mut memory, cpu.registers[7])?
                            as u32 as u64
                    } else if matches!(service, Service::UserGetLoginList) {
                        self.user_service
                            .get_login_user_id_list(&mut memory, cpu.registers[7])?
                            as u32 as u64
                    } else if matches!(service, Service::UserGetName) {
                        self.user_service.get_user_name(
                            &mut memory,
                            cpu.registers[7] as i32,
                            cpu.registers[6],
                            cpu.registers[2],
                        )? as u32 as u64
                    } else if matches!(service, Service::SystemHideSplashScreen) {
                        self.system_service.hide_splash_screen() as u32 as u64
                    } else if matches!(service, Service::SystemLaunchWebBrowser) {
                        self.system_service
                            .launch_web_browser(cpu.registers[7], cpu.registers[6])?
                            as u32 as u64
                    } else if matches!(service, Service::Getargc | Service::Getargv) {
                        let (count, address) = self
                            .process_arguments
                            .ok_or("Guest process arguments are not configured")?;
                        if count > 4096 || address == 0 || address & 7 != 0 {
                            return Err("Invalid guest process argument table".into());
                        }
                        let table = memory.read(address, (count as usize + 1) * 8)?;
                        if table[count as usize * 8..] != [0; 8]
                            || table[..count as usize * 8]
                                .chunks_exact(8)
                                .any(|p| p == [0; 8])
                        {
                            return Err("Malformed guest process argument table".into());
                        }
                        if matches!(service, Service::Getargc) {
                            u64::from(count)
                        } else {
                            address
                        }
                    } else if matches!(service, Service::Strerror | Service::InternalStrerror) {
                        if matches!(service, Service::InternalStrerror) && cpu.registers[6] != 0 {
                            return Err("Non-null _Strerror buffer is unsupported".into());
                        }
                        let (mut text, known) =
                            crate::error_text::describe(cpu.registers[7] as i32);
                        if !known && matches!(service, Service::Strerror) {
                            let errno = self
                                .kernel
                                .as_ref()
                                .ok_or("Missing strerror errno context")?
                                .errno_address();
                            memory.store(errno, 22, 4)?;
                        }
                        text.push('\0');
                        memory.update_service_data(ERROR_TEXT, text.as_bytes())?;
                        ERROR_TEXT
                    } else if matches!(service, Service::Strxfrm) {
                        let plan = crate::guest_strxfrm::plan(
                            &memory,
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )?;
                        if !plan.bytes.is_empty() {
                            validate_net_output(&memory, plan.destination, plan.bytes.len(), sp, ret)?;
                        }
                        plan.commit(&mut memory)?;
                        plan.value
                    } else if matches!(service, Service::Strcoll) {
                        crate::guest_strcoll::compare_c(
                            &mut memory,
                            cpu.registers[7],
                            cpu.registers[6],
                        )
                        .map_err(|fault| format!("Guest C/POSIX strcoll fault {fault:?}"))?
                    } else if matches!(service, Service::Errno) {
                        let address = self
                            .kernel
                            .as_ref()
                            .ok_or("Missing errno context")?
                            .errno_address();
                        memory.validate_write(address, 4)?;
                        address
                    } else {
                        crate::strings::call(
                            &mut memory,
                            service.operation(),
                            cpu.registers[7],
                            cpu.registers[6],
                            cpu.registers[2],
                        )
                        .map_err(|fault| format!("Module string service fault {fault:?}"))?
                    };
                    if memory.load(sp, 8)? != ret {
                        return Err("Service corrupted guest return address".into());
                    }
                    cpu.registers[4] = sp.checked_add(8).ok_or("Service stack overflow")?;
                    cpu.registers[0] = value;
                    if terminal {
                        if callbacks.authentic_return(sp, ret) {
                            cpu.registers[4] = sp;
                            cpu.stop = 3;
                            continue;
                        }
                        completed!(value);
                    }
                    cpu.rip = cpu.next_rip;
                    cpu.stop = 0;
                    continue;
                }
                let target_bias = owner(cpu.target)?;
                let target = cpu
                    .target
                    .checked_sub(target_bias)
                    .ok_or("Call below image")?;
                if block_at(target_bias, target).is_none() {
                    translation_miss!(target_bias, target);
                }
                if tail {
                    if block_at(target_bias, target).is_none() || !memory.executable(cpu.target) {
                        return Err(format!(
                            "Unregistered module tail jump to {:#x}",
                            cpu.target
                        ));
                    }
                    cpu.rip = target;
                    cpu.bias = target_bias;
                } else {
                    let ret = cpu
                        .bias
                        .checked_add(cpu.next_rip)
                        .ok_or("Call return overflow")?;
                    if block_at(target_bias, target).is_none()
                        || !memory.executable(cpu.target)
                        || !memory.executable(ret)
                    {
                        return Err("Unregistered module call".into());
                    }
                    let sp = cpu.registers[4]
                        .checked_sub(8)
                        .ok_or("Call stack underflow")?;
                    memory.store(sp, ret, 8)?;
                    cpu.registers[4] = sp;
                    cpu.rip = target;
                    cpu.bias = target_bias;
                }
            } else if cpu.stop == 2 {
                let target = cpu
                    .bias
                    .checked_add(cpu.rip)
                    .ok_or("Branch address overflow")?;
                let target_bias = owner(target)?;
                let target_offset = target
                    .checked_sub(target_bias)
                    .ok_or("Branch below image")?;
                if block_at(target_bias, target_offset).is_none() {
                    translation_miss!(target_bias, target_offset);
                }
                cpu.bias = owner(target)?;
                cpu.rip = target.checked_sub(cpu.bias).ok_or("Branch below image")?;
            }
            if cpu.fuel == 0 || cpu.stop == 4 {
                return Err("Module fuel exhausted".into());
            }
            if block_at(cpu.bias, cpu.rip).is_none() {
                // The pre-dispatch checks above cover all control transfers;
                // this path is an initial entry or a normal service continuation.
                translation_miss!(cpu.bias, cpu.rip);
            }
            let block = block_at(cpu.bias, cpu.rip).ok_or_else(|| {
                format!(
                    "Missing module translation at {:#x} (bias {:#x})",
                    cpu.rip, cpu.bias
                )
            })?;
            let pc = cpu.bias.checked_add(cpu.rip).ok_or("Module PC overflow")?;
            let source = cpu
                .bias
                .checked_add(block.start)
                .ok_or("Block source address overflow")?;
            if !memory.executable(pc) || memory.read(source, block.bytes.len())? != &*block.bytes {
                return Err("Module code does not match translation".into());
            }
            cpu.stop = 0;
            cpu.target = 0;
            // Refresh the raw borrow after Rust-side dispatch accesses, and use
            // it only for this synchronous translated block.
            cpu.memory = (&mut memory as *mut Memory<'_>) as u64;
            let total_fuel = cpu.fuel;
            let dispatch_fuel = if slice.0.is_some() {
                total_fuel.min(slice.1)
            } else {
                total_fuel
            };
            cpu.fuel = dispatch_fuel;
            // SAFETY: registration contract plus byte verification; CPU and all
            // borrowed mappings remain live and exclusive for this call.
            unsafe {
                (block.function)(&mut cpu);
            }
            let spent = dispatch_fuel
                .checked_sub(cpu.fuel)
                .ok_or("Translation increased instruction fuel")?;
            self.consumed_instruction_fuel += u128::from(spent);
            cpu.fuel = total_fuel - spent;
            if slice.0.is_some() {
                slice.1 -= spent;
                if cpu.stop == 4 && cpu.fuel != 0 {
                    cpu.stop = 0;
                }
            }
            cpu.memory = 0;
            if cpu.stop == 0 && cpu.fault == 0 && !(slice.0.is_some() && slice.1 == 0) {
                return Err("Translation did not stop at a boundary".into());
            }
        }
        Err("Module transition budget exhausted".into())
    }
}
impl Executor for NativeExecutor {
    fn signal_termination(&self) -> Option<i32> {
        self.signal_termination
    }

    fn validate_image_retirement(&self, image: &Image) -> Result<()> {
        if image.segments.iter().any(|segment| {
            retained_overlap(
                &self.semaphores,
                &self.callback_ranges,
                segment.address,
                segment.bytes.len(),
            )
        }) {
            return Err("Cannot retire an image containing a pending semaphore object".into());
        }
        Ok(())
    }
    fn exit_request(&self) -> Option<ExitRequest> {
        self.exit_request
    }
    fn call_with_dependencies(
        &mut self,
        image: &mut Image,
        address: u64,
        arguments: Arguments,
        dependencies: &mut [&mut Image],
    ) -> Result<i32> {
        self.execute(image, address, arguments, dependencies)
    }
    fn initialize_with_dependencies(
        &mut self,
        image: &mut Image,
        address: u64,
        dependencies: &mut [&mut Image],
    ) -> Result<()> {
        self.execute(image, address, Arguments::default(), dependencies)
            .map(|_| ())
    }
    fn call(&mut self, image: &mut Image, address: u64, arguments: Arguments) -> Result<i32> {
        self.execute(image, address, arguments, &mut [])
    }
    fn initialize(&mut self, image: &mut Image, address: u64) -> Result<()> {
        self.execute(image, address, Arguments::default(), &mut [])
            .map(|_| ())
    }
}

#[cfg(test)]
mod service_registration_tests {
    use super::*;
    #[test]
    fn stdin_requires_real_source_and_freezes_even_after_rejected_entry() {
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        assert!(native.stdin_pointer_address().is_err());
        assert!(native.configure_stdin(b"/input").is_err());
        native.kernel = Some(crate::kernel::Kernel::new(65536, 0x800000).unwrap());
        assert!(native.configure_stdin(b"/missing").is_err());
        assert!(
            native
                .kernel
                .as_ref()
                .unwrap()
                .descriptors
                .file_token(0)
                .is_err()
        );
        native
            .kernel
            .as_mut()
            .unwrap()
            .files
            .register(b"/input", b"line\n".to_vec())
            .unwrap();
        native.configure_stdin(b"/input").unwrap();
        assert_eq!(
            native.stdin_pointer_address().unwrap(),
            crate::stdio_file::STDIN_PTR_ADDR
        );
        assert_eq!(
            u64::from_le_bytes(native.stdin_pointer),
            crate::stdio_file::STDIN_FILE_ADDR
        );
        assert!(
            native
                .kernel
                .as_ref()
                .unwrap()
                .descriptors
                .file_token(0)
                .is_ok()
        );
        assert!(native.configure_stdin(b"/input").is_err());
        let mut image = Image::load(&crate::modules::fixture(), 0x100000, 65536).unwrap();
        let mut unconfigured = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        unconfigured.kernel = native.kernel.take();
        let _ = unconfigured.run(&mut image, u64::MAX - 1, Arguments::default(), &mut []);
        assert!(
            unconfigured
                .configure_stdin(b"/input")
                .unwrap_err()
                .contains("frozen")
        );
        assert!(unconfigured.stdin_pointer_address().is_err());
    }
    #[test]
    fn system_profile_is_explicit_and_frozen_on_first_execution_attempt() {
        use crate::guest_system::GuestSystemProfile;
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        assert!(native.system_profile().is_none());
        let original = GuestSystemProfile::new(16 << 30, 1, 1).unwrap();
        native.configure_system_profile(original.clone()).unwrap();
        let mut image = Image::load(&crate::modules::fixture(), 0x100000, 65536).unwrap();
        // Even a rejected entry attempt freezes configuration: no host retry can
        // mutate reported hardware after any execution path has been entered.
        let _ = native.run(&mut image, 0xffff_ffff, Arguments::default(), &mut []);
        assert!(
            native
                .configure_system_profile(GuestSystemProfile::new(8 << 30, 1, 1).unwrap())
                .is_err()
        );
        assert_eq!(native.system_profile(), Some(&original));
        let mut unconfigured = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let _ = unconfigured.run(&mut image, 0xffff_ffff, Arguments::default(), &mut []);
        assert!(unconfigured.configure_system_profile(original).is_err());
        assert!(unconfigured.system_profile().is_none());
    }
    #[test]
    fn loader_export_publication_is_atomic_and_resolves_name_and_nid() {
        let mut modules = crate::modules::Modules::new(65536);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        assert!(
            native
                .register_loader_module(&mut modules, "../bad", 0x700000)
                .is_err()
        );
        assert!(native.services.is_empty());
        native
            .register_loader_module(&mut modules, "libkernel.prx", 0x700000)
            .unwrap();
        let nid = crate::strings::nid("sceKernelLoadStartModule");
        for name in [
            "sceKernelLoadStartModule",
            std::str::from_utf8(&nid[..11]).unwrap(),
        ] {
            let identity = crate::imports::identify(name, &BTreeMap::new(), &[]).unwrap();
            assert_eq!(modules.resolve(&identity), Some(0x700000));
        }
        assert!(
            native
                .register_loader_module(&mut modules, "libkernel.prx", 0x710000)
                .is_err()
        );
        assert_eq!(native.services.len(), 3);
        let stop =
            crate::imports::identify("sceKernelStopUnloadModule", &BTreeMap::new(), &[]).unwrap();
        assert_eq!(modules.resolve(&stop), Some(0x700010));
        assert!(
            native
                .register_loader_module(&mut modules, "other.prx", 0x700000)
                .is_err()
        );
    }
    #[test]
    fn loader_request_validates_all_inputs_before_side_effects() {
        let mut data = vec![0u8; 2048];
        let path = b"/app0/test.prx\0";
        data[..path.len()].copy_from_slice(path);
        let mut code = [0u8; 16];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.add(0x3000, &mut code, false, true).unwrap();
        let valid = [0x1000, 3, 0x1100, 0, 0, 0x1200];
        let request = LoadRequest::read(&memory, valid).unwrap();
        assert_eq!(request.path, "/app0/test.prx");
        assert_eq!(request.arguments.size, 3);
        assert_eq!(request.result_address, 0x1200);
        for (index, value) in [
            (0, 0),
            (0, u64::MAX),
            (1, u64::MAX),
            (2, 0x17ff),
            (3, 1),
            (4, 1),
            (5, 0x3000),
            (5, 0x17ff),
        ] {
            let mut invalid = valid;
            invalid[index] = value;
            assert!(LoadRequest::read(&memory, invalid).is_err());
        }
        assert_eq!(memory.load(0x1200, 4).unwrap(), 0);
        memory.write(0x1000, b"/app0/../bad\0").unwrap();
        assert!(LoadRequest::read(&memory, valid).is_err());
        memory.write(0x1000, &vec![b'a'; 1025]).unwrap();
        assert!(LoadRequest::read(&memory, valid).is_err());
        memory.write(0x1000, b"/\xff\0").unwrap();
        assert!(LoadRequest::read(&memory, valid).is_err());
    }
    #[test]
    fn service_publication_is_atomic_across_executor_and_registry() {
        let mut modules = crate::modules::Modules::new(65536);
        let mut native = NativeExecutor::new(0x900000, 4096, 100, 100).unwrap();
        let plain = |name: &str| crate::imports::identify(name, &BTreeMap::new(), &[]).unwrap();
        native.register_service(0x700020, Service::Strlen).unwrap();
        assert!(
            native
                .register_string_module(&mut modules, "libstrings.prx", 0x700000)
                .is_err()
        );
        assert_eq!(native.services.len(), 1);
        assert_eq!(modules.resolve(&plain("strcpy")), None);
        assert!(
            native
                .register_string_module(&mut modules, "libstrings.prx", u64::MAX - 16)
                .is_err()
        );
        assert_eq!(native.services.len(), 1);
        assert!(
            native
                .register_string_module(&mut modules, "../bad", 0x710000)
                .is_err()
        );
        assert_eq!(native.services.len(), 1);
        let handle = native
            .register_string_module(&mut modules, "libstrings.prx", 0x710000)
            .unwrap();
        assert_eq!(modules.phase(handle), Some(crate::modules::Phase::Started));
        assert_eq!(native.services.len(), 6);
        for (i, name) in crate::strings::NAMES[..5].iter().enumerate() {
            assert_eq!(
                modules.resolve(&plain(name)),
                Some(0x710000 + i as u64 * 16)
            );
            let nid = crate::strings::nid(name);
            assert_eq!(
                modules.resolve(&plain(std::str::from_utf8(&nid[..11]).unwrap())),
                Some(0x710000 + i as u64 * 16)
            );
        }
        assert!(
            native
                .register_string_module(&mut modules, "libstrings.prx", 0x720000)
                .is_err()
        );
        assert_eq!(native.services.len(), 6);
    }
}

#[cfg(test)]
mod exit_request_tests {
    use super::*;
    fn suspended(image: &Image, tail: bool, status: u64, fuel: u64) -> Continuation {
        let mut cpu = Cpu {
            bias: image.bias,
            rip: 0x100,
            next_rip: 0x106,
            stop: if tail { 5 } else { 1 },
            target: 0x700000,
            fuel,
            ..Cpu::default()
        };
        cpu.registers[7] = status;
        // Tail exit must not dereference a return address, even if RSP is unmapped.
        cpu.registers[4] = if tail { 0 } else { 0x900ff8 };
        Continuation {
            callbacks: None,
            stack_lease: Rc::new(()),
            quantum: None,
            return_convention: ReturnConvention::ModuleStatus,
            guest_thread: MAIN_GUEST_THREAD,
            cpu,
            stack: vec![0; 4096],
            stack_address: 0x900000,
            remaining: 10,
            caller_bias: image.bias,
        }
    }
    #[test]
    fn noreturn_dispatch_skips_ud2_and_tail_return_stack_and_latches_status() {
        for (service, kind) in [
            (Service::Exit, ExitKind::Normal),
            (Service::ImmediateExit, ExitKind::Immediate),
        ] {
            for tail in [false, true] {
                for status in [0, 1, 0xabcd_1234_ffff_ffff] {
                    let mut file = crate::modules::fixture();
                    file[0x106..0x108].copy_from_slice(&[0x0f, 0x0b]);
                    let mut image = Image::load(&file, 0x100000, 65536).unwrap();
                    let pending = suspended(&image, tail, status, 3);
                    let stale = suspended(&image, false, 0, 3);
                    let mut executor = NativeExecutor::new(0x900000, 4096, 10, 10).unwrap();
                    executor.register_service(0x700000, service).unwrap();
                    // Begin at the dispatcher boundary: no generated function or
                    // return-PC translation exists, and UD2 cannot be executed.
                    let result = executor
                        .execute_flow(
                            &mut image,
                            0,
                            Arguments::default(),
                            &mut [],
                            Some(pending),
                            &mut [],
                        )
                        .unwrap();
                    let expected = ExitRequest {
                        status: status as u32 as i32,
                        kind,
                    };
                    assert!(
                        matches!(result,Execution::ExitRequested(request) if request==expected)
                    );
                    assert_eq!(executor.exit_request(), Some(expected));
                    assert_eq!(executor.exit_remaining_fuel(), Some(2));
                    assert!(
                        executor
                            .run(&mut image, 0x100100, Arguments::default(), &mut [])
                            .err()
                            .unwrap()
                            .contains("exit requested")
                    );
                    assert!(
                        executor
                            .resume(&mut image, &mut [], stale, 0)
                            .err()
                            .unwrap()
                            .contains("exit requested")
                    );
                }
            }
        }
    }
    #[test]
    fn exhausted_budget_does_not_invent_exit_request() {
        let mut image = Image::load(&crate::modules::fixture(), 0x100000, 65536).unwrap();
        let pending = suspended(&image, true, 1, 0);
        let mut executor = NativeExecutor::new(0x900000, 4096, 10, 10).unwrap();
        executor.register_service(0x700000, Service::Exit).unwrap();
        assert!(
            executor
                .execute_flow(
                    &mut image,
                    0,
                    Arguments::default(),
                    &mut [],
                    Some(pending),
                    &mut []
                )
                .err()
                .unwrap()
                .contains("fuel")
        );
        assert_eq!(executor.exit_request(), None);
    }
}

#[cfg(test)]
mod callback_slot_tests {
    use super::callback_stack_slot;
    #[test]
    fn callback_alignment_red_zone_and_live_input_separation() {
        for sp in [0x2000, 0x2008, 0x2010, 0x2018] {
            let slot = callback_stack_slot(sp, 0x1000, 0x3000, 32, None).unwrap();
            assert_eq!(slot % 16, 8);
            assert!(slot + 8 <= sp - 128);
            let low = slot - 128;
            assert!(callback_stack_slot(sp, 0x1000, low, 1, None).is_err());
            assert!(callback_stack_slot(sp, 0x1000, low - 1, 2, None).is_err());
            assert!(callback_stack_slot(sp, 0x1000, 0x3000, 1, Some(low)).is_err());
            assert!(callback_stack_slot(sp, 0x1000, sp, 32, Some(sp)).is_ok());
            assert!(callback_stack_slot(sp, 0x1000, low - 8, 8, Some(low - 1)).is_ok());
            assert!(callback_stack_slot(sp, low + 1, 0x3000, 32, None).is_err());
        }
        assert!(callback_stack_slot(64, 0, 0x3000, 1, None).is_err());
        assert!(callback_stack_slot(0x2000, 0x1000, u64::MAX, 2, None).is_err());
    }
}

#[cfg(test)]
mod strdup_dispatch_tests {
    use super::*;
    #[test]
    fn strdup_success_ignores_errno_and_invalid_input_precedes_it() {
        let mut source = *b"test\0";
        let mut readonly_errno = 91u32.to_le_bytes();
        let mut owner = crate::guest_memory::OwnedMappings::default();
        {
            let mut memory = Memory::default();
            memory.add(0x1000, &mut source, false, false).unwrap();
            memory
                .add(0x2000, &mut readonly_errno, false, false)
                .unwrap();
            memory.attach_owned(&mut owner).unwrap();
            for errno in [None, Some(0x2000), Some(0x3000)] {
                let copy = strdup_service(&mut memory, 0x1000, errno, 0x4000, 0x5000).unwrap();
                assert_eq!(memory.read(copy, 5).unwrap(), b"test\0");
                memory.free(copy).unwrap();
                let error = strdup_service(&mut memory, 0, errno, 0x4000, 0x5000).unwrap_err();
                assert!(error.contains("Null guest strdup source"));
            }
            assert_eq!(memory.load(0x2000, 4).unwrap(), 91);
        }
        assert_eq!(owner.bytes_allocated(), 0);
    }
    #[test]
    fn strdup_allocation_failure_guards_errno_and_return_frame_before_write() {
        let mut source = *b"x\0";
        let mut errno = 91u32.to_le_bytes();
        let mut readonly_errno = 92u32.to_le_bytes();
        let mut frame = 0x5000u64.to_le_bytes();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut source, false, false).unwrap();
        memory.add(0x2000, &mut errno, true, false).unwrap();
        memory
            .add(0x3000, &mut readonly_errno, false, false)
            .unwrap();
        memory.add(0x4000, &mut frame, true, false).unwrap();
        // No allocator is attached: the real allocator returns ENOMEM without mutation.
        for target in [None, Some(0x3000), Some(0x6000), Some(0x4000)] {
            assert!(strdup_service(&mut memory, 0x1000, target, 0x4000, 0x5000).is_err());
            assert_eq!(memory.load(0x4000, 8).unwrap(), 0x5000);
            assert_eq!(memory.load(0x2000, 4).unwrap(), 91);
            assert_eq!(memory.load(0x3000, 4).unwrap(), 92);
        }
        assert_eq!(
            strdup_service(&mut memory, 0x1000, Some(0x2000), 0x4000, 0x5000).unwrap(),
            0
        );
        assert_eq!(memory.load(0x2000, 4).unwrap(), 12);
    }
}

#[cfg(test)]
mod condition_storage_tests {
    use super::*;
    #[test]
    fn own_stack_heap_and_image_survive_foreign_and_spoofed_stack_rejection() {
        let own = Rc::new(());
        let foreign = Rc::new(());
        let mut own_bytes = [0u8; 32];
        let mut foreign_bytes = [0u8; 32];
        let mut image = [0u8; 32];
        let mut replaced = [0u8; 32];
        let stacks = [
            StackIdentity {
                address: 0x1000,
                length: 32,
                backing: own_bytes.as_ptr() as usize,
                lease: Rc::downgrade(&own),
            },
            StackIdentity {
                address: 0x2000,
                length: 32,
                backing: foreign_bytes.as_ptr() as usize,
                lease: Rc::downgrade(&foreign),
            },
        ];
        let mut allocations = crate::guest_memory::OwnedMappings::default();
        let mut memory = Memory::default();
        memory.attach_owned(&mut allocations).unwrap();
        memory.add(0x1000, &mut own_bytes, true, false).unwrap();
        memory.add(0x2000, &mut foreign_bytes, true, false).unwrap();
        memory.add(0x3000, &mut image, true, false).unwrap();
        let heap = memory.malloc(32).unwrap();
        assert_eq!(
            condition_storage(&memory, 0x1018, &[], &stacks, &own).unwrap(),
            (0x1000, 32)
        );
        assert_eq!(
            condition_storage(&memory, heap + 8, &[], &stacks, &own)
                .unwrap()
                .0,
            heap
        );
        assert_eq!(
            condition_storage(&memory, 0x3008, &[(0x3000, 32)], &stacks, &own).unwrap(),
            (0x3000, 32)
        );
        // The original foreign backing and identity are both live: still rejected.
        assert!(
            condition_storage(&memory, 0x2000, &[], &stacks, &own)
                .unwrap_err()
                .contains("foreign-stack")
        );
        assert!(condition_storage(&memory, 0x1019, &[], &stacks, &own).is_err());
        assert!(condition_storage(&memory, 0x3000, &[], &stacks, &own).is_err());
        drop(memory);
        let mut replacement = Memory::default();
        replacement.add(0x1000, &mut replaced, true, false).unwrap();
        assert!(condition_storage(&replacement, 0x1000, &[], &stacks, &own).is_err());
    }
}

#[cfg(test)]
mod condition_transfer_tests {
    use super::*;
    fn completed(native: &NativeExecutor, image: &Image) -> ThreadRequest {
        let mut stack = vec![0u8; 4096];
        stack[4088..].copy_from_slice(&u64::MAX.to_le_bytes());
        let mut cpu = Cpu {
            stop: 3,
            fuel: 16,
            ..Cpu::default()
        };
        cpu.registers[4] = 0x900ff8;
        ThreadRequest {
            owner: native.mutex_wait_owner.clone(),
            condition_faults: Some(native.condition_faults.clone()),
            operation: ThreadOperation::Completed(0),
            continuation: Some(Continuation {
                callbacks: None,
                stack_lease: Rc::new(()),
                quantum: None,
                return_convention: ReturnConvention::WorkerPointer,
                guest_thread: MAIN_GUEST_THREAD,
                cpu,
                stack,
                stack_address: 0x900000,
                remaining: 8,
                caller_bias: image.bias,
            }),
        }
    }
    #[test]
    fn completed_condition_transfer_preflight_keeps_stack_and_drop_latches_fault() {
        let mut native = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
        let image = Image::load(&memory_fixture(), 0x100000, 65536).unwrap();
        let mut wrong = Image::load(&memory_fixture(), 0x200000, 65536).unwrap();
        let mut request = completed(&native, &image);
        let pointer = request.continuation.as_ref().unwrap().stack.as_ptr();
        assert!(
            native
                .finish_thread_request(&mut request, &mut wrong, &mut [], &mut [], 0)
                .is_err()
        );
        assert_eq!(
            request.continuation.as_ref().unwrap().stack.as_ptr(),
            pointer
        );
        assert!(native.require_running().is_ok()); // Still owned, never abandoned.
        drop(request);
        assert_eq!(native.condition_faults.borrow()[0].stack.as_ptr(), pointer);
        assert!(native.require_running().is_err());
    }
    #[test]
    fn completed_condition_bad_stack_or_alias_mapping_is_not_consumed() {
        for bad in 0..4 {
            let mut native = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
            let mut image = Image::load(&memory_fixture(), 0x100000, 65536).unwrap();
            let mut request = completed(&native, &image);
            match bad {
                0 => request.continuation.as_mut().unwrap().stack_address = 0x8ff000,
                1 => request.continuation.as_mut().unwrap().stack_address += 1,
                2 => {
                    request.continuation.as_mut().unwrap().stack.pop();
                }
                _ => {}
            }
            let pointer = request.continuation.as_ref().unwrap().stack.as_ptr();
            let mut alias = [0u8; 8];
            let mut data = if bad == 3 {
                vec![DataRegion {
                    address: 0x900000,
                    bytes: &mut alias,
                    writable: true,
                }]
            } else {
                Vec::new()
            };
            assert!(
                native
                    .finish_thread_request(&mut request, &mut image, &mut [], &mut data, 0)
                    .is_err()
            );
            assert_eq!(
                request.continuation.as_ref().unwrap().stack.as_ptr(),
                pointer
            );
            assert!(!native.condition_transfer_active);
            assert!(native.require_running().is_ok());
        }
    }
    #[test]
    fn consuming_condition_resume_error_pins_bytes_but_success_retires_them() {
        for fail in [true, false] {
            let mut native = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
            let mut image = Image::load(&memory_fixture(), 0x100000, 65536).unwrap();
            let mut request = completed(&native, &image);
            if fail {
                request.continuation.as_mut().unwrap().remaining = 0;
            }
            let pointer = request.continuation.as_ref().unwrap().stack.as_ptr();
            let result =
                native.finish_thread_request(&mut request, &mut image, &mut [], &mut [], 0);
            assert!(request.continuation.is_none());
            assert!(!native.condition_transfer_active);
            if fail {
                assert!(result.is_err());
                assert_eq!(native.fault_stack.borrow()[0].1.as_ptr(), pointer);
                assert!(native.require_running().is_err());
            } else {
                assert!(matches!(result, Ok(Execution::WorkerReturned(0))));
                assert!(native.fault_stack.borrow().is_empty());
                assert!(native.require_running().is_ok());
            }
        }
    }
}

#[cfg(test)]
mod multibyte_dispatch_tests {
    use super::*;
    #[test]
    fn success_does_not_require_errno_and_errors_guard_it() {
        let mut src = [255, 0];
        let mut dst = [0xa5; 4];
        let mut readonly_errno = 91u32.to_le_bytes();
        let mut writable_errno = 92u32.to_le_bytes();
        let mut frame = 0x9000u64.to_le_bytes();
        let mut memory = Memory::default();
        for (address, bytes, writable) in [
            (0x1000, &mut src[..], false),
            (0x2000, &mut dst[..], true),
            (0x3000, &mut readonly_errno[..], false),
            (0x4000, &mut writable_errno[..], true),
            (0x8000, &mut frame[..], true),
        ] {
            memory.add(address, bytes, writable, false).unwrap();
        }
        for errno in [None, Some(0x3000), Some(u64::MAX)] {
            let p = crate::guest_multibyte::mbrtowc(&memory, 0x2000, 0x1000, 1, u64::MAX).unwrap();
            assert_eq!(
                multibyte_service(&mut memory, p, errno, 0x8000, 0x9000).unwrap(),
                1
            );
            assert_eq!(memory.load(0x2000, 2).unwrap(), 255);
            let p =
                crate::guest_multibyte::mbrtowc(&memory, u64::MAX, u64::MAX, 0, u64::MAX).unwrap();
            assert_eq!(
                multibyte_service(&mut memory, p, errno, 0x8000, 0x9000).unwrap(),
                u64::MAX - 1
            );
        }
        for errno in [None, Some(0x3000), Some(u64::MAX), Some(0x8000)] {
            let p = crate::guest_multibyte::wcrtomb(u64::MAX, 256, u64::MAX).unwrap();
            assert!(multibyte_service(&mut memory, p, errno, 0x8000, 0x9000).is_err());
            assert_eq!(memory.load(0x8000, 8).unwrap(), 0x9000);
        }
        let p = crate::guest_multibyte::wcrtomb(u64::MAX, 256, u64::MAX).unwrap();
        assert_eq!(
            multibyte_service(&mut memory, p, Some(0x4000), 0x8000, 0x9000).unwrap(),
            u64::MAX
        );
        assert_eq!(memory.load(0x4000, 4).unwrap(), 86);
        assert_eq!(memory.load(0x3000, 4).unwrap(), 91);
    }
    #[test]
    fn source_pointer_and_output_frame_guards_precede_all_writes() {
        let mut src = [65, 0];
        let mut dst = [0xa5; 4];
        let mut pointer = 0x1000u64.to_le_bytes();
        let mut memory = Memory::default();
        memory.add(0x1000, &mut src, false, false).unwrap();
        memory.add(0x2000, &mut dst, true, false).unwrap();
        memory.add(0x8000, &mut pointer, true, false).unwrap();
        let p = crate::guest_multibyte::mbsrtowcs(&memory, 0x2000, 0x8000, 2, 0).unwrap();
        assert!(multibyte_service(&mut memory, p, None, 0x8000, 0x1000).is_err());
        assert_eq!(memory.read(0x2000, 4).unwrap(), [0xa5; 4]);
        assert_eq!(memory.load(0x8000, 8).unwrap(), 0x1000);
        let p = crate::guest_multibyte::mbrtowc(&memory, 0x8007, 0x1000, 1, 0).unwrap();
        assert!(multibyte_service(&mut memory, p, None, 0x8000, 0x1000).is_err());
        assert_eq!(memory.load(0x8000, 8).unwrap(), 0x1000);
    }
}

#[cfg(test)]
mod abort_dispatch_tests {
    use super::*;
    const ABORT: u64 = 0x700000;
    fn pending(image: &Image) -> Continuation {
        let mut cpu = Cpu {
            stop: 1,
            target: ABORT,
            bias: image.bias,
            rip: 0x100,
            next_rip: 0x106,
            fuel: 16,
            ..Cpu::default()
        };
        cpu.registers[4] = 0x900ff8;
        cpu.registers[7] = 0xfeed;
        cpu.flags = 0x847;
        cpu.mxcsr = 0x5f80;
        cpu.x87_control = 0x077f;
        cpu.x87_status = 0x120;
        cpu.vectors[3] = [0x6d; 32];
        Continuation {
            callbacks: None,
            stack_lease: Rc::new(()),
            quantum: None,
            return_convention: ReturnConvention::ModuleStatus,
            guest_thread: MAIN_GUEST_THREAD,
            cpu,
            stack: vec![0; 4096],
            stack_address: 0x900000,
            remaining: 16,
            caller_bias: image.bias,
        }
    }
    fn action(n: &mut NativeExecutor, handler: u64, flags: u32) {
        let snapshot = n.signal_actions.snapshot(6).unwrap();
        n.signal_actions
            .prepare_snapshot(
                &snapshot,
                Some(crate::guest_signal_action::Disposition {
                    handler,
                    flags,
                    mask: [0; 16],
                }),
            )
            .unwrap()
            .commit();
    }
    #[test]
    fn default_and_ignored_abort_stop_at_service_without_following_ud2() {
        for handler in [0, 1] {
            let mut file = memory_fixture();
            file[0x106..0x108].copy_from_slice(&[0x0f, 0x0b]);
            let mut image = Image::load(&file, 0x100000, 65536).unwrap();
            let mut n = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
            n.register_service(ABORT, Service::Abort).unwrap();
            action(&mut n, handler, 0);
            n.tls
                .replace_signal_mask(MAIN_GUEST_THREAD, [255; 16])
                .unwrap();
            let state = pending(&image);
            let result = n
                .execute_flow(
                    &mut image,
                    0,
                    Arguments::default(),
                    &mut [],
                    Some(state),
                    &mut [],
                )
                .unwrap();
            assert!(matches!(result, Execution::SignalTerminated { signal: 6 }));
            assert_eq!(n.signal_termination(), Some(6));
            assert_eq!(n.exit_request(), None);
            assert_eq!(n.exit_context.as_ref().unwrap().cpu.fuel, 15);
            assert_eq!(
                n.tls.signal_mask(MAIN_GUEST_THREAD).unwrap(),
                crate::guest_abort::abort_mask()
            );
            assert!(n.require_running().is_err());
        }
    }
    #[test]
    fn unsupported_handler_delivery_changes_no_mask_disposition_or_phase() {
        for flags in [1, 0x40] {
            let mut image = Image::load(&memory_fixture(), 0x100000, 65536).unwrap();
            let mut n = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
            n.register_service(ABORT, Service::Abort).unwrap();
            action(&mut n, 0x100100, flags);
            let before = n.tls.signal_mask(MAIN_GUEST_THREAD).unwrap();
            let state = pending(&image);
            assert!(
                n.execute_flow(
                    &mut image,
                    0,
                    Arguments::default(),
                    &mut [],
                    Some(state),
                    &mut []
                )
                .is_err()
            );
            assert_eq!(n.tls.signal_mask(MAIN_GUEST_THREAD).unwrap(), before);
            assert_eq!(n.signal_actions.snapshot(6).unwrap().handler(), 0x100100);
            assert_eq!(n.signal_termination(), None);
            assert!(n.abort_faults.borrow().is_empty());
        }
    }
    #[test]
    fn dropping_suspended_abort_handler_preserves_actual_stack_backing() {
        unsafe extern "C" fn not_executed(_cpu: *mut Cpu) {}
        let file = memory_fixture();
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let mut n = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
        n.register_service(ABORT, Service::Abort).unwrap();
        action(&mut n, 0x100100, 0);
        n.blocks.insert(
            0x100,
            Block {
                start: 0x100,
                bytes: Rc::from(&file[0x100..0x101]),
                function: not_executed,
                _library: None,
            },
        );
        let mut state = pending(&image);
        state.quantum = Some(1);
        let Execution::QuantumYield(wait) = n
            .execute_flow(
                &mut image,
                0,
                Arguments::default(),
                &mut [],
                Some(state),
                &mut [],
            )
            .unwrap()
        else {
            panic!("handler yield");
        };
        let pointer = wait.continuation.as_ref().unwrap().stack.as_ptr();
        drop(wait);
        assert_eq!(n.abort_faults.borrow().len(), 1);
        assert_eq!(n.fault_stack.borrow().len(), 1);
        assert_eq!(n.fault_stack.borrow()[0].1.as_ptr(), pointer);
        assert!(n.callback_ranges.overlaps(0x900000, 4096));
        assert!(n.require_running().is_err());
    }
    // Dispatcher-state injection tests restoration independently of generated
    // ARM64 code. Native handler execution has a separate verification gate.
    #[test]
    fn handler_owned_quantum_return_restores_cpu_and_live_fuel() {
        unsafe extern "C" fn not_executed(_cpu: *mut Cpu) {}
        let file = memory_fixture();
        let mut image = Image::load(&file, 0x100000, 65536).unwrap();
        let mut n = NativeExecutor::new(0x900000, 4096, 16, 16).unwrap();
        n.register_service(ABORT, Service::Abort).unwrap();
        action(&mut n, 0x100100, crate::guest_abort::SA_RESETHAND);
        n.blocks.insert(
            0x100,
            Block {
                start: 0x100,
                bytes: Rc::from(&file[0x100..0x101]),
                function: not_executed,
                _library: None,
            },
        );
        let mut state = pending(&image);
        state.quantum = Some(1);
        // Keep precision beyond binary64 and a nonzero TOP across the handler.
        state.cpu.x87.push_raw(
            0x037f,
            &mut state.cpu.x87_status,
            [1, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f],
        ).unwrap();
        let original = state.cpu.clone();
        let Execution::QuantumYield(mut wait) = n
            .execute_flow(
                &mut image,
                0,
                Arguments::default(),
                &mut [],
                Some(state),
                &mut [],
            )
            .unwrap()
        else {
            panic!("handler dispatch must yield");
        };
        assert_eq!(n.signal_actions.snapshot(6).unwrap().handler(), 0);
        assert_ne!(n.tls.signal_mask(MAIN_GUEST_THREAD).unwrap()[0] & 32, 0);
        let saved = wait.continuation.as_mut().unwrap();
        assert_eq!(saved.cpu.registers[7], 6);
        let frame = saved
            .callbacks
            .as_ref()
            .unwrap()
            .abort_frames
            .last()
            .unwrap();
        assert!(
            saved
                .callbacks
                .as_ref()
                .unwrap()
                .authentic_abort_return(saved.cpu.registers[4], u64::MAX)
        );
        assert_eq!(frame.cpu.flags, original.flags);
        saved.cpu.flags = 0;
        saved.cpu.mxcsr = 0;
        saved.cpu.x87_control = 0;
        saved.cpu.x87_status = 0;
        saved.cpu.x87 = Default::default();
        saved.cpu.vectors.fill([0; 32]);
        saved.cpu.registers[7] = 99;
        saved.cpu.fuel = 9;
        saved.cpu.stop = 3;
        let outcome = n
            .resume_quantum(&mut image, &mut [], &mut wait, &mut [])
            .unwrap();
        assert!(matches!(outcome, Execution::SignalTerminated { signal: 6 }));
        let restored = &n.exit_context.as_ref().unwrap().cpu;
        assert_eq!(restored.registers, original.registers);
        assert_eq!(restored.flags, original.flags);
        assert_eq!(restored.mxcsr, original.mxcsr);
        assert_eq!(restored.x87_control, original.x87_control);
        assert_eq!(restored.x87_status, original.x87_status);
        assert_eq!(restored.x87, original.x87);
        assert_eq!(restored.vectors, original.vectors);
        assert_eq!(restored.fuel, 9);
        assert_eq!(
            n.tls.signal_mask(MAIN_GUEST_THREAD).unwrap(),
            crate::guest_abort::abort_mask()
        );
    }
}
