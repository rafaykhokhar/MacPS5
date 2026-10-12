//! Scoped aligned LOCK XADD32. AMD APM vol3 XADD specifies old DEST to SRC,
//! sum to DEST and ADD arithmetic flags. Other LOCK widths/forms stay unsupported.
//! Primary: https://www.amd.com/content/dam/amd/en/documents/processor-tech-docs/programmer-references/24594.pdf
//! Atomic safety: https://doc.rust-lang.org/std/sync/atomic/struct.AtomicU32.html
use std::sync::atomic::{AtomicU32, Ordering};
pub(crate) fn exchange_add(value: &AtomicU32, addend: u32) -> u32 {
    value.fetch_add(addend, Ordering::SeqCst)
}
#[allow(unsafe_code)]
mod ffi {
    use crate::{guest_memory::Memory, legacy_cpu::Cpu};
    /// # Safety
    /// CPU and its Memory context are valid exclusive disjoint objects for this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_module_atomic_xadd32_v1(
        cpu: *mut Cpu,
        address: u64,
        register: u64,
    ) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        if cpu.fault != 0 {
            return;
        }
        if register >= 16 || cpu.memory == 0 {
            cpu.fault = 2;
            return;
        }
        let addend = cpu.registers[register as usize] as u32;
        let memory = unsafe { &mut *(cpu.memory as *mut Memory<'_>) };
        match memory.atomic_xadd_u32(address, addend) {
            Ok(old) => {
                let flags = crate::arithmetic::flags(
                    cpu.flags,
                    old as u64,
                    addend as u64,
                    old.wrapping_add(addend) as u64,
                    32,
                    1,
                );
                cpu.registers[register as usize] = old as u64;
                cpu.flags = flags.flags;
            }
            Err(_) => cpu.fault = 2,
        }
    }
}
#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use crate::{guest_memory::Memory, legacy_cpu::Cpu};
    #[repr(align(4))]
    struct Bytes([u8; 16]);
    #[test]
    fn flags_guards_and_zeroextension() {
        for (old, add, expected) in [(u32::MAX, 1, 0x55), (0x7fffffff, 1, 0x894), (0xf, 1, 0x10)] {
            let mut bytes = Bytes([0; 16]);
            bytes.0[..4].copy_from_slice(&old.to_le_bytes());
            let mut memory = Memory::default();
            memory.add(0x1000, &mut bytes.0, true, false).unwrap();
            let mut cpu = Cpu {
                memory: &mut memory as *mut _ as u64,
                flags: 0x202,
                ..Default::default()
            };
            cpu.registers[0] = 0xffff000000000000 | add;
            unsafe { ffi::native_ps5_module_atomic_xadd32_v1(&mut cpu, 0x1000, 0) };
            assert_eq!(cpu.fault, 0);
            assert_eq!(cpu.registers[0], old as u64);
            assert_eq!(cpu.flags, 0x202 | expected);
            assert_eq!(
                memory.load(0x1000, 4).unwrap(),
                old.wrapping_add(add as u32) as u64
            );
            for address in [0x1001, 0x100e, u64::MAX - 3] {
                let registers = cpu.registers;
                let flags = cpu.flags;
                let snapshot = memory.read(0x1000, 16).unwrap().to_vec();
                unsafe { ffi::native_ps5_module_atomic_xadd32_v1(&mut cpu, address, 0) };
                assert_eq!(cpu.fault, 2);
                assert_eq!(cpu.registers, registers);
                assert_eq!(cpu.flags, flags);
                assert_eq!(memory.read(0x1000, 16).unwrap(), snapshot);
                cpu.fault = 0;
            }
        }
        let mut bytes = Bytes([0; 16]);
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes.0, false, false).unwrap();
        assert!(memory.atomic_xadd_u32(0x1000, 1).is_err());
    }
    #[test]
    fn actual_atomic_contention() {
        let value = AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let value = &value;
                scope.spawn(move || {
                    for _ in 0..10000 {
                        exchange_add(value, 1);
                    }
                });
            }
        });
        assert_eq!(value.load(Ordering::SeqCst), 40000);
    }
    #[test]
    fn exact_form_only() {
        let asm =
            crate::aot::translate_rust_memory_region(&[0xf0, 0x0f, 0xc1, 0x07], 0, "xadd").unwrap();
        assert!(asm.contains("native_ps5_module_atomic_xadd32_v1"));
        for bytes in [
            &[0xf0, 0x48, 0x0f, 0xc1, 0x07][..],
            &[0xf0, 0x01, 0x07],
            &[0x0f, 0xc1, 0x07],
        ] {
            assert!(crate::aot::translate_rust_memory_region(bytes, 0, "no").is_err());
        }
    }
}
