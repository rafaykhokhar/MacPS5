//! Read-only macOS GameController snapshots, independent of the guest Pad ABI.
//! Host main-thread polling must run alongside the application's event loop.
//! An empty snapshot means no extended controllers are currently enumerated,
//! not proof that Bluetooth discovery or event delivery has completed.
//! https://developer.apple.com/documentation/gamecontroller/gccontroller
//! https://developer.apple.com/documentation/gamecontroller/gccontroller/capture()
//! https://developer.apple.com/documentation/gamecontroller/gccontroller/extendedgamepad
#![allow(unsafe_code)]
use crate::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceSnapshot {
    /// Opaque hub-local ID; never reused, including after a disconnect.
    pub id: u64,
    pub snapshot: Snapshot,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub devices: Vec<DeviceSnapshot>,
    pub disconnected: Vec<u64>,
}

/// Main-thread owner of retained controller identities. Deliberately !Send and
/// !Sync; copied snapshots may be passed to the guest execution thread.
/// Disconnects are observed at poll boundaries; a disconnect/reconnect entirely
/// between polls cannot be distinguished if Apple reuses the same object.
pub struct ControllerHub {
    identities: Identities,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    retained: Vec<apple::Retained>,
    _main_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl ControllerHub {
    pub fn new() -> Result<Self> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        apple::require_main_thread()?;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        return Err("GameController polling requires Apple Silicon macOS".into());
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        Ok(Self {
            identities: Identities::default(),
            retained: Vec::new(),
            _main_thread: std::marker::PhantomData,
        })
    }
    pub fn poll(&mut self) -> Result<Update> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let observed = apple::poll()?;
            let keys: Vec<_> = observed.iter().map(|(object, _)| object.key()).collect();
            let (ids, disconnected) = self.identities.reconcile(&keys)?;
            let mut retained = Vec::with_capacity(observed.len());
            let mut devices = Vec::with_capacity(observed.len());
            for ((object, snapshot), id) in observed.into_iter().zip(ids) {
                retained.push(object);
                devices.push(DeviceSnapshot { id, snapshot });
            }
            self.retained = retained;
            Ok(Update {
                devices,
                disconnected,
            })
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            Err("GameController polling requires Apple Silicon macOS".into())
        }
    }
}

#[derive(Default)]
struct Identities {
    active: Vec<(usize, u64)>,
    last: u64,
}
impl Identities {
    fn reconcile(&mut self, keys: &[usize]) -> Result<(Vec<u64>, Vec<u64>)> {
        if keys.len() > 16
            || keys
                .iter()
                .enumerate()
                .any(|(i, key)| *key == 0 || keys[..i].contains(key))
        {
            return Err("Invalid or excessive GameController identities".into());
        }
        let mut last = self.last;
        let mut next = Vec::with_capacity(keys.len());
        for key in keys {
            let id = if let Some((_, id)) = self.active.iter().find(|(old, _)| old == key) {
                *id
            } else {
                last = last
                    .checked_add(1)
                    .ok_or("GameController ID space exhausted")?;
                last
            };
            next.push((*key, id));
        }
        let disconnected = self
            .active
            .iter()
            .filter(|(key, _)| !keys.contains(key))
            .map(|(_, id)| *id)
            .collect();
        let ids = next.iter().map(|(_, id)| *id).collect();
        self.active = next;
        self.last = last;
        Ok((ids, disconnected))
    }
}

/// Values retain Apple's normalized ranges and direction conventions. No guest
/// button mask, deadzone, user assignment or persistent handle is invented.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Index in this enumeration only; not stable across polling calls.
    pub enumeration_index: usize,
    pub left_stick: [f32; 2],
    pub right_stick: [f32; 2],
    pub dpad: [f32; 2],
    /// A, B, X, Y in Apple's extended gamepad profile.
    pub face: [f32; 4],
    pub shoulders: [f32; 2],
    pub triggers: [f32; 2],
}

pub fn poll() -> Result<Vec<Snapshot>> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        apple::poll().map(|values| values.into_iter().map(|(_, snapshot)| snapshot).collect())
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    {
        Err("GameController polling requires Apple Silicon macOS".into())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use super::*;
    use std::ffi::{c_char, c_void};
    type Obj = *mut c_void;
    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_msgSend();
        fn objc_getClass(name: *const c_char) -> Obj;
        fn sel_registerName(name: *const c_char) -> Obj;
    }
    #[link(name = "Foundation", kind = "framework")]
    unsafe extern "C" {}
    #[link(name = "GameController", kind = "framework")]
    unsafe extern "C" {}
    macro_rules! msg {
        ($object:expr, $selector:literal => $ret:ty $(, $ty:ty => $arg:expr)*) => {{
            let selector = sel_registerName(concat!($selector, "\0").as_ptr().cast());
            let call: unsafe extern "C" fn(Obj, Obj $(, $ty)*) -> $ret = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            call($object, selector $(, $arg)*)
        }};
    }
    struct Pool(Obj);
    pub(super) struct Retained(Obj);
    impl Retained {
        pub(super) fn key(&self) -> usize {
            self.0 as usize
        }
    }
    impl Drop for Retained {
        fn drop(&mut self) {
            unsafe {
                msg!(self.0, "release" => ());
            }
        }
    }
    impl Drop for Pool {
        fn drop(&mut self) {
            unsafe {
                msg!(self.0, "drain" => ());
            }
        }
    }
    fn checked(object: Obj) -> Result<Obj> {
        if object.is_null() {
            Err("GameController returned a missing required object".into())
        } else {
            Ok(object)
        }
    }
    unsafe fn value(element: Obj, axis: bool) -> Result<f32> {
        let element = checked(element)?;
        let value = unsafe { msg!(element, "value" => f32) };
        let lower = if axis { -1.0 } else { 0.0 };
        if !value.is_finite() || !(lower..=1.0).contains(&value) {
            return Err("GameController returned out-of-range input".into());
        }
        Ok(value)
    }
    unsafe fn axes(element: Obj) -> Result<[f32; 2]> {
        let element = checked(element)?;
        unsafe {
            Ok([
                value(msg!(element, "xAxis" => Obj), true)?,
                value(msg!(element, "yAxis" => Obj), true)?,
            ])
        }
    }
    pub(super) fn require_main_thread() -> Result<()> {
        unsafe {
            let thread = checked(objc_getClass(c"NSThread".as_ptr()))?;
            if !msg!(thread, "isMainThread" => bool) {
                return Err("GameController polling requires the host main thread".into());
            }
        }
        Ok(())
    }
    pub(super) fn poll() -> Result<Vec<(Retained, Snapshot)>> {
        // SAFETY: fixed SDK Foundation/GameController signatures on ARM64;
        // only framework-owned host objects, contained within an autorelease
        // pool, are messaged. Original controllers are explicitly retained for
        // the hub; temporary captures stay in the pool. No guest pointers enter.
        unsafe {
            require_main_thread()?;
            let _pool = Pool(checked(
                msg!(checked(objc_getClass(c"NSAutoreleasePool".as_ptr()))?, "new" => Obj),
            )?);
            let class = checked(objc_getClass(c"GCController".as_ptr()))?;
            let controllers = checked(msg!(class, "controllers" => Obj))?;
            let count = msg!(controllers, "count" => usize);
            if count > 16 {
                return Err("GameController enumeration exceeds model limit16".into());
            }
            let mut results = Vec::with_capacity(count);
            for index in 0..count {
                let controller =
                    checked(msg!(controllers, "objectAtIndex:" => Obj, usize => index))?;
                let captured = checked(msg!(controller, "capture" => Obj))?;
                let profile = msg!(captured, "extendedGamepad" => Obj);
                if profile.is_null() {
                    continue;
                }
                let retained = Retained(checked(msg!(controller,"retain" => Obj))?);
                results.push((
                    retained,
                    Snapshot {
                        enumeration_index: index,
                        left_stick: axes(msg!(profile, "leftThumbstick" => Obj))?,
                        right_stick: axes(msg!(profile, "rightThumbstick" => Obj))?,
                        dpad: axes(msg!(profile, "dpad" => Obj))?,
                        face: [
                            value(msg!(profile,"buttonA" => Obj), false)?,
                            value(msg!(profile,"buttonB" => Obj), false)?,
                            value(msg!(profile,"buttonX" => Obj), false)?,
                            value(msg!(profile,"buttonY" => Obj), false)?,
                        ],
                        shoulders: [
                            value(msg!(profile,"leftShoulder" => Obj), false)?,
                            value(msg!(profile,"rightShoulder" => Obj), false)?,
                        ],
                        triggers: [
                            value(msg!(profile,"leftTrigger" => Obj), false)?,
                            value(msg!(profile,"rightTrigger" => Obj), false)?,
                        ],
                    },
                ));
            }
            Ok(results)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_reorder_but_not_disconnect_or_pointer_reuse() {
        let mut ids = Identities::default();
        assert_eq!(ids.reconcile(&[10, 20]).unwrap(), (vec![1, 2], vec![]));
        assert_eq!(ids.reconcile(&[20, 10]).unwrap(), (vec![2, 1], vec![]));
        assert_eq!(ids.reconcile(&[20]).unwrap(), (vec![2], vec![1]));
        assert_eq!(ids.reconcile(&[10, 20]).unwrap(), (vec![3, 2], vec![]));
        assert_eq!(ids.reconcile(&[]).unwrap(), (vec![], vec![3, 2]));
        assert_eq!(ids.reconcile(&[]).unwrap(), (vec![], vec![]));
    }
    #[test]
    fn failed_enumeration_preserves_active_ids_and_counter() {
        let mut ids = Identities::default();
        ids.reconcile(&[10]).unwrap();
        for keys in [vec![10, 10], vec![0], (1..=17).collect()] {
            assert!(ids.reconcile(&keys).is_err());
            assert_eq!(ids.active, [(10, 1)]);
            assert_eq!(ids.last, 1);
        }
        ids.last = u64::MAX;
        assert!(ids.reconcile(&[10, 20]).is_err());
        assert_eq!(ids.active, [(10, 1)]);
        assert_eq!(ids.last, u64::MAX);
        assert_eq!(ids.reconcile(&[10]).unwrap(), (vec![1], vec![]));
    }
}
