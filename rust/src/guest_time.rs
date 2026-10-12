//! Guest gettimeofday values and ordered output plan.
//! SDK10b539de8f2b6773a9a78caa7576a97dbd99b29a: sys/_timeval.h,
//! sys/_types.h and x86/_types.h define signed64 time_t/suseconds_t onLP64;
//! sys/time.h timezone is two int32 fields. FreeBSD11.4 kern_time.c
//! sys_gettimeofday copies optional timeval before optional timezone.
//! Output guards remain explicit prototype faults: this plan does not emulate
//! kernel copyout's possible partial writes on invalid mappings.
use crate::Result;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeval {
    pub seconds: i64,
    pub microseconds: i64,
}
impl Timeval {
    pub fn bytes(self) -> Result<[u8; 16]> {
        if !(0..1_000_000).contains(&self.microseconds) {
            return Err("Unnormalized guest timeval".into());
        }
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&self.seconds.to_le_bytes());
        bytes[8..].copy_from_slice(&self.microseconds.to_le_bytes());
        Ok(bytes)
    }
    pub fn from_epoch_duration(duration: Duration, before_epoch: bool) -> Result<Self> {
        let seconds = i128::from(duration.as_secs());
        let nanos = duration.subsec_nanos();
        let (seconds, micros) = if before_epoch {
            if nanos == 0 {
                (-seconds, 0)
            } else {
                (-seconds - 1, (1_000_000_000 - nanos) / 1000)
            }
        } else {
            (seconds, nanos / 1000)
        };
        Ok(Self {
            seconds: i64::try_from(seconds).map_err(|_| "Guest wallclock seconds overflow")?,
            microseconds: i64::from(micros),
        })
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub enum TimeSource {
    #[default]
    HostRealtime,
    Fixed(Timeval),
}
#[derive(Debug)]
pub struct Write {
    pub address: u64,
    pub bytes: Vec<u8>,
}
#[derive(Debug)]
pub struct Plan {
    pub writes: Vec<Write>,
}
#[derive(Debug, Default)]
pub struct GuestTime {
    source: TimeSource,
    utc_timezone: bool,
}
impl GuestTime {
    /// Explicit guest timezone policy; never reads the host's local timezone.
    pub fn configure_utc(&mut self) {
        self.utc_timezone = true;
    }
    /// Fixed source is for deterministic verification, not a ticking guest clock.
    pub fn set_source(&mut self, source: TimeSource) {
        self.source = source;
    }
    fn sample(&self) -> Result<Timeval> {
        match self.source {
            TimeSource::Fixed(time) => Ok(time),
            TimeSource::HostRealtime => match SystemTime::now().duration_since(UNIX_EPOCH) {
                Ok(d) => Timeval::from_epoch_duration(d, false),
                Err(e) => Timeval::from_epoch_duration(e.duration(), true),
            },
        }
    }
    pub fn plan(&self, timeval: u64, timezone: u64) -> Result<Plan> {
        if timezone != 0 && !self.utc_timezone {
            return Err("Guest timezone requires explicit UTC configuration".into());
        }
        let mut writes = Vec::new();
        if timeval != 0 {
            timeval
                .checked_add(16)
                .ok_or("Guest timeval address overflow")?;
            writes.push(Write {
                address: timeval,
                bytes: self.sample()?.bytes()?.to_vec(),
            });
        }
        if timezone != 0 {
            timezone
                .checked_add(8)
                .ok_or("Guest timezone address overflow")?;
            writes.push(Write {
                address: timezone,
                bytes: vec![0; 8],
            });
        }
        Ok(Plan { writes })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_epoch_conversion() {
        for (d, before, seconds, microseconds) in [
            (Duration::new(0, 1), false, 0, 0),
            (Duration::new(0, 1), true, -1, 999999),
            (Duration::new(5, 1001), true, -6, 999998),
            (Duration::new(5, 0), true, -5, 0),
            (Duration::new(5, 999999999), false, 5, 999999),
        ] {
            assert_eq!(
                Timeval::from_epoch_duration(d, before).unwrap(),
                Timeval {
                    seconds,
                    microseconds
                }
            );
        }
        assert!(Timeval::from_epoch_duration(Duration::new(u64::MAX, 0), false).is_err());
        assert!(
            Timeval {
                seconds: 0,
                microseconds: -1
            }
            .bytes()
            .is_err()
        );
        assert!(
            Timeval {
                seconds: 0,
                microseconds: 1_000_000
            }
            .bytes()
            .is_err()
        );
    }
    #[test]
    fn nullable_outputs_explicit_timezone_and_order() {
        let mut clock = GuestTime::default();
        clock.set_source(TimeSource::Fixed(Timeval {
            seconds: 0x1_0000_0001,
            microseconds: 123456,
        }));
        assert!(clock.plan(0, 0).unwrap().writes.is_empty());
        assert!(clock.plan(0, 0x1000).is_err());
        let p = clock.plan(0x1_0000_0001, 0).unwrap();
        assert_eq!(&p.writes[0].bytes[..8], &0x1_0000_0001i64.to_le_bytes());
        clock.configure_utc();
        let p = clock.plan(0x1000, 0x1008).unwrap();
        assert_eq!(p.writes.len(), 2);
        assert_eq!(p.writes[0].address, 0x1000);
        assert_eq!(p.writes[1].address, 0x1008);
        assert_eq!(p.writes[1].bytes, [0; 8]);
        assert_eq!(clock.plan(0, 0x1000).unwrap().writes.len(), 1);
        assert!(clock.plan(u64::MAX, 0).is_err());
        assert!(clock.plan(0, u64::MAX).is_err());
        clock.set_source(TimeSource::Fixed(Timeval {
            seconds: 0,
            microseconds: -1,
        }));
        assert!(clock.plan(0, 0).unwrap().writes.is_empty());
        assert!(clock.plan(1, 0).is_err());
    }
}
