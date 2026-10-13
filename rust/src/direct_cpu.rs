//! Initial direct x86-to-ARM64 lowering using the shared Cpu dispatcher ABI.
use crate::{
    Result,
    arm64_code::{Arm64Code, Branch},
    decode,
};
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};
fn byte_arithmetic(i: &Instruction) -> bool {
    matches!(
        i.mnemonic(),
        Mnemonic::Add
            | Mnemonic::Sub
            | Mnemonic::Cmp
            | Mnemonic::And
            | Mnemonic::Or
            | Mnemonic::Xor
            | Mnemonic::Test
    ) && i.op0_kind() == OpKind::Register
        && i.op0_register().size() == 1
        && (i.op1_kind() == OpKind::Immediate8
            || (i.op1_kind() == OpKind::Register && i.op1_register().size() == 1))
}
fn setcc(i: &Instruction) -> bool {
    matches!(
        i.code(),
        Code::Seto_rm8
            | Code::Setno_rm8
            | Code::Setb_rm8
            | Code::Setae_rm8
            | Code::Sete_rm8
            | Code::Setne_rm8
            | Code::Setbe_rm8
            | Code::Seta_rm8
            | Code::Sets_rm8
            | Code::Setns_rm8
            | Code::Setp_rm8
            | Code::Setnp_rm8
            | Code::Setl_rm8
            | Code::Setge_rm8
            | Code::Setle_rm8
            | Code::Setg_rm8
    )
}
fn immediate_shift(i: &Instruction) -> bool {
    matches!(i.mnemonic(), Mnemonic::Shl | Mnemonic::Shr | Mnemonic::Sar)
        && i.op0_kind() == OpKind::Register
        && matches!(i.op0_register().size(), 4 | 8)
        && i.op1_kind() == OpKind::Immediate8
}
fn arithmetic_supported(i: &Instruction) -> bool {
    if i.op0_kind() == OpKind::Memory
        && matches!(
            i.code(),
            Code::Cmp_rm32_r32 | Code::Cmp_rm64_r64 | Code::Test_rm32_r32 | Code::Test_rm64_r64
        )
    {
        return true;
    }
    if matches!(
        i.code(),
        Code::Inc_rm32 | Code::Inc_rm64 | Code::Dec_rm32 | Code::Dec_rm64
    ) {
        return i.op0_kind() == OpKind::Register;
    }
    matches!(
        i.mnemonic(),
        Mnemonic::Add
            | Mnemonic::Sub
            | Mnemonic::Cmp
            | Mnemonic::And
            | Mnemonic::Or
            | Mnemonic::Xor
            | Mnemonic::Test
    ) && i.op0_kind() == OpKind::Register
        && matches!(i.op0_register().size(), 4 | 8)
        && match i.op1_kind() {
            OpKind::Memory => true,
            OpKind::Register => i.op0_register().size() == i.op1_register().size(),
            OpKind::Immediate32 | OpKind::Immediate8to32 => i.op0_register().size() == 4,
            OpKind::Immediate32to64 | OpKind::Immediate8to64 => i.op0_register().size() == 8,
            _ => false,
        }
}
fn memory_address(c: &mut Arm64Code, i: &Instruction) -> Result<()> {
    if i.mnemonic() != Mnemonic::Lea && matches!(i.memory_segment(), Register::FS | Register::GS) {
        return Err("Direct FS/GS addressing unsupported".into());
    }
    if i.is_ip_rel_memory_operand() {
        if i.memory_base() != Register::RIP {
            return Err("Direct 32-bit addressing unsupported".into());
        }
        c.context_access(1, 19, 144, true, false)?;
        c.load_u64(9, i.ip_rel_memory_address())?;
        return c.emit(0x8b090021); // add x1,x1,x9
    }
    if i.memory_base() == Register::None {
        if i.memory_index() == Register::None && i.memory_displ_size() != 8 {
            return Err("Direct absolute 32-bit addressing unsupported".into());
        }
        c.load_u64(1, 0)?;
    } else {
        c.context_access(1, 19, register(i.memory_base(), true)? * 8, true, false)?;
    }
    if i.memory_index() != Register::None {
        c.context_access(9, 19, register(i.memory_index(), true)? * 8, true, false)?;
        c.emit(0x8b090021 | (i.memory_index_scale().trailing_zeros() << 10))?;
    }
    if i.memory_displacement64() != 0 {
        c.load_u64(9, i.memory_displacement64())?;
        c.emit(0x8b090021)?;
    }
    Ok(())
}
fn register(register: Register, wide: bool) -> Result<usize> {
    let first = if wide { Register::RAX } else { Register::EAX } as u32;
    let value = register as u32;
    if (first..first + 16).contains(&value) {
        Ok((value - first) as usize)
    } else {
        Err("Direct MOV requires a 32/64-bit general register".into())
    }
}
fn byte_offset(register: Register) -> Result<u32> {
    let r = register as u32;
    if (Register::AL as u32..=Register::BL as u32).contains(&r) {
        Ok((r - Register::AL as u32) * 8)
    } else if (Register::AH as u32..=Register::BH as u32).contains(&r) {
        Ok((r - Register::AH as u32) * 8 + 1)
    } else if (Register::SPL as u32..=Register::R15L as u32).contains(&r) {
        Ok((r - Register::SPL as u32 + 4) * 8)
    } else {
        Err("Invalid direct byte register".into())
    }
}
pub fn translate(bytes: &[u8], address: u64, instruction_limit: usize) -> Result<Vec<u32>> {
    translate_span(bytes, address, instruction_limit, false)
}
/// Permit a dispatcher handoff at the end of a nonterminal span when discovery
/// has verified that the following PC already has a registered translation.
pub(crate) fn translate_span(
    bytes: &[u8],
    address: u64,
    instruction_limit: usize,
    fallthrough: bool,
) -> Result<Vec<u32>> {
    let mut decoded = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        if decoded.len() >= instruction_limit {
            return Err("Direct guest instruction budget exceeded".into());
        }
        let pc = address
            .checked_add(offset as u64)
            .ok_or("Direct PC overflow")?;
        let i = decode::instruction(&bytes[offset..], pc)?;
        match i.code() {
            Code::Nopd
            | Code::Nopq
            | Code::Nop_rm16
            | Code::Nop_rm32
            | Code::Nop_rm64
            | Code::Retnq => {}
            Code::Push_r64 | Code::Pop_r64 => {}
            Code::Lea_r32_m | Code::Lea_r64_m => {}
            Code::Mov_r32_imm32 | Code::Mov_r64_imm64 => {}
            Code::Mov_rm32_imm32 | Code::Mov_rm64_imm32 => {}
            Code::Mov_r8_imm8 | Code::Mov_rm8_imm8 => {}
            Code::Movzx_r32_rm8
            | Code::Movzx_r64_rm8
            | Code::Movzx_r32_rm16
            | Code::Movzx_r64_rm16 => {}
            Code::Mov_r32_rm32 | Code::Mov_rm32_r32 | Code::Mov_r64_rm64 | Code::Mov_rm64_r64 => {}
            _ if i.is_jcc_short_or_near() || i.is_jmp_short_or_near() => {}
            _ if setcc(&i) => {}
            Code::Call_rel32_64 | Code::Call_rm64 | Code::Jmp_rm64 => {}
            _ if arithmetic_supported(&i) => {}
            _ if byte_arithmetic(&i) => {}
            _ if immediate_shift(&i) => {}
            _ => return Err(format!("Direct lowering unsupported: {:?}", i.code())),
        }
        offset += i.len();
        if i.code() == Code::Retnq && offset != bytes.len() {
            return Err("Direct block bytes continue after RET".into());
        }
        if matches!(i.code(), Code::Call_rel32_64 | Code::Call_rm64) && offset != bytes.len() {
            return Err(
                "Direct CALL must end its registered span; register the continuation separately"
                    .into(),
            );
        }
        if i.code() == Code::Jmp_rm64 && offset != bytes.len() {
            return Err("Direct indirect JMP must end its registered span".into());
        }
        decoded.push(i);
    }
    if decoded.last().is_none_or(|i| {
        !(fallthrough && i.flow_control() == iced_x86::FlowControl::Next)
            && i.code() != Code::Retnq
            && !matches!(
                i.code(),
                Code::Call_rel32_64 | Code::Call_rm64 | Code::Jmp_rm64
            )
            && !i.is_jmp_short_or_near()
            && !i.is_jcc_short_or_near()
    }) {
        return Err("Direct block must end in RET64, CALL64 or a supported branch".into());
    }
    const EXIT: u32 = u32::MAX;
    const FUEL: u32 = u32::MAX - 1;
    if decoded.len() >= FUEL as usize {
        return Err("Too many direct labels".into());
    }
    let mut boundaries: std::collections::BTreeMap<_, _> = decoded
        .iter()
        .enumerate()
        .map(|(index, i)| (i.ip(), index as u32))
        .collect();
    let end = decoded.last().unwrap().next_ip();
    let mut external = std::collections::BTreeSet::new();
    if fallthrough || decoded.last().unwrap().is_jcc_short_or_near() {
        external.insert(end);
    }
    for i in &decoded {
        if (i.is_jcc_short_or_near() || i.is_jmp_short_or_near())
            && !boundaries.contains_key(&i.near_branch_target())
        {
            let target = i.near_branch_target();
            if (address..end).contains(&target) {
                return Err("Direct branch enters an instruction interior".into());
            }
            external.insert(target);
        }
    }
    for target in &external {
        let label = u32::try_from(boundaries.len()).map_err(|_| "Too many direct labels")?;
        if label >= FUEL {
            return Err("Too many direct labels".into());
        }
        boundaries.insert(*target, label);
    }
    let limit = decoded
        .len()
        .checked_mul(96)
        .and_then(|n| n.checked_add(32))
        .ok_or("Direct emission budget overflow")?;
    let mut c = Arm64Code::new(limit);
    c.emit(0xa9bf7bf3)?; // stp x19,x30,[sp,#-16]!
    c.emit(0xaa0003f3)?; // mov x19,x0
    c.context_access(9, 19, 152, true, false)?;
    c.branch(
        EXIT,
        Branch::CompareZero {
            register: 9,
            wide: true,
            nonzero: true,
        },
    )?;
    c.context_access(10, 19, 128, true, false)?;
    for (index, i) in decoded.iter().enumerate() {
        c.load_u64(9, i.ip())?;
        c.emit(0xeb09015f)?; // cmp x10,x9
        c.branch(index as u32, Branch::Conditional(0))?;
    }
    c.load_u64(9, 7)?;
    c.context_access(9, 19, 152, true, true)?;
    c.branch(EXIT, Branch::Jump)?;
    for (index, i) in decoded.iter().enumerate() {
        c.bind(index as u32)?;
        c.load_u64(9, i.ip())?;
        c.context_access(9, 19, 128, true, true)?;
        c.context_access(9, 19, 160, true, false)?;
        c.branch(
            FUEL,
            Branch::CompareZero {
                register: 9,
                wide: true,
                nonzero: false,
            },
        )?;
        c.emit(0xd1000529)?; // sub x9,x9,#1
        c.context_access(9, 19, 160, true, true)?;
        if byte_arithmetic(i) {
            let destination = byte_offset(i.op0_register())?;
            c.emit(0x39400261 | (destination << 10))?; // ldrb w1,[x19,#offset]
            if i.op1_kind() == OpKind::Register {
                c.emit(0x39400262 | (byte_offset(i.op1_register())? << 10))?;
            } else {
                c.load_u64(2, u64::from(i.immediate8()))?;
            }
            let (opcode, operation) = match i.mnemonic() {
                Mnemonic::Add => (0x0b020023, 1),
                Mnemonic::Sub | Mnemonic::Cmp => (0x4b020023, 0),
                Mnemonic::And | Mnemonic::Test => (0x0a020023, 2),
                Mnemonic::Or => (0x2a020023, 2),
                Mnemonic::Xor => (0x4a020023, 2),
                _ => unreachable!(),
            };
            c.emit(opcode)?;
            if !matches!(i.mnemonic(), Mnemonic::Cmp | Mnemonic::Test) {
                c.emit(0x39000263 | (destination << 10))?; // strb w3,[x19,#offset]
            }
            c.context_access(0, 19, 136, true, false)?;
            c.load_u64(4, 8)?;
            c.load_u64(5, operation)?;
            c.call_absolute(crate::arithmetic::native_ps5_arithmetic_flags_v1 as *const () as u64)?;
            c.context_access(0, 19, 136, true, true)?;
            c.context_access(1, 19, 152, true, true)?;
            continue;
        }
        if setcc(i) {
            let condition = (i.condition_code() as u64)
                .checked_sub(1)
                .filter(|cc| *cc < 16)
                .ok_or("Invalid direct SETcc condition")?;
            c.emit(0xaa1303e0)?; // mov x0,x19
            c.load_u64(1, condition)?;
            c.call_absolute(crate::condition::ffi::native_ps5_condition_v1 as *const () as u64)?;
            if i.op0_kind() == OpKind::Register {
                c.emit(0x39000260 | (byte_offset(i.op0_register())? << 10))?; // strb w0,[x19,#offset]
            } else {
                c.emit(0xaa0003e2)?; // mov x2,x0; address lowering preserves x2
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(3, 1)?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_store_v1 as *const () as u64,
                )?;
                c.context_access(9, 19, 152, true, false)?;
                c.branch(
                    EXIT,
                    Branch::CompareZero {
                        register: 9,
                        wide: true,
                        nonzero: true,
                    },
                )?;
            }
            continue;
        }
        if immediate_shift(i) {
            let wide = i.op0_register().size() == 8;
            let destination = register(i.op0_register(), wide)?;
            let count = u64::from(i.immediate8()) & if wide { 63 } else { 31 };
            c.context_access(1, 19, destination * 8, wide, false)?;
            c.load_u64(2, count)?;
            let (opcode, operation) = match i.mnemonic() {
                Mnemonic::Shl => (0x1ac22023, 0),
                Mnemonic::Shr => (0x1ac22423, 1),
                Mnemonic::Sar => (0x1ac22823, 2),
                _ => unreachable!(),
            };
            c.emit(opcode | if wide { 0x80000000 } else { 0 })?;
            c.context_access(3, 19, destination * 8, true, true)?;
            c.context_access(0, 19, 136, true, false)?;
            c.load_u64(4, if wide { 64 } else { 32 })?;
            c.load_u64(5, operation)?;
            c.call_absolute(crate::shift_flags::native_ps5_shift_flags_v1 as *const () as u64)?;
            c.context_access(0, 19, 136, true, true)?;
            c.context_access(1, 19, 152, true, true)?;
            continue;
        }
        if matches!(
            i.code(),
            Code::Movzx_r32_rm8 | Code::Movzx_r64_rm8 | Code::Movzx_r32_rm16 | Code::Movzx_r64_rm16
        ) {
            let byte = matches!(i.code(), Code::Movzx_r32_rm8 | Code::Movzx_r64_rm8);
            if i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, if byte { 1 } else { 2 })?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_load_v1 as *const () as u64,
                )?;
                c.context_access(9, 19, 152, true, false)?;
                c.branch(
                    EXIT,
                    Branch::CompareZero {
                        register: 9,
                        wide: true,
                        nonzero: true,
                    },
                )?;
            } else if byte {
                c.emit(0x39400260 | (byte_offset(i.op1_register())? << 10))?; // ldrb w0,[x19,#offset]
            } else {
                let r = i.op1_register() as u32;
                if !(Register::AX as u32..=Register::R15W as u32).contains(&r) {
                    return Err("Invalid direct word register".into());
                }
                c.emit(0x79400260 | (((r - Register::AX as u32) * 4) << 10))?; // ldrh w0,[x19,#reg*8]
            }
            c.context_access(
                0,
                19,
                register(i.op0_register(), i.op0_register().size() == 8)? * 8,
                true,
                true,
            )?;
            continue;
        }
        if matches!(
            i.code(),
            Code::Mov_rm32_imm32 | Code::Mov_rm64_imm32 | Code::Mov_r8_imm8 | Code::Mov_rm8_imm8
        ) {
            let wide = i.code() == Code::Mov_rm64_imm32;
            let byte = matches!(i.code(), Code::Mov_r8_imm8 | Code::Mov_rm8_imm8);
            let value = if byte {
                u64::from(i.immediate8())
            } else if wide {
                i.immediate32to64() as u64
            } else {
                u64::from(i.immediate32())
            };
            if i.op0_kind() == OpKind::Register {
                c.load_u64(9, value)?;
                if byte {
                    let offset = byte_offset(i.op0_register())?;
                    c.emit(0x39000269 | (offset << 10))?; // strb w9,[x19,#offset]
                } else {
                    c.context_access(9, 19, register(i.op0_register(), wide)? * 8, true, true)?;
                }
            } else {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, value)?;
                c.load_u64(
                    3,
                    if byte {
                        1
                    } else if wide {
                        8
                    } else {
                        4
                    },
                )?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_store_v1 as *const () as u64,
                )?;
                c.context_access(9, 19, 152, true, false)?;
                c.branch(
                    EXIT,
                    Branch::CompareZero {
                        register: 9,
                        wide: true,
                        nonzero: true,
                    },
                )?;
            }
            continue;
        }
        if matches!(i.code(), Code::Lea_r32_m | Code::Lea_r64_m) {
            let wide = i.code() == Code::Lea_r64_m;
            memory_address(&mut c, i)?;
            if !wide {
                c.emit(0x2a0103e1)?;
            } // mov w1,w1, zero-extend result
            c.context_access(1, 19, register(i.op0_register(), wide)? * 8, true, true)?;
            continue;
        }
        if matches!(i.code(), Code::Push_r64 | Code::Pop_r64) {
            let push = i.code() == Code::Push_r64;
            let reg = register(i.op0_register(), true)?;
            c.context_access(1, 19, 32, true, false)?;
            c.emit(0xaa1303e0)?; // mov x0,x19
            if push {
                c.emit(0xd1002021)?; // sub x1,x1,#8
                c.context_access(2, 19, reg * 8, true, false)?;
                c.load_u64(3, 8)?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_store_v1 as *const () as u64,
                )?;
            } else {
                c.load_u64(2, 8)?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_load_v1 as *const () as u64,
                )?;
            }
            c.context_access(9, 19, 152, true, false)?;
            c.branch(
                EXIT,
                Branch::CompareZero {
                    register: 9,
                    wide: true,
                    nonzero: true,
                },
            )?;
            // Helpers may clobber caller-saved registers; recompute RSP only
            // after success. PUSH RSP stores the old value, POP RSP writes the
            // loaded value after the ordinary increment.
            c.context_access(9, 19, 32, true, false)?;
            c.emit(if push { 0xd1002129 } else { 0x91002129 })?;
            c.context_access(9, 19, 32, true, true)?;
            if !push {
                c.context_access(0, 19, reg * 8, true, true)?;
            }
            continue;
        }
        if matches!(
            i.code(),
            Code::Call_rel32_64 | Code::Call_rm64 | Code::Jmp_rm64
        ) {
            // The dispatcher owns guarded stack push/pop and target validation.
            // Resolve an indirect operand before publishing any call state.
            // Memory operands (including [RSP]) use the pre-push guest state.
            if i.code() == Code::Call_rel32_64 {
                c.load_u64(9, i.near_branch_target())?;
                c.context_access(10, 19, 144, true, false)?;
                c.emit(0x8b0a0129)?; // add x9,x9,x10 (image bias)
            } else if i.op0_kind() == OpKind::Register {
                c.context_access(9, 19, register(i.op0_register(), true)? * 8, true, false)?;
            } else {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, 8)?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_load_v1 as *const () as u64,
                )?;
                c.context_access(9, 19, 152, true, false)?;
                c.branch(
                    EXIT,
                    Branch::CompareZero {
                        register: 9,
                        wide: true,
                        nonzero: true,
                    },
                )?;
                c.emit(0xaa0003e9)?; // mov x9,x0
            }
            c.context_access(9, 19, 176, true, true)?;
            let tail = i.code() == Code::Jmp_rm64;
            if !tail {
                c.load_u64(9, i.next_ip())?;
                c.context_access(9, 19, 192, true, true)?;
            }
            c.load_u64(9, if tail { 5 } else { 1 })?;
            c.context_access(9, 19, 168, true, true)?;
            c.branch(EXIT, Branch::Jump)?;
            continue;
        }
        if i.is_jmp_short_or_near() {
            c.branch(boundaries[&i.near_branch_target()], Branch::Jump)?;
            continue;
        }
        if i.is_jcc_short_or_near() {
            let condition = (i.condition_code() as u64)
                .checked_sub(1)
                .filter(|cc| *cc < 16)
                .ok_or("Invalid direct branch condition")?;
            c.emit(0xaa1303e0)?; // mov x0,x19
            c.load_u64(1, condition)?;
            c.call_absolute(crate::condition::ffi::native_ps5_condition_v1 as *const () as u64)?;
            c.branch(
                boundaries[&i.near_branch_target()],
                Branch::CompareZero {
                    register: 0,
                    wide: true,
                    nonzero: true,
                },
            )?;
            continue;
        }
        if arithmetic_supported(i) {
            let memory_first = i.op0_kind() == OpKind::Memory;
            let wide = if memory_first {
                i.op1_register().size() == 8
            } else {
                i.op0_register().size() == 8
            };
            let destination = if memory_first {
                None
            } else {
                Some(register(i.op0_register(), wide)?)
            };
            if matches!(i.mnemonic(), Mnemonic::Inc | Mnemonic::Dec) {
                c.load_u64(2, 1)?;
            } else if memory_first || i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, if wide { 8 } else { 4 })?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_load_v1 as *const () as u64,
                )?;
                c.context_access(9, 19, 152, true, false)?;
                c.branch(
                    EXIT,
                    Branch::CompareZero {
                        register: 9,
                        wide: true,
                        nonzero: true,
                    },
                )?;
                if memory_first {
                    c.emit(0xaa0003e1)?; // mov x1,x0
                    c.context_access(2, 19, register(i.op1_register(), wide)? * 8, wide, false)?;
                } else {
                    c.emit(0xaa0003e2)?; // mov x2,x0
                }
            } else if i.op1_kind() == OpKind::Register {
                c.context_access(2, 19, register(i.op1_register(), wide)? * 8, wide, false)?;
            } else {
                let immediate = match i.op1_kind() {
                    OpKind::Immediate32 => u64::from(i.immediate32()),
                    OpKind::Immediate8to32 => u64::from(i.immediate8to32() as u32),
                    OpKind::Immediate32to64 => i.immediate32to64() as u64,
                    OpKind::Immediate8to64 => i.immediate8to64() as u64,
                    _ => return Err("Invalid direct arithmetic immediate".into()),
                };
                c.load_u64(2, immediate)?;
            }
            if let Some(destination) = destination {
                c.context_access(1, 19, destination * 8, wide, false)?;
            }
            let (opcode, operation) = match i.mnemonic() {
                Mnemonic::Add => (0x0b020023, 1),
                Mnemonic::Inc => (0x0b020023, 3),
                Mnemonic::Dec => (0x4b020023, 5),
                Mnemonic::Sub | Mnemonic::Cmp => (0x4b020023, 0),
                Mnemonic::And | Mnemonic::Test => (0x0a020023, 2),
                Mnemonic::Or => (0x2a020023, 2),
                Mnemonic::Xor => (0x4a020023, 2),
                _ => unreachable!(),
            };
            c.emit(opcode | if wide { 0x80000000 } else { 0 })?;
            if !matches!(i.mnemonic(), Mnemonic::Cmp | Mnemonic::Test) {
                c.context_access(
                    3,
                    19,
                    destination.ok_or("Direct arithmetic needs destination")? * 8,
                    true,
                    true,
                )?;
            }
            c.context_access(0, 19, 136, true, false)?;
            c.load_u64(4, if wide { 64 } else { 32 })?;
            c.load_u64(5, operation)?;
            // Valid constant width/operation make this integer-only helper
            // infallible; operands and result were captured before alias writes.
            c.call_absolute(crate::arithmetic::native_ps5_arithmetic_flags_v1 as *const () as u64)?;
            c.context_access(0, 19, 136, true, true)?;
            c.context_access(1, 19, 152, true, true)?;
            continue;
        }
        match i.code() {
            Code::Nopd | Code::Nopq | Code::Nop_rm16 | Code::Nop_rm32 | Code::Nop_rm64 => {}
            Code::Retnq => {
                c.load_u64(9, 3)?;
                c.context_access(9, 19, 168, true, true)?;
                c.load_u64(9, 0)?;
                c.context_access(9, 19, 176, true, true)?;
                c.branch(EXIT, Branch::Jump)?;
            }
            _ => {
                if i.op0_kind() == OpKind::Memory || i.op1_kind() == OpKind::Memory {
                    let store = i.op0_kind() == OpKind::Memory;
                    let operand = if store {
                        i.op1_register()
                    } else {
                        i.op0_register()
                    };
                    let wide = operand.size() == 8;
                    let reg = register(operand, wide)?;
                    memory_address(&mut c, i)?;
                    c.emit(0xaa1303e0)?; // mov x0,x19
                    if store {
                        c.context_access(2, 19, reg * 8, wide, false)?;
                        c.load_u64(3, if wide { 8 } else { 4 })?;
                        c.call_absolute(
                            crate::guest_memory::ffi::native_ps5_module_store_v1 as *const ()
                                as u64,
                        )?;
                    } else {
                        c.load_u64(2, if wide { 8 } else { 4 })?;
                        c.call_absolute(
                            crate::guest_memory::ffi::native_ps5_module_load_v1 as *const () as u64,
                        )?;
                    }
                    c.context_access(9, 19, 152, true, false)?;
                    c.branch(
                        EXIT,
                        Branch::CompareZero {
                            register: 9,
                            wide: true,
                            nonzero: true,
                        },
                    )?;
                    if !store {
                        c.context_access(0, 19, reg * 8, true, true)?;
                    }
                    continue;
                }
                let wide = i.op0_register().size() == 8;
                let destination = register(i.op0_register(), wide)?;
                if i.op1_kind() == OpKind::Register {
                    let source = register(i.op1_register(), wide)?;
                    c.context_access(9, 19, source * 8, wide, false)?;
                } else {
                    c.load_u64(
                        9,
                        if wide {
                            i.immediate64()
                        } else {
                            u64::from(i.immediate32())
                        },
                    )?;
                }
                c.context_access(9, 19, destination * 8, true, true)?;
            }
        }
    }
    if fallthrough || decoded.last().unwrap().is_jcc_short_or_near() {
        c.branch(boundaries[&end], Branch::Jump)?;
    }
    for target in external {
        c.bind(boundaries[&target])?;
        c.load_u64(9, target)?;
        c.context_access(9, 19, 128, true, true)?;
        c.context_access(9, 19, 176, true, true)?;
        c.load_u64(9, 2)?;
        c.context_access(9, 19, 168, true, true)?;
        c.branch(EXIT, Branch::Jump)?;
    }
    c.bind(FUEL)?;
    c.load_u64(9, 4)?;
    c.context_access(9, 19, 168, true, true)?;
    c.bind(EXIT)?;
    c.emit(0xa8c17bf3)?;
    c.emit(0xd65f03c0)?;
    c.finish()
}
