//! Explicit guest hardware reporting, independent of host capacity and allocation quotas.
//!
//! Selectors follow ps5-payload-dev/sdk 10b539de's include/freebsd/unistd.h.
//! Return/error distinctions follow FreeBSD releng/11.4 lib/libc/gen/sysconf.c.
//! This profile does not enforce an aggregate memory budget: images, stacks, heap,
//! direct backing and GPU resources currently retain their separate limits.

use crate::Result;

pub const PAGE_SIZE: u64 = 16_384;

/// Immutable reported virtual hardware. Capacity is not free memory or an
/// allocation guarantee. Configure explicitly before executing guest code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestSystemProfile {
    reported_physical_bytes: u64,
    configured_cpus: u32,
    online_cpus: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    Value(i64),
    Invalid,
    Unsupported,
}

impl GuestSystemProfile {
    pub fn new(
        reported_physical_bytes: u64,
        configured_cpus: u32,
        online_cpus: u32,
    ) -> Result<Self> {
        if reported_physical_bytes == 0 || reported_physical_bytes % PAGE_SIZE != 0 {
            return Err(
                "Guest reported physical capacity must be positive and page aligned".into(),
            );
        }
        // The current scheduler has exactly one cooperative execution lane.
        if configured_cpus != 1 || online_cpus != 1 {
            return Err("Guest CPU profile requires the supported single execution lane".into());
        }
        Ok(Self {
            reported_physical_bytes,
            configured_cpus,
            online_cpus,
        })
    }

    pub fn reported_physical_bytes(&self) -> u64 {
        self.reported_physical_bytes
    }

    pub fn query(&self, selector: i32) -> Query {
        match selector {
            47 => Query::Value(PAGE_SIZE as i64),
            57 => Query::Value(i64::from(self.configured_cpus)),
            58 => Query::Value(i64::from(self.online_cpus)),
            // u64 capacity divided by 16384 always fits the guest signed long.
            121 => Query::Value((self.reported_physical_bytes / PAGE_SIZE) as i64),
            _ => classify_unimplemented(selector),
        }
    }
}

/// SDK-assigned names remain explicit unsupported boundaries; they must not
/// accidentally become EINVAL or false feature-support claims. In particular,
/// some recognized sysconf names can return -1 without setting errno.
pub fn classify_unimplemented(selector: i32) -> Query {
    match selector {
        1..=52 | 56..=122 => Query::Unsupported,
        _ => Query::Invalid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_capacity_and_execution_lane() {
        let profile = GuestSystemProfile::new(16 << 30, 1, 1).unwrap();
        assert_eq!(profile.query(47), Query::Value(16_384));
        assert_eq!(profile.query(57), Query::Value(1));
        assert_eq!(profile.query(58), Query::Value(1));
        assert_eq!(profile.query(121), Query::Value(1_048_576));
        assert_eq!(profile.reported_physical_bytes(), 16 << 30);
        for capacity in [0, 1, PAGE_SIZE - 1, PAGE_SIZE + 1] {
            assert!(GuestSystemProfile::new(capacity, 1, 1).is_err());
        }
        for cpus in [(0, 1), (1, 0), (2, 1), (2, 2), (1, 2)] {
            assert!(GuestSystemProfile::new(PAGE_SIZE, cpus.0, cpus.1).is_err());
        }
    }
    #[test]
    fn long_width_and_selector_holes() {
        let bytes = !(PAGE_SIZE - 1);
        let profile = GuestSystemProfile::new(bytes, 1, 1).unwrap();
        assert_eq!(profile.query(121), Query::Value((bytes / PAGE_SIZE) as i64));
        assert!(bytes / PAGE_SIZE > u32::MAX as u64);
        for selector in [1, 49, 50, 56, 59, 122] {
            assert_eq!(profile.query(selector), Query::Unsupported);
        }
        for selector in [i32::MIN, -1, 0, 53, 54, 55, 123, i32::MAX] {
            assert_eq!(profile.query(selector), Query::Invalid);
        }
    }
}
