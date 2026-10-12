//! Pure relative nanosleep request/deadline model, no host thread blocking.
//! FreeBSD releng/11.4 sys/kern/kern_time.c lines474,511–546,581–595:
//! invalid nanoseconds => EINVAL; valid nsec with negative seconds => success;
//! remaining is copied only for EINTR. Signal delivery is not modeled here.
use crate::{Result, guest_memory::Memory};
const BILLION: u128 = 1_000_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    InvalidNanoseconds,
    Immediate,
    Delay { nanoseconds: u128 },
}
impl Request {
    pub fn decode(memory: &Memory<'_>, address: u64) -> Result<Self> {
        let bytes = memory.read(address, 16)?;
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanos = i64::from_le_bytes(bytes[8..].try_into().unwrap());
        Ok(Self::from_fields(seconds, nanos))
    }
    pub fn from_fields(seconds: i64, nanoseconds: i64) -> Self {
        if !(0..1_000_000_000).contains(&nanoseconds) {
            Self::InvalidNanoseconds
        } else if seconds < 0 || (seconds == 0 && nanoseconds == 0) {
            Self::Immediate
        } else {
            Self::Delay {
                nanoseconds: seconds as u128 * BILLION + nanoseconds as u128,
            }
        }
    }
}
/// Target in the scheduler's owned monotonic nanosecond domain. Never derive
/// deadlines from gettimeofday/SystemTime, which can change while sleeping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deadline {
    target: u128,
}
impl Deadline {
    pub fn after(now: u128, nanoseconds: u128) -> Result<Self> {
        Ok(Self {
            target: now
                .checked_add(nanoseconds)
                .ok_or("Sleep deadline exceeds monotonic domain")?,
        })
    }
    pub fn target(self) -> u128 {
        self.target
    }
    pub fn ready(self, now: u128) -> bool {
        now >= self.target
    }
    pub fn remaining_nanos(self, now: u128) -> u128 {
        self.target.saturating_sub(now)
    }
    /// Pure remainder encoding for a future real EINTR path. Calling this is not
    /// signal delivery. Normal completion must not write the guest remainder.
    pub fn remaining_bytes(self, now: u128) -> Result<[u8; 16]> {
        let remaining = self.remaining_nanos(now);
        let seconds =
            i64::try_from(remaining / BILLION).map_err(|_| "Sleep remaining time overflow")?;
        let nanoseconds = (remaining % BILLION) as i64;
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&seconds.to_le_bytes());
        bytes[8..].copy_from_slice(&nanoseconds.to_le_bytes());
        Ok(bytes)
    }
}
/// One executor-wide monotonic domain. Manual mode must be configured before
/// creating any deadlines; tests can advance it but cannot move it backwards.
#[derive(Debug)]
pub struct Clock {
    origin: std::time::Instant,
    manual: Option<u128>,
    used: bool,
}
impl Default for Clock {
    fn default() -> Self {
        Self {
            origin: std::time::Instant::now(),
            manual: None,
            used: false,
        }
    }
}
impl Clock {
    pub fn now(&self) -> u128 {
        self.manual
            .unwrap_or_else(|| self.origin.elapsed().as_nanos())
    }
    pub fn configure_manual(&mut self, now: u128) -> Result<()> {
        if self.used {
            return Err("Sleep clock already has issued deadlines".into());
        }
        self.manual = Some(now);
        Ok(())
    }
    pub fn advance_manual(&mut self, now: u128) -> Result<()> {
        let old = self.manual.ok_or("Sleep clock is not manual")?;
        if now < old {
            return Err("Sleep clock cannot move backwards".into());
        }
        self.manual = Some(now);
        Ok(())
    }
    pub fn deadline(&mut self, nanoseconds: u128) -> Result<Deadline> {
        let deadline = Deadline::after(self.now(), nanoseconds)?;
        self.used = true;
        Ok(deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_validation_and_full64_seconds() {
        for seconds in [i64::MIN, -1, 0, 1, i64::MAX] {
            for nanos in [i64::MIN, -1, 1_000_000_000, i64::MAX] {
                assert_eq!(
                    Request::from_fields(seconds, nanos),
                    Request::InvalidNanoseconds
                );
            }
        }
        assert_eq!(Request::from_fields(-1, 999999999), Request::Immediate);
        assert_eq!(Request::from_fields(0, 0), Request::Immediate);
        assert_eq!(
            Request::from_fields(0, 1),
            Request::Delay { nanoseconds: 1 }
        );
        assert_eq!(
            Request::from_fields(1i64 << 32, 999999999),
            Request::Delay {
                nanoseconds: (1u128 << 32) * BILLION + 999999999
            }
        );
        assert_eq!(
            Request::from_fields(i64::MAX, 999999999),
            Request::Delay {
                nanoseconds: i64::MAX as u128 * BILLION + 999999999
            }
        );
    }
    #[test]
    fn guarded_unaligned_snapshot() {
        let mut bytes = [0xa5; 18];
        bytes[1..9].copy_from_slice(&7i64.to_le_bytes());
        bytes[9..17].copy_from_slice(&9i64.to_le_bytes());
        let before = bytes;
        let mut memory = Memory::default();
        memory.add(0x1_0000_0000, &mut bytes, false, false).unwrap();
        assert_eq!(
            Request::decode(&memory, 0x1_0000_0001).unwrap(),
            Request::Delay {
                nanoseconds: 7 * BILLION + 9
            }
        );
        assert!(Request::decode(&memory, 0x1_0000_0003).is_err());
        assert!(Request::decode(&memory, u64::MAX).is_err());
        assert!(Request::decode(&memory, 0).is_err());
        assert_eq!(memory.read(0x1_0000_0000, 18).unwrap(), before);
    }
    #[test]
    fn deadline_not_early_and_normalized_remainder() {
        let deadline = Deadline::after(100, 2 * BILLION + 3).unwrap();
        assert_eq!(deadline.target(), 2 * BILLION + 103);
        assert!(!deadline.ready(2 * BILLION + 102));
        assert!(deadline.ready(2 * BILLION + 103));
        assert_eq!(deadline.remaining_nanos(2 * BILLION + 104), 0);
        let bytes = deadline.remaining_bytes(104).unwrap();
        assert_eq!(&bytes[..8], &1i64.to_le_bytes());
        assert_eq!(&bytes[8..], &999999999i64.to_le_bytes());
        assert_eq!(deadline.remaining_bytes(u128::MAX).unwrap(), [0; 16]);
        assert!(Deadline::after(u128::MAX, 1).is_err());
    }
}
