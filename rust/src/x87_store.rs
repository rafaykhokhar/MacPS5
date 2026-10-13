//! Guarded extended-precision x87 store adapter. Operand policy is kept separate
//! from raw stack storage; no host floating-point conversion is used.
//! AMD APM volume 5 FST/FSTP specifies invalid for SNaN/unsupported inputs.
//! Masked SNaN quieting follows the general invalid-response policy; exact Zen2
//! payload propagation and fault precedence still need a hardware oracle.
use crate::{guest_memory::Memory, legacy_cpu::Cpu, x87_control, x87_state};
pub const UNSUPPORTED_OPERAND: u64 = 20;

pub fn store80(cpu: &mut Cpu, memory: &mut Memory<'_>, address: u64) {
    x87_control::wait(cpu);
    if cpu.fault != 0 {
        return;
    }
    if memory.validate_write(address, 10).is_err() {
        cpu.fault = 2;
        return;
    }
    // Stage the pop so a rejected write cannot consume ST(0).
    let mut stack = cpu.x87.clone();
    let mut status = cpu.x87_status;
    let transfer = match stack.pop_raw(cpu.x87_control, &mut status) {
        Ok(value) => value,
        Err(_) => {
            cpu.x87_status = status;
            cpu.fault = x87_control::PENDING_FAULT;
            return;
        }
    };
    let mut value = transfer.value;
    // AMD's general pseudo-denormal rule and FSTx exception list do not clearly
    // specify whether this non-arithmetic store normalizes the exponent. Keep
    // the ambiguity explicit rather than silently adopting Intel behavior.
    if x87_state::classify(value) == x87_state::Class::PseudoDenormal {
        cpu.fault = UNSUPPORTED_OPERAND;
        return;
    }
    if !transfer.masked_stack_fault
        && matches!(
            x87_state::classify(value),
            x87_state::Class::SignalingNaN | x87_state::Class::Unsupported
        )
    {
        status |= 1;
        if cpu.x87_control & 1 == 0 {
            cpu.x87_status = (status & !(7 << 11)) | (cpu.x87_status & (7 << 11)) | 0x8080;
            cpu.fault = x87_control::PENDING_FAULT;
            return;
        }
        if x87_state::classify(value) == x87_state::Class::SignalingNaN {
            value[7] |= 0x40;
        } else {
            value = x87_state::INDEFINITE;
        }
    }
    if memory.write(address, &value).is_err() {
        cpu.fault = 2;
        return;
    }
    cpu.x87 = stack;
    cpu.x87_status = status;
}

#[allow(unsafe_code)]
mod abi {
    use super::*;
    /// # Safety
    /// `cpu` is a unique live Rust-sized CPU context; its memory pointer refers
    /// to a disjoint live exclusively borrowed guest Memory for this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn native_ps5_x87_store80_v1(cpu: *mut Cpu, address: u64) {
        let Some(cpu) = (unsafe { cpu.as_mut() }) else {
            return;
        };
        x87_control::wait(cpu);
        if cpu.fault != 0 {
            return;
        }
        if cpu.memory == 0 {
            cpu.fault = 2;
            return;
        }
        let memory = unsafe { &mut *(cpu.memory as *mut Memory<'_>) };
        store80(cpu, memory, address);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_extended_store_encoding_only() {
        let assembly =
            crate::aot::translate_rust_memory_region(&[0xdb, 0x3f], 0, "store80").unwrap();
        assert!(assembly.contains("native_ps5_x87_store80_v1"));
        for bytes in [
            &[0xd9, 0x1f][..],
            &[0xdd, 0x1f],
            &[0xdd, 0xd8],
            &[0x67, 0xdb, 0x3f],
        ] {
            assert!(crate::aot::translate_rust_memory_region(bytes, 0, "unsupported").is_err());
        }
    }
    #[test]
    fn protected_runtime_storage_cannot_consume_stack() {
        let owner = crate::guest_memory::RuntimeOwner::new();
        let mut bytes = [0x5a; 16];
        let mut memory = Memory::default();
        memory.add_runtime_data(0x1000, &mut bytes, &owner).unwrap();
        let mut cpu = Cpu::default();
        cpu.x87
            .push_raw(cpu.x87_control, &mut cpu.x87_status, [0; 10])
            .unwrap();
        let original = cpu.clone();
        store80(&mut cpu, &mut memory, 0x1000);
        assert_eq!(cpu.fault, 2);
        assert_eq!(cpu.x87, original.x87);
        assert_eq!(cpu.x87_status, original.x87_status);
        assert_eq!(memory.read(0x1000, 16).unwrap(), [0x5a; 16]);
    }
    #[test]
    fn ambiguous_pseudo_denormal_preserves_memory_and_stack() {
        let mut bytes = [0x5a; 10];
        let mut memory = Memory::default();
        memory.add(0x1000, &mut bytes, true, false).unwrap();
        let mut cpu = Cpu::default();
        let mut value = [0; 10];
        value[7] = 0x80;
        cpu.x87
            .push_raw(cpu.x87_control, &mut cpu.x87_status, value)
            .unwrap();
        let original = cpu.clone();
        store80(&mut cpu, &mut memory, 0x1000);
        assert_eq!(cpu.fault, UNSUPPORTED_OPERAND);
        assert_eq!(cpu.x87, original.x87);
        assert_eq!(cpu.x87_status, original.x87_status);
        assert_eq!(memory.read(0x1000, 10).unwrap(), [0x5a; 10]);
    }
    #[test]
    fn amd_invalid_response_policy_commits_only_when_masked() {
        for snan in [false, true] {
            for masked in [false, true] {
                let mut bytes = [0x5a; 10];
                let mut memory = Memory::default();
                memory.add(0x1000, &mut bytes, true, false).unwrap();
                let mut cpu = Cpu::default();
                let mut value = [0; 10];
                value[0] = 0x23;
                value[7] = if snan { 0x80 } else { 0 };
                value[8] = 0xff;
                value[9] = 0x7f;
                cpu.x87
                    .push_raw(cpu.x87_control, &mut cpu.x87_status, value)
                    .unwrap();
                if !masked {
                    cpu.x87_control &= !1;
                }
                let original = cpu.clone();
                store80(&mut cpu, &mut memory, 0x1000);
                assert_eq!(cpu.x87_status & 1, 1);
                if masked {
                    assert_eq!(cpu.fault, 0);
                    let expected = if snan {
                        value[7] |= 0x40;
                        value
                    } else {
                        x87_state::INDEFINITE
                    };
                    assert_eq!(memory.read(0x1000, 10).unwrap(), expected);
                    assert_eq!(x87_state::top(cpu.x87_status), 0);
                } else {
                    assert_eq!(cpu.fault, x87_control::PENDING_FAULT);
                    assert_eq!(cpu.x87, original.x87);
                    assert_eq!(
                        x87_state::top(cpu.x87_status),
                        x87_state::top(original.x87_status)
                    );
                    assert_eq!(cpu.x87_status & 0x8080, 0x8080);
                    assert_eq!(memory.read(0x1000, 10).unwrap(), [0x5a; 10]);
                }
            }
        }
    }
}
