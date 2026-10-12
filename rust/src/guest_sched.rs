//! FreeBSD11.4 scheduling ABI subset, not host scheduling or realtime execution.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a/include/freebsd/sys/{sched,priority,rtprio}.h:
//! FIFO1/OTHER2/RR3; sched_param is one i32; OTHER limits0..103, realtime0..31.
//! Primary semantics: freebsd/freebsd-src releng/11.4 lib/libthr/thread/
//! thr_{getschedparam,setschedparam,list}.c and sys/kern/ksched.c.
//! Same-OTHER setschedparam only changes libthr's cached priority; no kernel call.
//! Policy transitions require scheduling behavior and remain explicit unsupported.
use crate::{Result, guest_memory::Memory, guest_tls::GuestTls};
pub const EINVAL: i32 = 22;
pub const ESRCH: i32 = 3;
pub fn priority_limit(policy: i32, maximum: bool) -> std::result::Result<i32, i32> {
    match policy {
        1 | 3 => Ok(if maximum { 31 } else { 0 }),
        2 => Ok(if maximum { 103 } else { 0 }),
        _ => Err(EINVAL),
    }
}
pub fn target_error(tls: &GuestTls, target: u64) -> Option<i32> {
    if target == 0 {
        Some(EINVAL)
    } else if !tls.contains_thread(target) {
        Some(ESRCH)
    } else {
        None
    }
}
/// Match NULL-output-before-target validation. No writes occur until both
/// outputs have full mappings. Alias writes retain policy-then-priority order.
pub fn get(
    tls: &GuestTls,
    memory: &mut Memory<'_>,
    target: u64,
    policy: u64,
    parameter: u64,
) -> Result<i32> {
    if policy == 0 || parameter == 0 {
        return Ok(EINVAL);
    }
    if let Some(error) = target_error(tls, target) {
        return Ok(error);
    }
    let (kind, priority) = tls.scheduling(target)?;
    memory.validate_write(policy, 4)?;
    memory.validate_write(parameter, 4)?;
    memory.store(policy, kind as u32 as u64, 4)?;
    memory.store(parameter, priority as u32 as u64, 4)?;
    Ok(0)
}
/// Target validation precedes parameter access. The supported OTHER fast path
/// accepts every signed priority, exactly as libthr; don't apply kernel limits.
pub fn set(
    tls: &mut GuestTls,
    memory: &Memory<'_>,
    target: u64,
    policy: i32,
    parameter: u64,
) -> Result<i32> {
    if let Some(error) = target_error(tls, target) {
        return Ok(error);
    }
    let (old_policy, _) = tls.scheduling(target)?;
    if old_policy != 2 || policy != 2 {
        return Err(
            "Guest scheduling policy transition requires realtime scheduler support".into(),
        );
    }
    let priority = memory.load(parameter, 4)? as u32 as i32;
    tls.replace_scheduling(target, policy, priority)?;
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn limits_and_cached_other_priority() {
        for (policy, maximum) in [(1, 31), (2, 103), (3, 31)] {
            assert_eq!(priority_limit(policy, true), Ok(maximum));
            assert_eq!(priority_limit(policy, false), Ok(0));
        }
        assert_eq!(priority_limit(0, true), Err(EINVAL));
        let mut tls = GuestTls::default();
        tls.register_thread(1).unwrap();
        let mut bytes = [0u8; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        for priority in [i32::MIN, -1, 0, 51, 76, 103, i32::MAX] {
            memory.store(0x1000, priority as u32 as u64, 4).unwrap();
            assert_eq!(set(&mut tls, &memory, 1, 2, 0x1000).unwrap(), 0);
            assert_eq!(get(&tls, &mut memory, 1, 0x1000, 0x1000).unwrap(), 0);
            assert_eq!(memory.load(0x1000, 4).unwrap() as u32 as i32, priority);
        }
        assert_eq!(get(&tls, &mut memory, 99, 0, u64::MAX).unwrap(), EINVAL);
        assert_eq!(
            get(&tls, &mut memory, 99, u64::MAX, u64::MAX).unwrap(),
            ESRCH
        );
        assert_eq!(set(&mut tls, &memory, 0, 2, u64::MAX).unwrap(), EINVAL);
        assert_eq!(set(&mut tls, &memory, 99, 2, u64::MAX).unwrap(), ESRCH);
        assert!(set(&mut tls, &memory, 1, 3, 0x1000).is_err());
        let before = memory.read(0x1000, 8).unwrap().to_vec();
        assert!(get(&tls, &mut memory, 1, 0x1000, 0x1006).is_err());
        assert_eq!(memory.read(0x1000, 8).unwrap(), before);
    }
}
