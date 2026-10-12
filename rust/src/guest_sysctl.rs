//! Bounded read-only virtual hardware sysctl tree.
//!
//! ABI: SDK10b539de include/freebsd/sys/sysctl.h. Semantics audited against
//! FreeBSD releng/11.4 lib/libc/gen/{sysctlbyname,sysctlnametomib}.c and
//! sys/kern/{kern_sysctl,kern_mib}.c. This is an original Rust implementation.
//! hw.physmem and hw.realmem deliberately report the same virtual profile
//! capacity: no firmware reservation difference is modeled, nor free host RAM.
use crate::{guest_memory::Memory, guest_system::GuestSystemProfile, Result};

#[derive(Clone, Copy)]
enum Leaf {
    Cpu,
    Page,
    Physical,
    Real,
    Available,
}
#[derive(Clone, Copy)]
enum Lookup {
    Leaf(Leaf),
    Error(u32),
    Unsupported,
}

pub struct Write {
    pub address: u64,
    pub bytes: Vec<u8>,
}
/// Snapshot inputs, then validate every write before committing in this order.
/// No guest pointers or borrowed memory survive a service call.
pub struct Plan {
    pub writes: Vec<Write>,
    pub errno: Option<u32>,
}
impl Plan {
    fn error(errno: u32) -> Self {
        Self {
            writes: Vec::new(),
            errno: Some(errno),
        }
    }
    pub fn return_value(&self) -> u64 {
        if self.errno.is_some() {
            u32::MAX as u64
        } else {
            0
        }
    }
    pub fn commit(&self, memory: &mut Memory<'_>) -> Result<()> {
        for write in &self.writes {
            memory.validate_write(write.address, write.bytes.len())?;
        }
        for write in &self.writes {
            memory.write(write.address, &write.bytes)?;
        }
        Ok(())
    }
}
fn hardware(name: i32) -> Lookup {
    match name {
        3 => Lookup::Leaf(Leaf::Cpu),
        5 => Lookup::Leaf(Leaf::Physical),
        7 => Lookup::Leaf(Leaf::Page),
        12 => Lookup::Leaf(Leaf::Real),
        1..=11 => Lookup::Unsupported,
        // Higher dynamic OID numbers cannot safely be invented or declared absent.
        256.. => Lookup::Unsupported,
        _ => Lookup::Error(2),
    }
}
fn numeric(mib: &[i32]) -> Lookup {
    if mib[0] != 6 {
        return if (0..=9).contains(&mib[0]) || mib[0] >= 256 {
            Lookup::Unsupported
        } else {
            Lookup::Error(2)
        };
    }
    match hardware(mib[1]) {
        Lookup::Leaf(_) if mib.len() != 2 => Lookup::Error(20),
        result => result,
    }
}
fn named(name: &[u8]) -> Lookup {
    let name = name.strip_suffix(b".").unwrap_or(name);
    match name {
        b"hw.ncpu" => Lookup::Leaf(Leaf::Cpu),
        b"hw.pagesize" => Lookup::Leaf(Leaf::Page),
        b"hw.physmem" => Lookup::Leaf(Leaf::Physical),
        b"hw.realmem" => Lookup::Leaf(Leaf::Real),
        b"hw.availpages" => Lookup::Leaf(Leaf::Available),
        b"hw" => Lookup::Error(22), // name-to-MIB yields one component; sysctl rejects it.
        b"hw.machine" | b"hw.model" | b"hw.byteorder" | b"hw.usermem" | b"hw.disknames"
        | b"hw.diskstats" | b"hw.floatingpt" | b"hw.machine_arch" | b"hw.pagesizes" => {
            Lookup::Unsupported
        }
        _ => {
            if name
                .split(|byte| *byte == b'.')
                .any(|component| component.is_empty())
            {
                return Lookup::Error(2);
            }
            for leaf in [
                b"hw.ncpu".as_slice(),
                b"hw.pagesize",
                b"hw.physmem",
                b"hw.realmem",
                b"hw.availpages",
            ] {
                if name
                    .strip_prefix(leaf)
                    .is_some_and(|suffix| suffix.starts_with(b"."))
                {
                    return Lookup::Error(2);
                }
            }
            let root = name.split(|byte| *byte == b'.').next().unwrap_or_default();
            if matches!(
                root,
                b"hw"
                    | b"kern"
                    | b"vm"
                    | b"vfs"
                    | b"net"
                    | b"debug"
                    | b"machdep"
                    | b"user"
                    | b"p1003_1b"
                    | b"security"
                    | b"dev"
                    | b"compat"
                    | b"sysctl"
            ) {
                Lookup::Unsupported
            } else {
                Lookup::Error(2)
            }
        }
    }
}
/// Numeric sysctl argument layout matches the six guest integer registers.
pub fn plan_numeric(
    memory: &Memory<'_>,
    profile: Option<&GuestSystemProfile>,
    args: [u64; 6],
) -> Result<Plan> {
    let [mib, length, old, oldlen, new, _newlen] = args;
    let length = length as u32;
    if !(2..=24).contains(&length) {
        return Ok(Plan::error(22));
    }
    let mut names = Vec::with_capacity(length as usize);
    for chunk in memory.read(mib, length as usize * 4)?.chunks_exact(4) {
        names.push(i32::from_le_bytes(chunk.try_into().expect("four bytes")));
    }
    // Numeric kernel lookup occurs after the oldlen copyin.
    output(memory, profile, numeric(&names), old, oldlen, new)
}
pub fn plan_named(
    memory: &Memory<'_>,
    profile: Option<&GuestSystemProfile>,
    args: [u64; 5],
) -> Result<Plan> {
    let [address, old, oldlen, new, _newlen] = args;
    let mut name = Vec::new();
    for offset in 0..=1024u64 {
        let byte = memory.load(
            address
                .checked_add(offset)
                .ok_or("sysctl name address overflow")?,
            1,
        )? as u8;
        if byte == 0 {
            if name.len() >= 1024 {
                return Ok(Plan::error(63));
            }
            let lookup = named(&name);
            // sysctlbyname resolves the name before entering numeric sysctl.
            if let Lookup::Error(error) = lookup {
                return Ok(Plan::error(error));
            }
            return output(memory, profile, lookup, old, oldlen, new);
        }
        name.push(byte);
    }
    Err("Unterminated sysctl name exceeds bounded scan".into())
}
fn output(
    memory: &Memory<'_>,
    profile: Option<&GuestSystemProfile>,
    lookup: Lookup,
    old: u64,
    oldlen: u64,
    new: u64,
) -> Result<Plan> {
    let capacity = if oldlen == 0 {
        0
    } else {
        memory.load(oldlen, 8)?
    };
    let leaf = match lookup {
        Lookup::Error(error) => return Ok(Plan::error(error)),
        Lookup::Unsupported => return Err("Unsupported guest sysctl node".into()),
        Lookup::Leaf(leaf) => leaf,
    };
    if new != 0 {
        return Ok(Plan::error(1));
    }
    let profile = profile.ok_or("Missing explicit guest system profile for sysctl")?;
    let bytes = match leaf {
        Leaf::Cpu => 1i32.to_le_bytes().to_vec(),
        Leaf::Page => (crate::guest_system::PAGE_SIZE as i32)
            .to_le_bytes()
            .to_vec(),
        Leaf::Physical | Leaf::Real => profile.reported_physical_bytes().to_le_bytes().to_vec(),
        Leaf::Available => (profile.reported_physical_bytes() / crate::guest_system::PAGE_SIZE)
            .to_le_bytes()
            .to_vec(),
    };
    let required = bytes.len() as u64;
    let copied = if old == 0 {
        required
    } else {
        capacity.min(required)
    };
    let mut writes = Vec::new();
    if old != 0 && copied != 0 {
        writes.push(Write {
            address: old,
            bytes: bytes[..copied as usize].to_vec(),
        });
    }
    if oldlen != 0 {
        writes.push(Write {
            address: oldlen,
            bytes: copied.to_le_bytes().to_vec(),
        });
    }
    Ok(Plan {
        writes,
        errno: (old != 0 && copied < required).then_some(12),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> GuestSystemProfile {
        GuestSystemProfile::new(16 << 30, 1, 1).unwrap()
    }
    #[test]
    fn partial_sizing_aliases_and_widths() {
        let mut bytes = [0xa5; 128];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.write(0x1000, b"hw.realmem\0").unwrap();
        for capacity in 0..=10 {
            memory.store(0x1020, capacity, 8).unwrap();
            let p =
                plan_named(&memory, Some(&profile()), [0x1000, 0x1040, 0x1020, 0, 123]).unwrap();
            assert_eq!(p.errno, (capacity < 8).then_some(12));
            p.commit(&mut memory).unwrap();
            assert_eq!(memory.load(0x1020, 8).unwrap(), capacity.min(8));
        }
        let p = plan_named(&memory, Some(&profile()), [0x1000, 0, 0x1020, 0, 0]).unwrap();
        assert_eq!(p.errno, None);
        p.commit(&mut memory).unwrap();
        assert_eq!(memory.load(0x1020, 8).unwrap(), 8);
        let p = plan_named(&memory, Some(&profile()), [0x1000, 0x1020, 0x1020, 0, 0]).unwrap();
        p.commit(&mut memory).unwrap();
        assert_eq!(memory.load(0x1020, 8).unwrap(), 8);
        assert_eq!(
            plan_named(&memory, Some(&profile()), [0x1000, 0x1040, 0, 0, 0])
                .unwrap()
                .errno,
            Some(12)
        );
        assert!(plan_named(&memory, Some(&profile()), [0x1000, 0, 0, 0, 0])
            .unwrap()
            .writes
            .is_empty());
    }
    #[test]
    fn profile_values_names_and_numeric_error_order() {
        let mut bytes = [0u8; 1152];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let profile = profile();
        for (name, width, expected) in [
            ("hw.ncpu", 4, 1),
            ("hw.pagesize", 4, 16384),
            ("hw.physmem", 8, 16u64 << 30),
            ("hw.realmem", 8, 16u64 << 30),
            ("hw.availpages", 8, 1_048_576),
        ] {
            memory
                .write(0x1000, format!("{name}\0").as_bytes())
                .unwrap();
            memory.store(0x1040, 16, 8).unwrap();
            let plan = plan_named(&memory, Some(&profile), [0x1000, 0x1050, 0x1040, 0, 0]).unwrap();
            assert_eq!(plan.errno, None);
            plan.commit(&mut memory).unwrap();
            assert_eq!(memory.load(0x1040, 8).unwrap(), width);
            assert_eq!(memory.load(0x1050, width).unwrap(), expected);
        }
        memory
            .write(0x1000, &[6i32.to_le_bytes(), 99i32.to_le_bytes()].concat())
            .unwrap();
        assert!(plan_numeric(&memory, None, [0x1000, 2, 0, u64::MAX, 0, 0]).is_err());
        assert_eq!(
            plan_numeric(&memory, None, [0x1000, 2, 0, 0, 0, 0])
                .unwrap()
                .errno,
            Some(2)
        );
        memory.write(0x1000, &vec![b'x'; 1024]).unwrap();
        memory.store(0x1400, 0, 1).unwrap();
        assert_eq!(
            plan_named(&memory, None, [0x1000, 0, u64::MAX, 0, 0])
                .unwrap()
                .errno,
            Some(63)
        );
        for name in [b"hw.ncpu..".as_slice(), b"HW.ncpu", b"hw..ncpu", b""] {
            assert!(matches!(named(name), Lookup::Error(2)));
        }
    }
    #[test]
    fn resolution_permissions_and_transaction_guards() {
        let mut bytes = [0; 64];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        memory.write(0x1000, b"hw.ncpu.extra\0").unwrap();
        assert_eq!(
            plan_named(&memory, None, [0x1000, 0, u64::MAX, 0, 0])
                .unwrap()
                .errno,
            Some(2)
        );
        memory.write(0x1000, b"hw.ncpu.\0").unwrap();
        assert_eq!(
            plan_named(&memory, None, [0x1000, 0, 0, u64::MAX, 0])
                .unwrap()
                .errno,
            Some(1)
        );
        assert!(plan_named(&memory, None, [0x1000, 0, u64::MAX, 1, 0]).is_err());
        memory.store(0x1020, 4, 8).unwrap();
        let p = plan_named(&memory, Some(&profile()), [0x1000, 0x103e, 0x1020, 0, 0]).unwrap();
        let before = memory.read(0x1000, 64).unwrap().to_vec();
        assert!(p.commit(&mut memory).is_err());
        assert_eq!(memory.read(0x1000, 64).unwrap(), before);
        assert_eq!(
            plan_numeric(&memory, None, [u64::MAX, 1, u64::MAX, u64::MAX, 1, 0])
                .unwrap()
                .errno,
            Some(22)
        );
        memory
            .write(
                0x1000,
                &[6i32.to_le_bytes(), 3i32.to_le_bytes(), 1i32.to_le_bytes()].concat(),
            )
            .unwrap();
        assert_eq!(
            plan_numeric(&memory, None, [0x1000, 3, 0, 0, 0, 0])
                .unwrap()
                .errno,
            Some(20)
        );
        assert!(matches!(hardware(1), Lookup::Unsupported));
        assert!(matches!(named(b"hw.machine"), Lookup::Unsupported));
        for name in [
            b"hw.clockrate".as_slice(),
            b"hw.hv_vendor",
            b"hw.uninventoried",
        ] {
            assert!(matches!(named(name), Lookup::Unsupported));
        }
        assert!(matches!(numeric(&[256, 1]), Lookup::Unsupported));
        assert!(matches!(named(b"hw.ncpu.extra"), Lookup::Error(2)));
    }
}
