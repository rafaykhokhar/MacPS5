//! Bounded callback requests. No guest callback executes on a Rust host stack.
use crate::Result;
use std::rc::Rc;
pub const MAX_ELEMENTS: u64 = 1_048_576;
pub const MAX_ELEMENT_BYTES: u64 = 65_536;
pub const MAX_ARRAY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EVENTS: u64 = 128 * MAX_ELEMENTS;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Compare,
    Swap,
}
/// Deliberately non-Clone: a comparator result or completed swap is consumed once.
pub struct Request {
    owner: Rc<()>,
    sequence: u64,
    kind: Kind,
}
pub enum Event {
    Compare {
        request: Request,
        left: u64,
        right: u64,
    },
    Swap {
        request: Request,
        left: u64,
        right: u64,
        size: u64,
    },
    Done(u64),
}
pub(crate) struct Gate {
    owner: Rc<()>,
    sequence: u64,
    pending: Option<Kind>,
}
impl Gate {
    pub fn new() -> Self {
        Self {
            owner: Rc::new(()),
            sequence: 0,
            pending: None,
        }
    }
    pub fn idle(&self) -> Result<()> {
        if self.pending.is_some() {
            return Err("Comparator engine has an outstanding request".into());
        }
        Ok(())
    }
    pub fn issue(&mut self, kind: Kind) -> Result<Request> {
        self.idle()?;
        if self.sequence == MAX_EVENTS {
            return Err("Comparator engine event budget exhausted".into());
        }
        self.sequence += 1;
        self.pending = Some(kind);
        Ok(Request {
            owner: self.owner.clone(),
            sequence: self.sequence,
            kind,
        })
    }
    pub fn validate(&self, request: &Request, kind: Kind) -> Result<()> {
        if !Rc::ptr_eq(&self.owner, &request.owner)
            || request.sequence != self.sequence
            || request.kind != kind
            || self.pending != Some(kind)
        {
            return Err("Foreign, stale or mismatched comparator request".into());
        }
        Ok(())
    }
    pub fn accept(&mut self, request: Request, kind: Kind) -> Result<()> {
        self.validate(&request, kind)?;
        self.pending = None;
        Ok(())
    }
}
pub(crate) fn extent(base: u64, count: u64, size: u64) -> Result<()> {
    if size == 0 {
        return Err("Nontrivial zero-size comparator array is unsupported".into());
    }
    let bytes = count
        .checked_mul(size)
        .ok_or("Comparator array multiplication overflow")?;
    base.checked_add(bytes)
        .ok_or("Comparator array address overflow")?;
    if count > MAX_ELEMENTS || size > MAX_ELEMENT_BYTES || bytes > MAX_ARRAY_BYTES {
        return Err("Comparator array exceeds bounded engine limits".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_owner_kind_and_replay() {
        let mut a = Gate::new();
        let mut b = Gate::new();
        let request = a.issue(Kind::Compare).unwrap();
        assert!(a.issue(Kind::Swap).is_err());
        assert!(b.validate(&request, Kind::Compare).is_err());
        assert!(a.validate(&request, Kind::Swap).is_err());
        // Private test-only forgery verifies runtime sequence defense in addition to non-Clone API.
        let replay = Request {
            owner: request.owner.clone(),
            sequence: request.sequence,
            kind: request.kind,
        };
        a.accept(request, Kind::Compare).unwrap();
        assert!(a.accept(replay, Kind::Compare).is_err());
        let other = b.issue(Kind::Swap).unwrap();
        assert!(a.accept(other, Kind::Swap).is_err());
    }
}
