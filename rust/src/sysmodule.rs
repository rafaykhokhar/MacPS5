//! Sysmodule references attach to real started modules, not success-only stubs.
//! The host registers ID associations explicitly; this is not a complete ID table
//! or guest sceSysmodule API. See AnyPS5's libSceSysmodule for ABI reference.
use super::{Modules, Phase};
use crate::Result;
impl Modules {
    pub fn bind_sysmodule(&mut self, id: u32, handle: u64) -> Result<()> {
        if id & 0x7fff_ffff == 0
            || self.sysmodules.contains_key(&id)
            || self.sysmodules.len() >= 256
        {
            return Err("Invalid, duplicate or over-budget sysmodule ID".into());
        }
        if self.phase(handle) != Some(Phase::Started) {
            return Err("Sysmodule provider has not started".into());
        }
        self.sysmodules.insert(id, (handle, 0));
        Ok(())
    }
    /// Acquire only an existing, successfully initialized provider. The caller
    /// must perform actual loading first when no binding exists.
    pub fn retain_sysmodule(&mut self, id: u32) -> Result<u64> {
        let &(handle, count) = self.sysmodules.get(&id).ok_or("Unknown sysmodule ID")?;
        if self.phase(handle) != Some(Phase::Started) {
            return Err("Sysmodule provider is not running".into());
        }
        let count = count.checked_add(1).ok_or("Sysmodule reference overflow")?;
        self.sysmodules.insert(id, (handle, count));
        Ok(handle)
    }
    /// Releasing the last reference removes its pin, but does not pretend that
    /// module stop/finalization has executed. Normal lifecycle APIs handle that.
    pub fn release_sysmodule(&mut self, id: u32) -> Result<()> {
        let entry = self.sysmodules.get_mut(&id).ok_or("Unknown sysmodule ID")?;
        entry.1 = entry.1.checked_sub(1).ok_or("Sysmodule is not retained")?;
        Ok(())
    }
    pub fn sysmodule_loaded(&self, id: u32) -> bool {
        self.sysmodules.get(&id).is_some_and(|&(handle, count)| {
            count != 0 && self.phase(handle) == Some(Phase::Started)
        })
    }
    pub(super) fn check_sysmodule_pin(&self, handle: u64) -> Result<()> {
        if self
            .sysmodules
            .values()
            .any(|&(owner, count)| owner == handle && count != 0)
        {
            return Err("Sysmodule reference pins provider".into());
        }
        Ok(())
    }
}
