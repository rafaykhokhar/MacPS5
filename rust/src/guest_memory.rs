//! Borrowed guest mappings. Host pointers are never derived from guest addresses.
use crate::Result;
/// Unforgeable identity for runtime-private, guest-readable storage.
/// There is deliberately no whole-call protection bypass: each internal write
/// must supply its owner, while guest-directed writes retain normal checks.
#[derive(Clone)]
pub struct RuntimeOwner(std::rc::Rc<()>);
impl RuntimeOwner {
    pub(crate) fn new() -> Self {
        Self(std::rc::Rc::new(()))
    }
    fn matches(&self, other: &Self) -> bool {
        std::rc::Rc::ptr_eq(&self.0, &other.0)
    }
}

enum Bytes<'a> {
    Borrowed(&'a mut [u8]),
    Owned(Vec<u8>),
}
impl std::ops::Deref for Bytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Self::Borrowed(b) => b,
            Self::Owned(b) => b,
        }
    }
}
impl std::ops::DerefMut for Bytes<'_> {
    fn deref_mut(&mut self) -> &mut [u8] {
        match self {
            Self::Borrowed(b) => b,
            Self::Owned(b) => b,
        }
    }
}
/// Process-owned anonymous mappings survive dispatcher returns and suspension.
#[derive(Default)]
pub struct OwnedMappings {
    regions: Vec<OwnedRegion>,
}
struct OwnedRegion {
    address: u64,
    bytes: Vec<u8>,
    writable: bool,
    heap: bool,
    direct: bool,
    runtime_owner: Option<RuntimeOwner>,
}
impl OwnedMappings {
    pub fn bytes_allocated(&self) -> usize {
        self.regions.iter().map(|r| r.bytes.len()).sum()
    }
}
/// Borrowed process data, including a suspended caller's stack. Never executable.
pub struct DataRegion<'a> {
    pub address: u64,
    pub bytes: &'a mut [u8],
    pub writable: bool,
}
struct Region<'a> {
    heap: bool,
    direct: bool,
    service_data: bool,
    runtime_owner: Option<RuntimeOwner>,
    address: u64,
    bytes: Bytes<'a>,
    writable: bool,
    executable: bool,
}
#[derive(Default)]
pub struct Memory<'a> {
    regions: Vec<Region<'a>>,
    owned: Option<&'a mut OwnedMappings>,
    reserved: Vec<u64>,
    write_barriers: Vec<(u64, usize)>,
}
impl Drop for Memory<'_> {
    fn drop(&mut self) {
        if let Some(owner) = self.owned.as_mut() {
            for r in self.regions.drain(..) {
                if let Bytes::Owned(bytes) = r.bytes {
                    owner.regions.push(OwnedRegion {
                        address: r.address,
                        bytes,
                        writable: r.writable,
                        heap: r.heap,
                        direct: r.direct,
                        runtime_owner: r.runtime_owner,
                    });
                }
            }
        }
    }
}
impl<'a> Memory<'a> {
    pub(crate) fn set_write_barriers(&mut self, ranges: Vec<(u64, usize)>) {
        self.write_barriers = ranges;
    }
    pub(crate) fn owned_region_range(&self, address: u64) -> Option<(u64, usize)> {
        self.regions
            .iter()
            .find(|r| {
                matches!(r.bytes, Bytes::Owned(_))
                    && address >= r.address
                    && address - r.address < r.bytes.len() as u64
            })
            .map(|r| (r.address, r.bytes.len()))
    }

    /// Validate an owned arena's complete reservation before publishing it.
    pub fn validate_unmapped(&self, address: u64, length: usize) -> crate::Result<()> {
        let end = address
            .checked_add(length as u64)
            .ok_or("Mapping reservation overflow")?;
        if length == 0
            || self
                .regions
                .iter()
                .any(|r| address < r.address + r.bytes.len() as u64 && r.address < end)
            || self.reserved.iter().any(|&p| address <= p && p < end)
        {
            return Err("Owned arena reservation overlaps guest mapping or service".into());
        }
        Ok(())
    }
    pub fn reserve_service(&mut self, address: u64) {
        self.reserved.push(address);
    }
    pub fn owned_allocation_contains(&self, address: u64, length: usize) -> bool {
        let Some(end) = address.checked_add(length as u64) else {
            return false;
        };
        self.owned.is_some()
            && self.regions.iter().any(|r| {
                matches!(r.bytes, Bytes::Owned(_))
                    && address >= r.address
                    && end <= r.address + r.bytes.len() as u64
            })
    }
    pub fn anonymous_release_size(
        &self,
        address: u64,
        length: u64,
    ) -> std::result::Result<usize, u32> {
        if address % 16384 != 0 || length == 0 || length.checked_add(16383).is_none() {
            return Err(22);
        }
        let size = (length + 16383) & !16383;
        self.regions
            .iter()
            .find(|r| {
                r.address == address
                    && !r.heap
                    && !r.direct
                    && r.bytes.len() as u64 == size
                    && matches!(r.bytes, Bytes::Owned(_))
            })
            .map(|r| r.bytes.len())
            .ok_or(22)
    }
    pub fn attach_owned(&mut self, owner: &'a mut OwnedMappings) -> Result<()> {
        if self.owned.is_some() {
            return Err("Anonymous mapping owner already attached".into());
        }
        for r in &owner.regions {
            let address = r.address;
            let end = address + r.bytes.len() as u64;
            if self
                .regions
                .iter()
                .any(|r| address < r.address + r.bytes.len() as u64 && r.address < end)
            {
                return Err("Anonymous mapping overlaps process storage".into());
            }
        }
        for OwnedRegion {
            address,
            bytes,
            writable,
            heap,
            direct,
            runtime_owner,
        } in owner.regions.drain(..)
        {
            self.regions.push(Region {
                heap,
                direct,
                service_data: false,
                runtime_owner,
                address,
                bytes: Bytes::Owned(bytes),
                writable,
                executable: false,
            });
        }
        self.owned = Some(owner);
        Ok(())
    }
    pub fn map_anonymous(&mut self, args: [u64; 6]) -> std::result::Result<u64, u32> {
        let [hint, length, protection, flags, fd, offset] = args;
        const PAGE: u64 = 16384;
        if self.owned.is_none()
            || hint != 0
            || length == 0
            || length.checked_add(PAGE - 1).is_none()
            || !matches!(protection, 1 | 3)
            || flags != 0x1002
            || fd as u32 != u32::MAX
            || offset != 0
        {
            return Err(22);
        }
        let size = (length + PAGE - 1) & !(PAGE - 1);
        self.allocate_owned(size, PAGE, protection == 3, false, false)
    }
    pub fn malloc(&mut self, size: u64) -> std::result::Result<u64, u32> {
        let size = size.max(1).checked_add(15).ok_or(12u32)? & !15;
        self.allocate_owned(size, 16, true, true, false)
    }
    pub fn calloc(&mut self, count: u64, size: u64) -> std::result::Result<u64, u32> {
        self.malloc(count.checked_mul(size).ok_or(12u32)?)
    }
    /// Rounded usable capacity of an exact-base owned heap allocation.
    /// Borrowed, anonymous, direct and interior addresses are never heap bases.
    pub fn heap_allocation_size(&self, address: u64) -> Result<usize> {
        self.regions
            .iter()
            .find(|r| {
                r.address == address
                    && r.heap
                    && r.runtime_owner.is_none()
                    && matches!(r.bytes, Bytes::Owned(_))
            })
            .map(|r| r.bytes.len())
            .ok_or_else(|| "Invalid guest heap allocation base".into())
    }
    pub fn free(&mut self, address: u64) -> Result<()> {
        if address == 0 {
            return Ok(());
        }
        let index = self
            .regions
            .iter()
            .position(|r| {
                r.address == address
                    && r.heap
                    && r.runtime_owner.is_none()
                    && matches!(r.bytes, Bytes::Owned(_))
            })
            .ok_or("Invalid guest heap free")?;
        self.regions.remove(index);
        Ok(())
    }
    /// Claim an exact heap allocation, including allocator-rounded padding.
    pub fn mark_runtime_heap(&mut self, address: u64, owner: &RuntimeOwner) -> Result<()> {
        let r = self
            .regions
            .iter_mut()
            .find(|r| r.address == address && r.heap && matches!(r.bytes, Bytes::Owned(_)))
            .ok_or("Invalid runtime heap base")?;
        if r.runtime_owner
            .as_ref()
            .is_some_and(|old| !old.matches(owner))
        {
            return Err("Runtime storage owner mismatch".into());
        }
        r.runtime_owner = Some(owner.clone());
        Ok(())
    }
    pub fn runtime_heap_allocation_size(
        &self,
        address: u64,
        owner: &RuntimeOwner,
    ) -> Result<usize> {
        let r = self
            .regions
            .iter()
            .find(|r| r.address == address && r.heap && matches!(r.bytes, Bytes::Owned(_)))
            .ok_or("Invalid runtime heap base")?;
        if !r
            .runtime_owner
            .as_ref()
            .is_some_and(|old| old.matches(owner))
        {
            return Err("Runtime storage owner mismatch".into());
        }
        Ok(r.bytes.len())
    }
    pub fn runtime_free(&mut self, address: u64, owner: &RuntimeOwner) -> Result<()> {
        self.runtime_heap_allocation_size(address, owner)?;
        let index = self
            .regions
            .iter()
            .position(|r| r.address == address)
            .unwrap();
        self.regions.remove(index);
        Ok(())
    }
    /// Re-map an executor-owned static pool on each Memory reconstruction.
    pub fn add_runtime_data(
        &mut self,
        address: u64,
        bytes: &'a mut [u8],
        owner: &RuntimeOwner,
    ) -> Result<()> {
        self.add(address, bytes, true, false)?;
        self.regions.last_mut().unwrap().runtime_owner = Some(owner.clone());
        Ok(())
    }
    pub fn validate_runtime_write(
        &self,
        address: u64,
        size: usize,
        owner: &RuntimeOwner,
    ) -> Result<()> {
        self.validate_write_for(address, size, Some(owner))
    }
    pub fn runtime_write(
        &mut self,
        address: u64,
        bytes: &[u8],
        owner: &RuntimeOwner,
    ) -> Result<()> {
        self.validate_runtime_write(address, bytes.len(), owner)?;
        if bytes.is_empty() {
            return Ok(());
        }
        let index = self.region(address, bytes.len())?;
        let r = &mut self.regions[index];
        let p = (address - r.address) as usize;
        r.bytes[p..p + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    pub fn runtime_store(
        &mut self,
        address: u64,
        value: u64,
        size: u64,
        owner: &RuntimeOwner,
    ) -> Result<()> {
        if ![1, 2, 4, 8].contains(&size) {
            return Err("Invalid scalar width".into());
        }
        self.runtime_write(address, &value.to_le_bytes()[..size as usize], owner)
    }
    fn allocate_owned(
        &mut self,
        size: u64,
        alignment: u64,
        writable: bool,
        heap: bool,
        direct: bool,
    ) -> std::result::Result<u64, u32> {
        if self.owned.is_none() {
            return Err(12);
        }
        let (count, total) = self
            .regions
            .iter()
            .filter(|r| matches!(r.bytes, Bytes::Owned(_)))
            .fold((0, 0u64), |(n, t), r| {
                (n + usize::from(r.heap == heap), t + r.bytes.len() as u64)
            });
        if size
            > if direct {
                64 * 1024 * 1024
            } else {
                16 * 1024 * 1024
            }
            || size > (256 * 1024 * 1024u64).saturating_sub(total)
            || count >= if heap { 4096 } else { 256 }
        {
            return Err(12);
        }
        let mut address = 0x400000000u64;
        loop {
            let end = address.checked_add(size).ok_or(12u32)?;
            let conflict = self
                .regions
                .iter()
                .filter(|r| address < r.address + r.bytes.len() as u64 && r.address < end)
                .map(|r| r.address + r.bytes.len() as u64)
                .chain(
                    self.reserved
                        .iter()
                        .filter(|&&p| p >= address && p < end)
                        .map(|p| p + 1),
                )
                .max();
            match conflict {
                Some(next) => {
                    address = next.checked_add(alignment - 1).ok_or(12u32)? & !(alignment - 1)
                }
                None => break,
            }
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size as usize).map_err(|_| 12u32)?;
        bytes.resize(size as usize, 0);
        self.regions.push(Region {
            heap,
            direct,
            service_data: false,
            runtime_owner: None,
            address,
            bytes: Bytes::Owned(bytes),
            writable,
            executable: false,
        });
        Ok(address)
    }
    pub fn unmap_anonymous(&mut self, address: u64, length: u64) -> std::result::Result<u64, u32> {
        if address % 16384 != 0 || length == 0 || length.checked_add(16383).is_none() {
            return Err(22);
        }
        let size = (length + 16383) & !16383;
        let index = self
            .regions
            .iter()
            .position(|r| {
                r.address == address
                    && !r.heap
                    && !r.direct
                    && r.bytes.len() as u64 == size
                    && matches!(r.bytes, Bytes::Owned(_))
            })
            .ok_or(22u32)?;
        self.regions.remove(index);
        Ok(0)
    }
    /// Dedicated non-executable direct-memory backing; ordinary munmap/free
    /// cannot revoke it behind the direct-memory owner's lifetime checks.
    pub(crate) fn map_direct_owned(&mut self, size: u64, alignment: u64) -> Result<u64> {
        if size == 0
            || size % 16384 != 0
            || !alignment.is_power_of_two()
            || !(16384..=0x20000).contains(&alignment)
        {
            return Err("Unsupported direct-memory size/alignment".into());
        }
        self.allocate_owned(size, alignment, true, false, true)
            .map_err(|error| format!("Direct-memory backing allocation failed: {error}"))
    }
    pub(crate) fn unmap_direct_owned(&mut self, address: u64, size: u64) -> Result<()> {
        let index = self
            .regions
            .iter()
            .position(|r| {
                r.direct
                    && r.address == address
                    && r.bytes.len() as u64 == size
                    && matches!(r.bytes, Bytes::Owned(_))
            })
            .ok_or("Unknown direct-memory backing")?;
        self.regions.remove(index);
        Ok(())
    }
    pub fn add(
        &mut self,
        address: u64,
        bytes: &'a mut [u8],
        writable: bool,
        executable: bool,
    ) -> Result<()> {
        let end = address
            .checked_add(bytes.len() as u64)
            .ok_or("Guest mapping overflow")?;
        if bytes.is_empty()
            || self
                .regions
                .iter()
                .any(|r| address < r.address + r.bytes.len() as u64 && r.address < end)
        {
            return Err("Empty or overlapping guest mapping".into());
        }
        // Exclusive Rust borrows prohibit overlapping backing allocations.
        self.regions.push(Region {
            heap: false,
            direct: false,
            service_data: false,
            runtime_owner: None,
            address,
            bytes: Bytes::Borrowed(bytes),
            writable,
            executable,
        });
        Ok(())
    }
    pub(crate) fn add_service_data(&mut self, address: u64, bytes: &'a mut [u8]) -> Result<()> {
        self.add(address, bytes, false, false)?;
        self.regions.last_mut().unwrap().service_data = true;
        Ok(())
    }
    pub(crate) fn update_service_data(&mut self, address: u64, bytes: &[u8]) -> Result<()> {
        let index = self.region(address, bytes.len())?;
        let region = &mut self.regions[index];
        if !region.service_data {
            return Err("Not service-owned storage".into());
        }
        let offset = (address - region.address) as usize;
        region.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    fn region(&self, address: u64, size: usize) -> Result<usize> {
        if size == 0 {
            return Err("Empty guest access".into());
        }
        self.regions
            .iter()
            .position(|r| {
                address >= r.address
                    && address - r.address < r.bytes.len() as u64
                    && size as u64 <= r.bytes.len() as u64 - (address - r.address)
            })
            .ok_or_else(|| "Guest access outside mapping".into())
    }
    pub fn read(&self, address: u64, size: usize) -> Result<&[u8]> {
        let r = &self.regions[self.region(address, size)?];
        let p = (address - r.address) as usize;
        Ok(&r.bytes[p..p + size])
    }
    pub fn load(&self, address: u64, size: u64) -> Result<u64> {
        if ![1, 2, 4, 8].contains(&size) {
            return Err("Invalid scalar width".into());
        }
        let mut bytes = [0; 8];
        bytes[..size as usize].copy_from_slice(self.read(address, size as usize)?);
        Ok(u64::from_le_bytes(bytes))
    }
    pub fn store(&mut self, address: u64, value: u64, size: u64) -> Result<()> {
        if ![1, 2, 4, 8].contains(&size) {
            return Err("Invalid scalar width".into());
        }
        self.write(address, &value.to_le_bytes()[..size as usize])
    }
    /// Scoped LOCK XADD32: host atomic RMW, with no outstanding byte references.
    /// Current Memory ownership is exclusive/cooperative; this does not permit
    /// unsynchronized host threads to access the same non-atomic byte mappings.
    #[allow(unsafe_code)]
    pub fn atomic_xadd_u32(&mut self, address: u64, addend: u32) -> Result<u32> {
        use std::sync::atomic::AtomicU32;
        if address % 4 != 0 {
            return Err("Unaligned LOCK XADD32 unsupported".into());
        }
        self.validate_write(address, 4)?;
        let index = self.region(address, 4)?;
        let region = &mut self.regions[index];
        let offset = (address - region.address) as usize;
        let pointer = region.bytes[offset..offset + 4].as_mut_ptr();
        if (pointer as usize) % std::mem::align_of::<AtomicU32>() != 0 {
            return Err("Unaligned host backing for LOCK XADD32 unsupported".into());
        }
        // SAFETY: one live writable 4-byte allocation range, aligned, exclusively
        // borrowed through self; no references derived from the slice survive.
        // Atomic reference ends before subsequent non-atomic mapping operations.
        Ok(crate::atomic_xadd::exchange_add(
            unsafe { AtomicU32::from_ptr(pointer.cast()) },
            addend,
        ))
    }
    pub fn write(&mut self, address: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.validate_write(address, bytes.len())?;
        let index = self.region(address, bytes.len())?;
        let r = &mut self.regions[index];
        let p = (address - r.address) as usize;
        r.bytes[p..p + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }
    /// Exclusive, validated fixed-size guest storage for an infallible commit.
    /// The borrow prevents mapping/permission changes until the caller releases
    /// it. All ordinary write barriers and runtime-owner checks still apply.
    pub(crate) fn borrow_write<const N: usize>(&mut self, address: u64) -> Result<&mut [u8; N]> {
        if N == 0 {
            return Err("Empty guest write borrow".into());
        }
        self.validate_write(address, N)?;
        let index = self.region(address, N)?;
        let region = &mut self.regions[index];
        let offset = (address - region.address) as usize;
        (&mut region.bytes[offset..offset + N])
            .try_into()
            .map_err(|_| "Guest write borrow size mismatch".into())
    }
    /// Bounded libc memcpy. Validate both full ranges before committing output;
    /// overlapping ranges are explicitly unsupported (use memmove semantics separately).
    pub fn memcpy(&mut self, destination: u64, source: u64, count: u64) -> Result<u64> {
        if count > 16 * 1024 * 1024 {
            return Err("Guest memcpy exceeds 16 MiB limit".into());
        }
        if count == 0 {
            return Ok(destination);
        }
        let source_end = source
            .checked_add(count)
            .ok_or("Guest memcpy source overflow")?;
        let destination_end = destination
            .checked_add(count)
            .ok_or("Guest memcpy destination overflow")?;
        if source == 0 || destination == 0 || (source < destination_end && destination < source_end)
        {
            return Err("Invalid or overlapping guest memcpy ranges".into());
        }
        self.validate_write(destination, count as usize)?;
        let input = self.read(source, count as usize)?;
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(input.len())
            .map_err(|_| "Cannot allocate guest memcpy snapshot")?;
        snapshot.extend_from_slice(input);
        self.write(destination, &snapshot)?;
        Ok(destination)
    }
    /// Validate an entire output buffer without changing guest storage.
    pub fn validate_write(&self, address: u64, size: usize) -> Result<()> {
        self.validate_write_for(address, size, None)
    }
    fn validate_write_for(
        &self,
        address: u64,
        size: usize,
        owner: Option<&RuntimeOwner>,
    ) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let end = address
            .checked_add(size as u64)
            .ok_or("Guest write range overflow")?;
        if self
            .write_barriers
            .iter()
            .any(|&(base, length)| address < base.saturating_add(length as u64) && base < end)
        {
            return Err("Guest write overlaps an active comparator return frame".into());
        }
        let r = &self.regions[self.region(address, size)?];
        match (&r.runtime_owner, owner) {
            (None, None) => (),
            (Some(stored), Some(supplied)) if stored.matches(supplied) => (),
            _ => return Err("Guest write to runtime storage or runtime owner mismatch".into()),
        }
        if !r.writable || r.executable {
            return Err("Guest write permission failure".into());
        }
        Ok(())
    }
    pub fn executable(&self, address: u64) -> bool {
        self.region(address, 1)
            .is_ok_and(|i| self.regions[i].executable)
    }
}
fn vector_operand(address: u64, reg: u64, size: u64, aligned: u64) -> Result<()> {
    if reg >= 16
        || ![16, 32].contains(&size)
        || aligned > 1
        || (aligned == 1 && address % size != 0)
    {
        return Err("Invalid vector operand".into());
    }
    Ok(())
}
impl crate::strings::Memory for Memory<'_> {
    fn read(&self, address: u64) -> std::result::Result<u8, crate::strings::Fault> {
        self.load(address, 1)
            .map(|v| v as u8)
            .map_err(|_| crate::strings::Fault::Read)
    }
    fn read_into(
        &self,
        address: u64,
        out: &mut [u8],
    ) -> std::result::Result<(), crate::strings::Fault> {
        if out.is_empty() {
            return Ok(());
        }
        let bytes =
            Memory::read(self, address, out.len()).map_err(|_| crate::strings::Fault::Read)?;
        out.copy_from_slice(bytes);
        Ok(())
    }
    fn write(
        &mut self,
        address: u64,
        bytes: &[u8],
    ) -> std::result::Result<(), crate::strings::Fault> {
        Memory::write(self, address, bytes).map_err(|_| crate::strings::Fault::Write)
    }
}
pub fn vector_load(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    reg: u64,
    size: u64,
    aligned: u64,
) -> Result<()> {
    vector_operand(address, reg, size, aligned)?;
    let mut value = [0; 32];
    value[..size as usize].copy_from_slice(memory.read(address, size as usize)?);
    vectors[reg as usize] = value;
    Ok(())
}
pub fn vector_store(
    memory: &mut Memory<'_>,
    vectors: &[[u8; 32]; 16],
    address: u64,
    reg: u64,
    size: u64,
    aligned: u64,
) -> Result<()> {
    vector_operand(address, reg, size, aligned)?;
    memory.write(address, &vectors[reg as usize][..size as usize])
}
/// Guard the entire unaligned 256-bit source before changing any destination lane.
pub fn vector_cmpgtd256(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    destination: u64,
    first: u64,
) -> Result<()> {
    vector_operand(address, destination, 32, 0)?;
    if first >= 16 {
        return Err("Invalid source vector register".into());
    }
    let source = memory.read(address, 32)?;
    let mut result = [0; 32];
    for lane in 0..8 {
        let start = lane * 4;
        let lhs = i32::from_le_bytes(
            vectors[first as usize][start..start + 4]
                .try_into()
                .unwrap(),
        );
        let rhs = i32::from_le_bytes(source[start..start + 4].try_into().unwrap());
        result[start..start + 4].fill(if lhs > rhs { 0xff } else { 0 });
    }
    vectors[destination as usize] = result;
    Ok(())
}
/// Compare packed integer lanes after validating the complete memory operand.
/// A 128-bit result clears the destination's modeled upper 128 bits.
pub fn vector_cmpeq(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    destination: u64,
    first: u64,
    size: u64,
    lane_size: u64,
) -> Result<()> {
    vector_operand(address, destination, size, 0)?;
    if first >= 16 || ![1, 2, 4, 8].contains(&lane_size) {
        return Err("Invalid packed equality operand".into());
    }
    let source = memory.read(address, size as usize)?;
    let mut result = [0; 32];
    for start in (0..size as usize).step_by(lane_size as usize) {
        let end = start + lane_size as usize;
        result[start..end].fill(
            if vectors[first as usize][start..end] == source[start..end] {
                0xff
            } else {
                0
            },
        );
    }
    vectors[destination as usize] = result;
    Ok(())
}
/// Parameters for a guarded packed wrapping integer add/subtract operation.
#[derive(Clone, Copy)]
pub struct VectorAddSub {
    pub destination: u64,
    pub first: u64,
    pub size: u64,
    pub lane_size: u64,
    pub subtract: u64,
}
pub fn vector_addsub(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    operation: VectorAddSub,
) -> Result<()> {
    let VectorAddSub {
        destination,
        first,
        size,
        lane_size,
        subtract,
    } = operation;
    vector_operand(address, destination, size, 0)?;
    if first >= 16 || ![1, 2, 4, 8].contains(&lane_size) || subtract > 1 {
        return Err("Invalid packed add/subtract operand".into());
    }
    let source = memory.read(address, size as usize)?;
    let mut result = [0; 32];
    for start in (0..size as usize).step_by(lane_size as usize) {
        let end = start + lane_size as usize;
        let mut lhs = [0; 8];
        let mut rhs = [0; 8];
        lhs[..lane_size as usize].copy_from_slice(&vectors[first as usize][start..end]);
        rhs[..lane_size as usize].copy_from_slice(&source[start..end]);
        let lhs = u64::from_le_bytes(lhs);
        let rhs = u64::from_le_bytes(rhs);
        let value = if subtract == 1 {
            lhs.wrapping_sub(rhs)
        } else {
            lhs.wrapping_add(rhs)
        };
        result[start..end].copy_from_slice(&value.to_le_bytes()[..lane_size as usize]);
    }
    vectors[destination as usize] = result;
    Ok(())
}
/// Compute VPTEST flags after guarding its complete memory operand.
pub fn vector_test(
    memory: &Memory<'_>,
    vectors: &[[u8; 32]; 16],
    address: u64,
    first: u64,
    size: u64,
    old_flags: u64,
) -> Result<u64> {
    vector_operand(address, first, size, 0)?;
    let source = memory.read(address, size as usize)?;
    let mut intersection = 0u8;
    let mut complement_intersection = 0u8;
    for (lhs, rhs) in vectors[first as usize].iter().zip(source) {
        intersection |= lhs & rhs;
        complement_intersection |= !lhs & rhs;
    }
    // CF/PF/AF/ZF/SF/OF are the only flags written by VPTEST.
    Ok((old_flags & !0x8d5)
        | if intersection == 0 { 0x40 } else { 0 }
        | u64::from(complement_intersection == 0))
}
/// Parameters for packed integer widening from a shorter memory operand.
#[derive(Clone, Copy)]
pub struct VectorExtend {
    pub destination: u64,
    pub size: u64,
    pub input_lane: u64,
    pub output_lane: u64,
    pub signed: u64,
}
pub fn vector_extend(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    operation: VectorExtend,
) -> Result<()> {
    let VectorExtend {
        destination,
        size,
        input_lane,
        output_lane,
        signed,
    } = operation;
    vector_operand(address, destination, size, 0)?;
    if ![(1, 2), (1, 4), (1, 8), (2, 4), (2, 8), (4, 8)].contains(&(input_lane, output_lane))
        || signed > 1
    {
        return Err("Invalid packed integer extension operand".into());
    }
    let input_lane = input_lane as usize;
    let output_lane = output_lane as usize;
    let lanes = size as usize / output_lane;
    let source = memory.read(address, lanes * input_lane)?;
    let mut result = [0; 32];
    for lane in 0..lanes {
        let input = &source[lane * input_lane..(lane + 1) * input_lane];
        let output = &mut result[lane * output_lane..(lane + 1) * output_lane];
        if signed == 1 && input[input_lane - 1] & 0x80 != 0 {
            output.fill(0xff);
        }
        output[..input_lane].copy_from_slice(input);
    }
    vectors[destination as usize] = result;
    Ok(())
}
pub fn vector_xor(
    memory: &Memory<'_>,
    vectors: &mut [[u8; 32]; 16],
    address: u64,
    destination: u64,
    first: u64,
    size: u64,
) -> Result<()> {
    vector_operand(address, destination, size, 0)?;
    if first >= 16 {
        return Err("Invalid source vector register".into());
    }
    let mut value = [0; 32];
    for (i, &byte) in memory.read(address, size as usize)?.iter().enumerate() {
        value[i] = byte ^ vectors[first as usize][i];
    }
    vectors[destination as usize] = value;
    Ok(())
}
#[allow(unsafe_code)]
pub mod ffi {
    use super::*;
    use crate::legacy_cpu::Cpu;
    /// # Safety
    /// CPU and its Memory context are live, exclusive, disjoint and valid for the call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_load_v1(
        cpu: *mut Cpu,
        address: u64,
        reg: u64,
        size: u64,
        aligned: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_load(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                reg,
                size,
                aligned,
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same context contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_store_v1(
        cpu: *mut Cpu,
        address: u64,
        reg: u64,
        size: u64,
        aligned: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_store(
                unsafe { &mut *(cpu.memory as *mut Memory<'_>) },
                &cpu.vectors,
                address,
                reg,
                size,
                aligned,
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 2;
    }
    /// # Safety
    /// Same context contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_xor_v1(
        cpu: *mut Cpu,
        address: u64,
        destination: u64,
        first: u64,
        size: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_xor(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                destination,
                first,
                size,
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same live, exclusive CPU/Memory contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_extend_v1(
        cpu: *mut Cpu,
        address: u64,
        destination: u64,
        size: u64,
        input_lane: u64,
        output_lane: u64,
        signed: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_extend(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                VectorExtend {
                    destination,
                    size,
                    input_lane,
                    output_lane,
                    signed,
                },
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same live, exclusive CPU/Memory contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_test_v1(
        cpu: *mut Cpu,
        address: u64,
        first: u64,
        size: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0 {
            if let Ok(flags) = vector_test(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &cpu.vectors,
                address,
                first,
                size,
                cpu.flags,
            ) {
                cpu.flags = flags;
                return;
            }
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same live, exclusive CPU/Memory contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_addsub_v1(
        cpu: *mut Cpu,
        address: u64,
        destination: u64,
        first: u64,
        size: u64,
        lane_size: u64,
        subtract: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_addsub(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                VectorAddSub {
                    destination,
                    first,
                    size,
                    lane_size,
                    subtract,
                },
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same live, exclusive CPU/Memory contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_cmpeq_v1(
        cpu: *mut Cpu,
        address: u64,
        destination: u64,
        first: u64,
        size: u64,
        lane_size: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_cmpeq(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                destination,
                first,
                size,
                lane_size,
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// Same live, exclusive CPU/Memory contract as native_ps5_module_vector_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_vector_cmpgtd256_v1(
        cpu: *mut Cpu,
        address: u64,
        destination: u64,
        first: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && vector_cmpgtd256(
                unsafe { &*(cpu.memory as *const Memory<'_>) },
                &mut cpu.vectors,
                address,
                destination,
                first,
            )
            .is_ok()
        {
            return;
        }
        cpu.fault = 1;
    }
    /// # Safety
    /// CPU is exclusive and live, and its memory field points to an exclusive
    /// live Memory for this synchronous call, installed by the Rust executor.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_load_v1(
        cpu: *mut Cpu,
        address: u64,
        size: u64,
    ) -> u64 {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return 0;
        };
        if cpu.fault != 0 {
            return 0;
        }
        if cpu.memory != 0 {
            if let Ok(value) = unsafe { &*(cpu.memory as *const Memory<'_>) }.load(address, size) {
                return value;
            }
        }
        cpu.fault = 1;
        0
    }
    /// # Safety
    /// Same ownership/lifetime contract as native_ps5_module_load_v1.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_store_v1(
        cpu: *mut Cpu,
        address: u64,
        value: u64,
        size: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory != 0
            && unsafe { &mut *(cpu.memory as *mut Memory<'_>) }
                .store(address, value, size)
                .is_ok()
        {
            return;
        }
        cpu.fault = 2;
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn packed_extension_all_pairs_and_sign_edges() {
        for size in [16usize, 32] {
            for (input_lane, output_lane) in
                [(1usize, 2usize), (1, 4), (1, 8), (2, 4), (2, 8), (4, 8)]
            {
                let modulus = 1u64 << (input_lane * 8);
                for signed in [0, 1] {
                    for phase in 0..4 {
                        let mut data = vec![0; size / output_lane * input_lane];
                        let mut expected = [0; 32];
                        for lane in 0..size / output_lane {
                            let value =
                                [0, modulus / 2 - 1, modulus / 2, modulus - 1][(lane + phase) % 4];
                            data[lane * input_lane..(lane + 1) * input_lane]
                                .copy_from_slice(&value.to_le_bytes()[..input_lane]);
                            let extended = if signed == 1 && value >= modulus / 2 {
                                (value as i64 - modulus as i64) as u64
                            } else {
                                value
                            };
                            expected[lane * output_lane..(lane + 1) * output_lane]
                                .copy_from_slice(&extended.to_le_bytes()[..output_lane]);
                        }
                        let original_data = data.clone();
                        let mut memory = Memory::default();
                        memory.add(0x1_0000_0001, &mut data, false, false).unwrap();
                        let mut vectors = [[0xa5; 32]; 16];
                        let mut all_expected = vectors;
                        all_expected[15] = expected;
                        vector_extend(
                            &memory,
                            &mut vectors,
                            0x1_0000_0001,
                            VectorExtend {
                                destination: 15,
                                size: size as u64,
                                input_lane: input_lane as u64,
                                output_lane: output_lane as u64,
                                signed,
                            },
                        )
                        .unwrap();
                        assert_eq!(vectors, all_expected);
                        drop(memory);
                        assert_eq!(data, original_data);
                    }
                }
            }
        }
    }
    #[test]
    fn packed_extension_exact_span_and_parameter_guards() {
        let original = [[0xa5; 32]; 16];
        for size in [16u64, 32] {
            for (input_lane, output_lane) in [(1, 2), (1, 4), (1, 8), (2, 4), (2, 8), (4, 8)] {
                for signed in [0, 1] {
                    for length in 0..(size / output_lane * input_lane) as usize {
                        let mut data = vec![0xff; length];
                        let mut memory = Memory::default();
                        if length != 0 {
                            memory.add(0x1001, &mut data, false, false).unwrap();
                        }
                        let mut vectors = original;
                        assert!(
                            vector_extend(
                                &memory,
                                &mut vectors,
                                0x1001,
                                VectorExtend {
                                    destination: 0,
                                    size,
                                    input_lane,
                                    output_lane,
                                    signed,
                                }
                            )
                            .is_err()
                        );
                        assert_eq!(vectors, original);
                    }
                }
            }
        }
        let mut data = [0; 32];
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        for (destination, size, input_lane, output_lane, signed) in [
            (16, 16, 1, 2, 0),
            (u64::MAX, 16, 1, 2, 0),
            (0, 0, 1, 2, 0),
            (0, 8, 1, 2, 0),
            (0, 64, 1, 2, 0),
            (0, u64::MAX, 1, 2, 0),
            (0, 16, 0, 2, 0),
            (0, 16, 1, 0, 0),
            (0, 16, 1, 1, 0),
            (0, 16, 8, 4, 0),
            (0, 16, 3, 8, 0),
            (0, 16, 1, 16, 0),
            (0, 16, u64::MAX, 2, 0),
            (0, 16, 1, u64::MAX, 0),
            (0, 16, 1, 2, 2),
            (0, 16, 1, 2, u64::MAX),
        ] {
            let mut vectors = original;
            assert!(
                vector_extend(
                    &memory,
                    &mut vectors,
                    0x1001,
                    VectorExtend {
                        destination,
                        size,
                        input_lane,
                        output_lane,
                        signed,
                    }
                )
                .is_err()
            );
            assert_eq!(vectors, original);
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn packed_extension_ffi_faults_and_success() {
        use crate::legacy_cpu::Cpu;
        let mut cpu = Cpu {
            vectors: [[0xa5; 32]; 16],
            ..Cpu::default()
        };
        for fault in [0, 2, 13] {
            cpu.fault = fault;
            unsafe { ffi::native_ps5_module_vector_extend_v1(&mut cpu, 0x1001, 0, 16, 1, 8, 1) };
            assert_eq!(cpu.fault, if fault == 0 { 1 } else { fault });
            assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
        }
        let mut data = [0xff, 0x80];
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        cpu.memory = (&mut memory as *mut Memory<'_>) as u64;
        cpu.fault = 0;
        unsafe { ffi::native_ps5_module_vector_extend_v1(&mut cpu, 0x1001, 0, 16, 1, 8, 1) };
        assert_eq!(cpu.fault, 0);
        assert_eq!(&cpu.vectors[0][..8], &(-1i64).to_le_bytes());
        assert_eq!(&cpu.vectors[0][8..16], &(-128i64).to_le_bytes());
        assert_eq!(&cpu.vectors[0][16..], &[0; 16]);
        let before = cpu.vectors;
        for signed in [0, 2] {
            cpu.fault = 0;
            // signed0 is valid but the four-byte source span is incomplete.
            unsafe {
                ffi::native_ps5_module_vector_extend_v1(&mut cpu, 0x1001, 0, 32, 1, 8, signed)
            };
            assert_eq!(cpu.fault, 1);
            assert_eq!(cpu.vectors, before);
        }
    }
    #[test]
    fn packed_test_flags_all_outcomes_and_operand_direction() {
        for size in [16u64, 32] {
            for (lhs, rhs, expected) in [
                (0u8, 0u8, 0x41),
                (1, 1, 1),
                (1, 2, 0x40),
                (1, 3, 0),
                (3, 1, 1),
            ] {
                for at in [0, size as usize - 1] {
                    let mut vectors = [[0; 32]; 16];
                    let mut data = [0; 32];
                    vectors[15][at] = lhs;
                    data[at] = rhs;
                    let before = vectors;
                    let mut memory = Memory::default();
                    memory
                        .add(0x1_0000_0001, &mut data[..size as usize], false, false)
                        .unwrap();
                    for old_flags in [0, u64::MAX, 0x246] {
                        let result =
                            vector_test(&memory, &vectors, 0x1_0000_0001, 15, size, old_flags)
                                .unwrap();
                        assert_eq!(result, (old_flags & !0x8d5) | expected);
                        assert_eq!(vectors, before);
                    }
                }
            }
        }
    }
    #[test]
    fn packed_test_width_and_full_span_guards() {
        let mut vectors = [[0; 32]; 16];
        vectors[0][31] = 1;
        let before = vectors;
        let mut data = [0; 32];
        data[31] = 3;
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        assert_eq!(
            vector_test(&memory, &vectors, 0x1001, 0, 16, 0).unwrap(),
            0x41
        );
        assert_eq!(vector_test(&memory, &vectors, 0x1001, 0, 32, 0).unwrap(), 0);
        for (first, size) in [
            (16, 16),
            (u64::MAX, 16),
            (0, 0),
            (0, 8),
            (0, 64),
            (0, u64::MAX),
        ] {
            assert!(vector_test(&memory, &vectors, 0x1001, first, size, u64::MAX).is_err());
        }
        assert_eq!(vectors, before);
        for size in [16u64, 32] {
            for length in 0..size as usize {
                let mut data = vec![0; length];
                let mut memory = Memory::default();
                if length != 0 {
                    memory.add(0x1001, &mut data, false, false).unwrap();
                }
                assert!(vector_test(&memory, &vectors, 0x1001, 0, size, u64::MAX).is_err());
                assert_eq!(vectors, before);
            }
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn packed_test_ffi_commits_flags_only_after_success() {
        use crate::legacy_cpu::Cpu;
        let mut cpu = Cpu {
            flags: u64::MAX,
            vectors: [[0xa5; 32]; 16],
            ..Cpu::default()
        };
        let vectors = cpu.vectors;
        for fault in [0, 2, 13] {
            cpu.fault = fault;
            unsafe { ffi::native_ps5_module_vector_test_v1(&mut cpu, 0x1001, 0, 32) };
            assert_eq!(cpu.fault, if fault == 0 { 1 } else { fault });
            assert_eq!(cpu.flags, u64::MAX);
            assert_eq!(cpu.vectors, vectors);
        }
        let mut data = [0; 31];
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        cpu.memory = (&mut memory as *mut Memory<'_>) as u64;
        for (first, size) in [(0, 32), (16, 16), (0, 0)] {
            cpu.fault = 0;
            unsafe { ffi::native_ps5_module_vector_test_v1(&mut cpu, 0x1001, first, size) };
            assert_eq!(cpu.fault, 1);
            assert_eq!(cpu.flags, u64::MAX);
            assert_eq!(cpu.vectors, vectors);
        }
        cpu.fault = 0;
        unsafe { ffi::native_ps5_module_vector_test_v1(&mut cpu, 0x1001, 0, 16) };
        assert_eq!(cpu.fault, 0);
        assert_eq!(cpu.flags, !0x8d5u64 | 0x41);
        assert_eq!(cpu.vectors, vectors);
    }
    #[test]
    fn packed_addsub_wraps_each_lane_and_preserves_aliases() {
        for size in [16usize, 32] {
            for lane in [1usize, 2, 4, 8] {
                let modulus = 1u128 << (lane * 8);
                for subtract in [0, 1] {
                    for destination in [3usize, 15] {
                        for phase in 0..4 {
                            let mut vectors = [[0xa5; 32]; 16];
                            let mut data = [0; 32];
                            let mut expected = [0; 32];
                            for (index, start) in (0..size).step_by(lane).enumerate() {
                                let (lhs, rhs) = match (index + phase) % 4 {
                                    0 => (modulus - 1, 1),
                                    1 => (0, 1),
                                    2 => (modulus / 2, modulus / 2),
                                    _ => (7, 3),
                                };
                                vectors[15][start..start + lane]
                                    .copy_from_slice(&lhs.to_le_bytes()[..lane]);
                                data[start..start + lane]
                                    .copy_from_slice(&rhs.to_le_bytes()[..lane]);
                                let value = if subtract == 0 {
                                    (lhs + rhs) % modulus
                                } else {
                                    (lhs + modulus - rhs) % modulus
                                };
                                expected[start..start + lane]
                                    .copy_from_slice(&value.to_le_bytes()[..lane]);
                            }
                            let original_data = data;
                            let mut memory = Memory::default();
                            memory
                                .add(0x1_0000_0001, &mut data[..size], false, false)
                                .unwrap();
                            let mut all_expected = vectors;
                            all_expected[destination] = expected;
                            vector_addsub(
                                &memory,
                                &mut vectors,
                                0x1_0000_0001,
                                VectorAddSub {
                                    destination: destination as u64,
                                    first: 15,
                                    size: size as u64,
                                    lane_size: lane as u64,
                                    subtract,
                                },
                            )
                            .unwrap();
                            assert_eq!(vectors, all_expected);
                            drop(memory);
                            assert_eq!(data, original_data);
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn packed_addsub_invalid_operands_and_short_spans_are_atomic() {
        let original = [[0xa5; 32]; 16];
        for size in [16u64, 32] {
            for lane_size in [1, 2, 4, 8] {
                for subtract in [0, 1] {
                    for length in 0..size as usize {
                        let mut data = vec![1; length];
                        let mut memory = Memory::default();
                        if length != 0 {
                            memory.add(0x1001, &mut data, false, false).unwrap();
                        }
                        let mut vectors = original;
                        assert!(
                            vector_addsub(
                                &memory,
                                &mut vectors,
                                0x1001,
                                VectorAddSub {
                                    destination: 0,
                                    first: 0,
                                    size,
                                    lane_size,
                                    subtract,
                                }
                            )
                            .is_err()
                        );
                        assert_eq!(vectors, original);
                    }
                }
            }
        }
        let mut data = [1; 32];
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        for (destination, first, size, lane_size, subtract) in [
            (16, 0, 16, 1, 0),
            (0, 16, 16, 1, 0),
            (u64::MAX, 0, 16, 1, 0),
            (0, u64::MAX, 16, 1, 0),
            (0, 0, 0, 1, 0),
            (0, 0, 8, 1, 0),
            (0, 0, 64, 1, 0),
            (0, 0, u64::MAX, 1, 0),
            (0, 0, 16, 0, 0),
            (0, 0, 16, 3, 0),
            (0, 0, 16, 16, 0),
            (0, 0, 16, u64::MAX, 0),
            (0, 0, 16, 1, 2),
            (0, 0, 16, 1, u64::MAX),
        ] {
            let mut vectors = original;
            assert!(
                vector_addsub(
                    &memory,
                    &mut vectors,
                    0x1001,
                    VectorAddSub {
                        destination,
                        first,
                        size,
                        lane_size,
                        subtract,
                    }
                )
                .is_err()
            );
            assert_eq!(vectors, original);
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn packed_addsub_ffi_faults_and_success() {
        use crate::legacy_cpu::Cpu;
        let mut cpu = Cpu {
            vectors: [[0; 32]; 16],
            ..Cpu::default()
        };
        for fault in [0, 2, 13] {
            cpu.fault = fault;
            unsafe { ffi::native_ps5_module_vector_addsub_v1(&mut cpu, 0x1000, 0, 0, 16, 1, 1) };
            assert_eq!(cpu.fault, if fault == 0 { 1 } else { fault });
            assert_eq!(cpu.vectors, [[0; 32]; 16]);
        }
        let mut data = [1; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, false, false).unwrap();
        cpu.memory = (&mut memory as *mut Memory<'_>) as u64;
        cpu.fault = 0;
        unsafe { ffi::native_ps5_module_vector_addsub_v1(&mut cpu, 0x1000, 0, 0, 16, 1, 1) };
        assert_eq!(cpu.fault, 0);
        assert_eq!(&cpu.vectors[0][..16], &[255; 16]);
        assert_eq!(&cpu.vectors[0][16..], &[0; 16]);
        let before = cpu.vectors;
        unsafe { ffi::native_ps5_module_vector_addsub_v1(&mut cpu, 0x1000, 0, 0, 16, 1, 2) };
        assert_eq!(cpu.fault, 1);
        assert_eq!(cpu.vectors, before);
    }
    #[test]
    fn packed_equality_all_widths_and_aliases() {
        for size in [16usize, 32] {
            for lane in [1usize, 2, 4, 8] {
                for destination in [3usize, 15] {
                    let first = 15;
                    let mut vectors = [[0xa5; 32]; 16];
                    vectors[first] = std::array::from_fn(|i| (i * 17 + 129) as u8);
                    let mut data = vectors[first];
                    let mut expected = [0; 32];
                    for start in (0..size).step_by(lane) {
                        if start / lane % 2 == 0 {
                            expected[start..start + lane].fill(0xff);
                        } else {
                            // Last-byte mismatches catch partial-lane comparisons.
                            data[start + lane - 1] ^= 1;
                        }
                    }
                    let mut memory = Memory::default();
                    memory
                        .add(0x1_0000_0001, &mut data[..size], false, false)
                        .unwrap();
                    let mut all_expected = vectors;
                    all_expected[destination] = expected;
                    vector_cmpeq(
                        &memory,
                        &mut vectors,
                        0x1_0000_0001,
                        destination as u64,
                        first as u64,
                        size as u64,
                        lane as u64,
                    )
                    .unwrap();
                    assert_eq!(vectors, all_expected);
                }
            }
        }
    }
    #[test]
    fn packed_equality_rejects_parameters_and_incomplete_spans_atomically() {
        let original = [[0xa5; 32]; 16];
        for size in [16u64, 32] {
            for lane in [1, 2, 4, 8] {
                for length in 0..size as usize {
                    let mut data = vec![0xa5; length];
                    let mut memory = Memory::default();
                    if length != 0 {
                        memory.add(0x1001, &mut data, false, false).unwrap();
                    }
                    let mut vectors = original;
                    assert!(vector_cmpeq(&memory, &mut vectors, 0x1001, 0, 0, size, lane).is_err());
                    assert_eq!(vectors, original);
                }
            }
        }
        let mut data = [0xa5; 32];
        let mut memory = Memory::default();
        memory.add(0x1001, &mut data, false, false).unwrap();
        for (dst, first, size, lane) in [
            (16, 0, 16, 1),
            (0, 16, 16, 1),
            (u64::MAX, 0, 16, 1),
            (0, u64::MAX, 16, 1),
            (0, 0, 0, 1),
            (0, 0, 8, 1),
            (0, 0, 64, 1),
            (0, 0, u64::MAX, 1),
            (0, 0, 16, 0),
            (0, 0, 16, 3),
            (0, 0, 16, 16),
            (0, 0, 16, u64::MAX),
        ] {
            let mut vectors = original;
            assert!(vector_cmpeq(&memory, &mut vectors, 0x1001, dst, first, size, lane).is_err());
            assert_eq!(vectors, original);
        }
    }
    #[test]
    #[allow(unsafe_code)]
    fn packed_equality_ffi_preserves_prior_fault_and_guards_missing_memory() {
        use crate::legacy_cpu::Cpu;
        let mut cpu = Cpu {
            vectors: [[0xa5; 32]; 16],
            ..Cpu::default()
        };
        for fault in [0, 2, 13] {
            cpu.fault = fault;
            unsafe { ffi::native_ps5_module_vector_cmpeq_v1(&mut cpu, 0x1000, 0, 0, 32, 4) };
            assert_eq!(cpu.fault, if fault == 0 { 1 } else { fault });
            assert_eq!(cpu.vectors, [[0xa5; 32]; 16]);
        }
        let mut data = [0xa5; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, false, false).unwrap();
        cpu.memory = (&mut memory as *mut Memory<'_>) as u64;
        cpu.fault = 0;
        unsafe { ffi::native_ps5_module_vector_cmpeq_v1(&mut cpu, 0x1000, 0, 0, 16, 8) };
        assert_eq!(cpu.fault, 0);
        assert_eq!(&cpu.vectors[0][..16], &[0xff; 16]);
        assert_eq!(&cpu.vectors[0][16..], &[0; 16]);
        let before = cpu.vectors;
        unsafe { ffi::native_ps5_module_vector_cmpeq_v1(&mut cpu, 0x1000, 0, 0, 16, 3) };
        assert_eq!(cpu.fault, 1);
        assert_eq!(cpu.vectors, before);
    }
    #[test]
    fn packed_compare_full_span_and_bad_registers_preserve_destination() {
        for length in 1..32 {
            let mut data = vec![0; length];
            let mut memory = Memory::default();
            memory.add(0x1000, &mut data, false, false).unwrap();
            let mut vectors = [[0xa5; 32]; 16];
            let before = vectors;
            assert!(vector_cmpgtd256(&memory, &mut vectors, 0x1000, 0, 0).is_err());
            assert_eq!(vectors, before);
        }
        let mut data = [0; 32];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, false, false).unwrap();
        let mut vectors = [[0xa5; 32]; 16];
        let before = vectors;
        for (dst, src) in [(16, 0), (0, 16), (u64::MAX, 0)] {
            assert!(vector_cmpgtd256(&memory, &mut vectors, 0x1000, dst, src).is_err());
            assert_eq!(vectors, before);
        }
    }
    #[test]
    fn runtime_pool_requires_identity_and_preserves_frame_barriers() {
        use super::{Memory, RuntimeOwner};
        let owner = RuntimeOwner::new();
        let other = RuntimeOwner::new();
        let mut pool = [0xa5; 64];
        let mut source = [7; 16];
        let mut m = Memory::default();
        m.add_runtime_data(0x1000, &mut pool, &owner).unwrap();
        m.add(0x2000, &mut source, true, false).unwrap();
        assert!(m.write(0x1000, &[1]).is_err());
        assert!(m.store(0x1000, 1, 8).is_err());
        assert!(m.memcpy(0x1000, 0x2000, 16).is_err());
        assert!(m.atomic_xadd_u32(0x1000, 1).is_err());
        assert!(m.runtime_write(0x1000, &[1], &other).is_err());
        assert!(m.runtime_write(0x2000, &[1], &owner).is_err());
        assert_eq!(m.read(0x1000, 64).unwrap(), &[0xa5; 64]);
        m.runtime_store(0x1000, 0x1234, 2, &owner.clone()).unwrap();
        assert_eq!(m.load(0x1000, 2).unwrap(), 0x1234);
        m.set_write_barriers(vec![(0x1008, 8)]);
        assert!(m.runtime_write(0x1007, &[0; 10], &owner).is_err());
        assert_eq!(m.read(0x1007, 10).unwrap(), &[0xa5; 10]);
        m.set_write_barriers(Vec::new());
        assert!(m.write(0x1008, &[1]).is_err());
        assert!(m.runtime_free(0x1000, &owner).is_err());
        assert!(m.unmap_anonymous(0x1000, 64).is_err());
    }
    #[test]
    fn runtime_pool_can_be_remapped_without_losing_protection() {
        use super::{Memory, RuntimeOwner};
        let owner = RuntimeOwner::new();
        let mut pool = [0; 64];
        for value in [17, 29] {
            let mut m = Memory::default();
            m.add_runtime_data(0x4000, &mut pool, &owner).unwrap();
            assert!(m.write(0x4000, &[0]).is_err());
            assert!(m.mark_runtime_heap(0x4000, &owner).is_err());
            assert!(m.unmap_anonymous(0x4000, 16384).is_err());
            m.runtime_write(0x4000, &[value], &owner).unwrap();
        }
        assert_eq!(pool[0], 29);
    }
    #[test]
    fn runtime_heap_protection_survives_memory_reconstruction() {
        use super::{Memory, OwnedMappings, RuntimeOwner};
        let owner = RuntimeOwner::new();
        let foreign = RuntimeOwner::new();
        let mut mappings = OwnedMappings::default();
        let address;
        {
            let mut m = Memory::default();
            m.attach_owned(&mut mappings).unwrap();
            address = m.malloc(40).unwrap();
            assert!(m.mark_runtime_heap(address + 1, &owner).is_err());
            m.mark_runtime_heap(address, &owner).unwrap();
            assert!(m.mark_runtime_heap(address, &foreign).is_err());
            assert!(m.runtime_free(address, &foreign).is_err());
            assert!(m.free(address).is_err());
            assert!(m.heap_allocation_size(address).is_err());
            assert_eq!(m.runtime_heap_allocation_size(address, &owner).unwrap(), 48);
            assert!(m.write(address + 47, &[1]).is_err());
            let before = m.regions.len();
            assert!(crate::libc_memory::realloc(&mut m, address, 64).is_err());
            assert_eq!(m.regions.len(), before);
            m.runtime_write(address + 47, &[99], &owner).unwrap();
            assert!(m.unmap_anonymous(address, 48).is_err());
        }
        assert_eq!(mappings.bytes_allocated(), 48);
        {
            let mut m = Memory::default();
            m.attach_owned(&mut mappings).unwrap();
            assert_eq!(m.load(address + 47, 1).unwrap(), 99);
            assert!(m.free(address).is_err());
            assert!(m.write(address, &[1]).is_err());
            assert_eq!(m.runtime_heap_allocation_size(address, &owner).unwrap(), 48);
            m.runtime_free(address, &owner).unwrap();
            let ordinary = m.malloc(40).unwrap();
            assert_eq!(ordinary, address);
            m.write(ordinary, &[1]).unwrap();
            assert!(m.runtime_free(ordinary, &owner).is_err());
            m.free(ordinary).unwrap();
        }
        assert_eq!(mappings.bytes_allocated(), 0);
    }

    #[test]
    fn memcpy_validates_complete_ranges_before_copying() {
        let mut source = [1, 2, 3, 4, 5, 6, 7, 8];
        let mut output = [0xa5; 16];
        let mut memory = super::Memory::default();
        memory.add(0x1000, &mut source, false, false).unwrap();
        memory.add(0x2000, &mut output, true, false).unwrap();
        for (destination, source, count) in [
            (0x2000, 0x1004, 8),
            (0x200c, 0x1000, 8),
            (0x1000, 0x2000, 8),
            (0x2001, 0x2000, 8),
            (u64::MAX, 0x1000, 8),
            (0x2000, 0x1000, 16 * 1024 * 1024 + 1),
        ] {
            assert!(memory.memcpy(destination, source, count).is_err());
            assert_eq!(memory.read(0x2000, 16).unwrap(), &[0xa5; 16]);
        }
        assert_eq!(memory.memcpy(u64::MAX, 0, 0).unwrap(), u64::MAX);
        assert_eq!(memory.memcpy(0x2003, 0x1001, 6).unwrap(), 0x2003);
        assert_eq!(memory.read(0x2003, 6).unwrap(), &[2, 3, 4, 5, 6, 7]);
        assert_eq!(memory.read(0x2000, 3).unwrap(), &[0xa5; 3]);
        assert_eq!(memory.read(0x2009, 7).unwrap(), &[0xa5; 7]);
    }
    #[test]
    fn heap_ownership_persistence_and_allocation_failures() {
        let mut owner = super::OwnedMappings::default();
        let heap;
        let anonymous;
        {
            let mut m = super::Memory::default();
            m.attach_owned(&mut owner).unwrap();
            heap = m.calloc(3, 7).unwrap();
            assert_eq!(heap % 16, 0);
            assert_eq!(m.read(heap, 32).unwrap(), &[0; 32]);
            m.store(heap, 99, 8).unwrap();
            anonymous = m.map_anonymous([0, 16, 3, 0x1002, u64::MAX, 0]).unwrap();
            assert!(m.free(anonymous).is_err());
            assert!(m.free(heap + 1).is_err());
            assert!(m.unmap_anonymous(heap, 32).is_err());
            assert_eq!(m.calloc(u64::MAX, 2), Err(12));
            assert_eq!(m.malloc(u64::MAX), Err(12));
            assert_eq!(m.malloc(16 * 1024 * 1024 + 1), Err(12));
        }
        let mut m = super::Memory::default();
        m.attach_owned(&mut owner).unwrap();
        assert_eq!(m.load(heap, 8).unwrap(), 99);
        m.free(heap).unwrap();
        assert!(m.read(heap, 1).is_err());
        assert!(m.free(heap).is_err());
        m.free(0).unwrap();
        m.unmap_anonymous(anonymous, 16).unwrap();
    }
    #[test]
    fn service_storage_is_guest_readonly_and_updates_are_scoped() {
        let mut service = [0u8; 32];
        let mut ordinary = [0u8; 32];
        let mut memory = super::Memory::default();
        memory.add_service_data(0x1000, &mut service).unwrap();
        memory.add(0x2000, &mut ordinary, true, false).unwrap();
        memory.update_service_data(0x1000, b"message\0").unwrap();
        assert_eq!(memory.read(0x1000, 8).unwrap(), b"message\0");
        assert!(memory.write(0x1000, b"changed").is_err());
        assert!(memory.update_service_data(0x2000, b"changed").is_err());
        assert!(memory.update_service_data(0x101f, b"overflow").is_err());
    }
    #[test]
    fn anonymous_maps_survive_resume_and_revoke_access() {
        let mut owner = super::OwnedMappings::default();
        let address;
        {
            let mut memory = super::Memory::default();
            memory.attach_owned(&mut owner).unwrap();
            memory.reserve_service(0x400000000);
            address = memory
                .map_anonymous([0, 17, 3, 0x1002, u64::MAX, 0])
                .unwrap();
            assert_eq!(address, 0x400004000);
            assert_eq!(memory.read(address, 16384).unwrap(), &[0; 16384]);
            memory.store(address, 0x1234, 8).unwrap();
            assert!(!memory.executable(address));
            assert_eq!(memory.anonymous_release_size(address, 1), Ok(16384));
            assert_eq!(memory.anonymous_release_size(address, 16385), Err(22));
            assert_eq!(memory.anonymous_release_size(address, u64::MAX), Err(22));
            assert_eq!(memory.load(address, 8).unwrap(), 0x1234);
            assert_eq!(memory.unmap_anonymous(address, 16385), Err(22));
            let readonly = memory
                .map_anonymous([0, 1, 1, 0x1002, u64::MAX, 0])
                .unwrap();
            assert!(memory.store(readonly, 1, 1).is_err());
            assert_eq!(memory.unmap_anonymous(readonly, 1), Ok(0));
        }
        let mut memory = super::Memory::default();
        memory.attach_owned(&mut owner).unwrap();
        assert_eq!(memory.load(address, 8).unwrap(), 0x1234);
        assert_eq!(memory.unmap_anonymous(address, 17), Ok(0));
        assert!(memory.load(address, 1).is_err());
        assert_eq!(memory.unmap_anonymous(address, 17), Err(22));
        assert_eq!(
            memory.map_anonymous([0, 1, 7, 0x1002, u64::MAX, 0]),
            Err(22)
        );
        assert_eq!(
            memory.map_anonymous([0, u64::MAX, 3, 0x1002, u64::MAX, 0]),
            Err(22)
        );
        assert_eq!(
            memory.map_anonymous([0, 16 * 1024 * 1024 + 1, 3, 0x1002, u64::MAX, 0]),
            Err(12)
        );
    }
    use super::*;
    #[test]
    fn string_memory_bridge_validates_whole_ranges_before_changes() {
        let mut bytes = *b"hello\0xx";
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut out = [0xa5; 8];
        assert!(crate::strings::Memory::read_into(&memory, 0x1001, &mut out).is_err());
        assert_eq!(out, [0xa5; 8]);
        assert!(crate::strings::Memory::write(&mut memory, 0x1005, b"oops").is_err());
        assert_eq!(memory.read(0x1000, 8).unwrap(), b"hello\0xx");
        assert_eq!(
            crate::strings::call(&mut memory, 4, 0x1000, 0, 0).unwrap(),
            5
        );
    }
    #[test]
    fn vectors_preserve_fault_outputs_and_clear_vex_upper_half() {
        let mut bytes = std::array::from_fn::<_, 128, _>(|i| i as u8);
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        for size in [16, 32] {
            for reg in 0..16 {
                let mut vectors = [[0xa5; 32]; 16];
                vector_load(&memory, &mut vectors, 0x1001, reg, size, 0).unwrap();
                assert_eq!(
                    &vectors[reg as usize][..size as usize],
                    memory.read(0x1001, size as usize).unwrap()
                );
                assert!(
                    vectors[reg as usize][size as usize..]
                        .iter()
                        .all(|&b| b == 0)
                );
                let before = vectors;
                for (address, index, width, aligned) in [
                    (0x1001, reg, size, 1),
                    (0x1078, reg, size, 0),
                    (u64::MAX, reg, size, 0),
                    (0x1000, 16, size, 0),
                    (0x1000, reg, 15, 0),
                    (0x1000, reg, size, 2),
                ] {
                    assert!(
                        vector_load(&memory, &mut vectors, address, index, width, aligned).is_err()
                    );
                    assert_eq!(vectors, before);
                }
                vector_xor(&memory, &mut vectors, 0x1020, reg, reg, size).unwrap();
                for i in 0..size as usize {
                    assert_eq!(
                        vectors[reg as usize][i],
                        before[reg as usize][i] ^ (0x20 + i as u8)
                    );
                }
            }
        }
        let vectors = [[0x5a; 32]; 16];
        let before = memory.read(0x1000, 128).unwrap().to_vec();
        assert!(vector_store(&mut memory, &vectors, 0x1078, 0, 32, 0).is_err());
        assert_eq!(memory.read(0x1000, 128).unwrap(), before);
        vector_store(&mut memory, &vectors, 0x1040, 15, 32, 1).unwrap();
        assert_eq!(memory.read(0x1040, 32).unwrap(), [0x5a; 32]);
    }
    #[test]
    fn scalar_accesses_are_whole_range_and_permission_checked() {
        let mut data = [0u8; 16];
        let mut code = [0u8; 16];
        let mut adjacent = [0u8; 8];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut data, true, false).unwrap();
        memory.add(0x1010, &mut adjacent, true, false).unwrap();
        memory.add(0x2000, &mut code, true, true).unwrap();
        memory.store(0x1000, 0x8877665544332211, 8).unwrap();
        assert_eq!(memory.load(0x1002, 2).unwrap(), 0x4433);
        for (address, size) in [(0x100c, 8), (u64::MAX, 8), (0x1000, 3), (0x2000, 8)] {
            assert!(memory.store(address, 0, size).is_err());
        }
        assert_eq!(memory.load(0x1000, 8).unwrap(), 0x8877665544332211);
        assert!(memory.executable(0x2000));
        assert!(!memory.executable(0x1000));
    }
}

#[cfg(test)]
#[path = "guest_memory_callback_tests.rs"]
mod callback_tests;
