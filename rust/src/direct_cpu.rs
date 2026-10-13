//! Initial direct x86-to-ARM64 lowering using the shared Cpu dispatcher ABI.
use crate::{
    Result,
    arm64_code::{Arm64Code, Branch},
    decode,
};
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};
fn carry_arithmetic(i: &Instruction) -> bool {
    matches!(i.mnemonic(), Mnemonic::Adc | Mnemonic::Sbb)
        && i.encoding() == iced_x86::EncodingKind::Legacy
        && !i.has_lock_prefix()
        && i.op0_kind() == OpKind::Register
        && matches!(i.op0_register().size(), 1 | 2 | 4 | 8)
}
fn movbe(i: &Instruction) -> bool {
    matches!(
        i.code(),
        Code::Movbe_r16_m16
            | Code::Movbe_r32_m32
            | Code::Movbe_r64_m64
            | Code::Movbe_m16_r16
            | Code::Movbe_m32_r32
            | Code::Movbe_m64_r64
    )
}
fn bmi_shift(i: &Instruction) -> Option<u32> {
    match i.code() {
        Code::VEX_Shlx_r32_rm32_r32 | Code::VEX_Shlx_r64_rm64_r64 => Some(0x1ac12000),
        Code::VEX_Shrx_r32_rm32_r32 | Code::VEX_Shrx_r64_rm64_r64 => Some(0x1ac12400),
        Code::VEX_Sarx_r32_rm32_r32 | Code::VEX_Sarx_r64_rm64_r64 => Some(0x1ac12800),
        _ => None,
    }
}
fn memory_arithmetic(i: &Instruction) -> bool {
    matches!(
        i.mnemonic(),
        Mnemonic::Add
            | Mnemonic::Sub
            | Mnemonic::And
            | Mnemonic::Or
            | Mnemonic::Xor
            | Mnemonic::Adc
            | Mnemonic::Sbb
    ) && !i.has_lock_prefix()
        && i.encoding() == iced_x86::EncodingKind::Legacy
        && i.op0_kind() == OpKind::Memory
        && matches!(i.memory_size().size(), 1 | 2 | 4 | 8)
        && match i.op1_kind() {
            OpKind::Register => i.op1_register().size() == i.memory_size().size(),
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => true,
            _ => false,
        }
}
fn extending_move(i: &Instruction) -> Option<(u64, bool)> {
    match i.code() {
        Code::Movzx_r32_rm8 | Code::Movzx_r64_rm8 => Some((1, false)),
        Code::Movzx_r32_rm16 | Code::Movzx_r64_rm16 => Some((2, false)),
        Code::Movsx_r16_rm8 | Code::Movsx_r32_rm8 | Code::Movsx_r64_rm8 => Some((1, true)),
        Code::Movsx_r32_rm16 | Code::Movsx_r64_rm16 => Some((2, true)),
        Code::Movsxd_r64_rm32 => Some((4, true)),
        _ => None,
    }
}
fn conditional_move(i: &Instruction) -> bool {
    matches!(
        i.code(),
        Code::Cmovo_r16_rm16
            | Code::Cmovo_r32_rm32
            | Code::Cmovo_r64_rm64
            | Code::Cmovno_r16_rm16
            | Code::Cmovno_r32_rm32
            | Code::Cmovno_r64_rm64
            | Code::Cmovb_r16_rm16
            | Code::Cmovb_r32_rm32
            | Code::Cmovb_r64_rm64
            | Code::Cmovae_r16_rm16
            | Code::Cmovae_r32_rm32
            | Code::Cmovae_r64_rm64
            | Code::Cmove_r16_rm16
            | Code::Cmove_r32_rm32
            | Code::Cmove_r64_rm64
            | Code::Cmovne_r16_rm16
            | Code::Cmovne_r32_rm32
            | Code::Cmovne_r64_rm64
            | Code::Cmovbe_r16_rm16
            | Code::Cmovbe_r32_rm32
            | Code::Cmovbe_r64_rm64
            | Code::Cmova_r16_rm16
            | Code::Cmova_r32_rm32
            | Code::Cmova_r64_rm64
            | Code::Cmovs_r16_rm16
            | Code::Cmovs_r32_rm32
            | Code::Cmovs_r64_rm64
            | Code::Cmovns_r16_rm16
            | Code::Cmovns_r32_rm32
            | Code::Cmovns_r64_rm64
            | Code::Cmovp_r16_rm16
            | Code::Cmovp_r32_rm32
            | Code::Cmovp_r64_rm64
            | Code::Cmovnp_r16_rm16
            | Code::Cmovnp_r32_rm32
            | Code::Cmovnp_r64_rm64
            | Code::Cmovl_r16_rm16
            | Code::Cmovl_r32_rm32
            | Code::Cmovl_r64_rm64
            | Code::Cmovge_r16_rm16
            | Code::Cmovge_r32_rm32
            | Code::Cmovge_r64_rm64
            | Code::Cmovle_r16_rm16
            | Code::Cmovle_r32_rm32
            | Code::Cmovle_r64_rm64
            | Code::Cmovg_r16_rm16
            | Code::Cmovg_r32_rm32
            | Code::Cmovg_r64_rm64
    )
}
fn vector_move(i: &Instruction) -> Option<(u64, bool)> {
    match i.code() {
        Code::VEX_Vmovups_xmm_xmmm128
        | Code::VEX_Vmovups_xmmm128_xmm
        | Code::VEX_Vmovupd_xmm_xmmm128
        | Code::VEX_Vmovupd_xmmm128_xmm
        | Code::VEX_Vmovdqu_xmm_xmmm128
        | Code::VEX_Vmovdqu_xmmm128_xmm => Some((16, false)),
        Code::VEX_Vmovaps_xmm_xmmm128
        | Code::VEX_Vmovaps_xmmm128_xmm
        | Code::VEX_Vmovapd_xmm_xmmm128
        | Code::VEX_Vmovapd_xmmm128_xmm
        | Code::VEX_Vmovdqa_xmm_xmmm128
        | Code::VEX_Vmovdqa_xmmm128_xmm => Some((16, true)),
        Code::VEX_Vmovups_ymm_ymmm256
        | Code::VEX_Vmovups_ymmm256_ymm
        | Code::VEX_Vmovupd_ymm_ymmm256
        | Code::VEX_Vmovupd_ymmm256_ymm
        | Code::VEX_Vmovdqu_ymm_ymmm256
        | Code::VEX_Vmovdqu_ymmm256_ymm => Some((32, false)),
        Code::VEX_Vmovaps_ymm_ymmm256
        | Code::VEX_Vmovaps_ymmm256_ymm
        | Code::VEX_Vmovapd_ymm_ymmm256
        | Code::VEX_Vmovapd_ymmm256_ymm
        | Code::VEX_Vmovdqa_ymm_ymmm256
        | Code::VEX_Vmovdqa_ymmm256_ymm => Some((32, true)),
        _ => None,
    }
}
fn vector_xor(i: &Instruction) -> Option<u64> {
    match i.code() {
        Code::VEX_Vxorps_xmm_xmm_xmmm128 | Code::VEX_Vxorpd_xmm_xmm_xmmm128 => Some(16),
        Code::VEX_Vxorps_ymm_ymm_ymmm256 | Code::VEX_Vxorpd_ymm_ymm_ymmm256 => Some(32),
        _ => None,
    }
}
fn vector_register(r: Register, size: u64) -> Result<usize> {
    let base = if size == 16 {
        Register::XMM0
    } else {
        Register::YMM0
    };
    (r as usize)
        .checked_sub(base as usize)
        .filter(|n| *n < 16)
        .ok_or_else(|| "Invalid direct vector register".into())
}
fn byte_arithmetic(i: &Instruction) -> bool {
    if matches!(
        i.code(),
        Code::Cmp_rm8_imm8
            | Code::Cmp_rm8_r8
            | Code::Cmp_r8_rm8
            | Code::Test_rm8_imm8
            | Code::Test_rm8_r8
    ) && (i.op0_kind() == OpKind::Memory || i.op1_kind() == OpKind::Memory)
    {
        return true;
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
fn register_shift(i: &Instruction) -> bool {
    matches!(i.mnemonic(), Mnemonic::Shl | Mnemonic::Shr | Mnemonic::Sar)
        && i.op0_kind() == OpKind::Register
        && matches!(i.op0_register().size(), 4 | 8)
        && (i.op1_kind() == OpKind::Immediate8
            || (i.op1_kind() == OpKind::Register && i.op1_register() == Register::CL))
}
fn arithmetic_supported(i: &Instruction) -> bool {
    if i.op0_kind() == OpKind::Memory
        && matches!(
            i.code(),
            Code::Cmp_rm16_r16
                | Code::Cmp_rm32_r32
                | Code::Cmp_rm64_r64
                | Code::Test_rm16_r16
                | Code::Test_rm32_r32
                | Code::Test_rm64_r64
                | Code::Cmp_rm16_imm8
                | Code::Cmp_rm16_imm16
                | Code::Cmp_rm32_imm8
                | Code::Cmp_rm32_imm32
                | Code::Cmp_rm64_imm8
                | Code::Cmp_rm64_imm32
                | Code::Test_rm16_imm16
                | Code::Test_rm32_imm32
                | Code::Test_rm64_imm32
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
            Code::Nopw
            | Code::Nopd
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
            Code::Mov_r8_rm8 | Code::Mov_rm8_r8 | Code::Mov_r16_rm16 | Code::Mov_rm16_r16 => {}
            Code::Movzx_r32_rm8
            | Code::Movzx_r64_rm8
            | Code::Movzx_r32_rm16
            | Code::Movzx_r64_rm16 => {}
            _ if extending_move(&i).is_some() => {}
            Code::Mov_r32_rm32 | Code::Mov_rm32_r32 | Code::Mov_r64_rm64 | Code::Mov_rm64_r64 => {}
            _ if i.is_jcc_short_or_near() || i.is_jmp_short_or_near() => {}
            _ if setcc(&i) => {}
            Code::Call_rel32_64 | Code::Call_rm64 | Code::Jmp_rm64 => {}
            _ if arithmetic_supported(&i) => {}
            _ if byte_arithmetic(&i) => {}
            _ if register_shift(&i) => {}
            _ if vector_xor(&i).is_some() => {}
            _ if vector_move(&i).is_some() => {}
            Code::VEX_Vzeroupper | Code::VEX_Vzeroall => {}
            _ if conditional_move(&i) => {}
            _ if memory_arithmetic(&i) => {}
            Code::Not_rm8 | Code::Not_rm16 | Code::Not_rm32 | Code::Not_rm64
                if !i.has_lock_prefix() => {}
            Code::Neg_rm8 | Code::Neg_rm16 | Code::Neg_rm32 | Code::Neg_rm64
                if !i.has_lock_prefix() => {}
            Code::VEX_Bextr_r32_rm32_r32 | Code::VEX_Bextr_r64_rm64_r64 => {}
            _ if bmi_shift(&i).is_some() => {}
            _ if movbe(&i) => {}
            _ if carry_arithmetic(&i) => {}
            Code::VEX_Andn_r32_r32_rm32 | Code::VEX_Andn_r64_r64_rm64 => {}
            Code::VEX_Rorx_r32_rm32_imm8 | Code::VEX_Rorx_r64_rm64_imm8 => {}
            Code::VEX_Mulx_r32_r32_rm32 | Code::VEX_Mulx_r64_r64_rm64 => {}
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
        if matches!(i.code(), Code::VEX_Vzeroupper | Code::VEX_Vzeroall) {
            c.load_u64(9, 0)?;
            let first_byte = if i.code() == Code::VEX_Vzeroall {
                0
            } else {
                16
            };
            for reg in 0..16 {
                for offset in (first_byte..32).step_by(8) {
                    c.context_access(9, 19, 208 + reg * 32 + offset, true, true)?;
                }
            }
            continue;
        }
        if let Some((size, aligned)) = vector_move(i) {
            if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register {
                let dst = vector_register(i.op0_register(), size)?;
                let src = vector_register(i.op1_register(), size)?;
                for offset in (0..size as usize).step_by(8) {
                    c.context_access(9, 19, 208 + src * 32 + offset, true, false)?;
                    c.context_access(9, 19, 208 + dst * 32 + offset, true, true)?;
                }
                if size == 16 {
                    c.load_u64(9, 0)?;
                    c.context_access(9, 19, 224 + dst * 32, true, true)?;
                    c.context_access(9, 19, 232 + dst * 32, true, true)?;
                }
            } else {
                let load = i.op1_kind() == OpKind::Memory;
                let reg = vector_register(i.op_register(if load { 0 } else { 1 }), size)?;
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, reg as u64)?;
                c.load_u64(3, size)?;
                c.load_u64(4, u64::from(aligned))?;
                c.call_absolute(if load {
                    crate::guest_memory::ffi::native_ps5_module_vector_load_v1 as *const () as u64
                } else {
                    crate::guest_memory::ffi::native_ps5_module_vector_store_v1 as *const () as u64
                })?;
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
        if let Some(size) = vector_xor(i) {
            let dst = vector_register(i.op0_register(), size)?;
            let first = vector_register(i.op1_register(), size)?;
            if i.op2_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, dst as u64)?;
                c.load_u64(3, first as u64)?;
                c.load_u64(4, size)?;
                c.load_u64(5, 3)?; // XOR, full guarded operand before commit
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_vector_logical_v1 as *const ()
                        as u64,
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
            } else {
                let second = vector_register(i.op2_register(), size)?;
                // Each 64-bit piece is independent, including when dst aliases a source.
                for offset in (0..size as usize).step_by(8) {
                    c.context_access(9, 19, 208 + first * 32 + offset, true, false)?;
                    c.context_access(10, 19, 208 + second * 32 + offset, true, false)?;
                    c.emit(0xca0a0129)?; // eor x9,x9,x10
                    c.context_access(9, 19, 208 + dst * 32 + offset, true, true)?;
                }
                if size == 16 {
                    c.load_u64(9, 0)?;
                    c.context_access(9, 19, 224 + dst * 32, true, true)?;
                    c.context_access(9, 19, 232 + dst * 32, true, true)?;
                }
            }
            continue;
        }
        if movbe(i) {
            let store = i.op0_kind() == OpKind::Memory;
            let operand = i.op_register(if store { 1 } else { 0 });
            let size = operand.size();
            let reg = register(operand.full_register(), true)?;
            let reverse = match size {
                2 => 0x5ac00400,
                4 => 0x5ac00800,
                8 => 0xdac00c00,
                _ => unreachable!(),
            };
            memory_address(&mut c, i)?;
            c.emit(0xaa1303e0)?;
            if store {
                if size == 2 {
                    c.emit(0x79400262 | ((reg as u32 * 4) << 10))?;
                } else {
                    c.context_access(2, 19, reg * 8, size == 8, false)?;
                }
                c.emit(reverse | 0x42)?; // rev16/rev w2/x2,w2/x2
                c.load_u64(3, size as u64)?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_store_v1 as *const () as u64,
                )?;
            } else {
                c.load_u64(2, size as u64)?;
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
                c.emit(reverse)?; // rev16/rev w0/x0,w0/x0
                if size == 2 {
                    c.emit(0x79000260 | ((reg as u32 * 4) << 10))?;
                } else {
                    c.context_access(0, 19, reg * 8, true, true)?;
                }
            }
            continue;
        }
        if matches!(
            i.code(),
            Code::Mov_r8_rm8 | Code::Mov_rm8_r8 | Code::Mov_r16_rm16 | Code::Mov_rm16_r16
        ) {
            let store = i.op0_kind() == OpKind::Memory;
            let operand = if store {
                i.op1_register()
            } else {
                i.op0_register()
            };
            let byte = operand.size() == 1;
            let offset = |r: Register| -> Result<u32> {
                if byte {
                    byte_offset(r)
                } else {
                    Ok(register(r.full_register(), true)? as u32 * 4)
                }
            };
            let load_opcode = if byte { 0x39400260 } else { 0x79400260 };
            let store_opcode = if byte { 0x39000260 } else { 0x79000260 };
            if store || i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
                if store {
                    c.emit(load_opcode | 2 | (offset(operand)? << 10))?; // ldrb/ldrh w2
                    c.load_u64(3, if byte { 1 } else { 2 })?;
                    c.call_absolute(
                        crate::guest_memory::ffi::native_ps5_module_store_v1 as *const () as u64,
                    )?;
                } else {
                    c.load_u64(2, if byte { 1 } else { 2 })?;
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
                    c.emit(store_opcode | (offset(operand)? << 10))?;
                }
            } else {
                c.emit(load_opcode | (offset(i.op1_register())? << 10))?;
                c.emit(store_opcode | (offset(i.op0_register())? << 10))?;
            }
            continue;
        }
        if matches!(
            i.code(),
            Code::VEX_Bextr_r32_rm32_r32
                | Code::VEX_Bextr_r64_rm64_r64
                | Code::VEX_Andn_r32_r32_rm32
                | Code::VEX_Andn_r64_r64_rm64
                | Code::VEX_Rorx_r32_rm32_imm8
                | Code::VEX_Rorx_r64_rm64_imm8
                | Code::VEX_Mulx_r32_r32_rm32
                | Code::VEX_Mulx_r64_r64_rm64
        ) || bmi_shift(i).is_some()
        {
            let wide = i.op0_register().size() == 8;
            let dst = register(i.op0_register(), wide)?;
            let andn = i.mnemonic() == Mnemonic::Andn;
            let mulx = i.mnemonic() == Mnemonic::Mulx;
            let data_operand = if andn || mulx { 2 } else { 1 };
            let other_operand = if andn { 1 } else { 2 };
            if i.op_kind(data_operand) == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
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
            } else {
                c.context_access(
                    0,
                    19,
                    register(i.op_register(data_operand), wide)? * 8,
                    wide,
                    false,
                )?;
            }
            if mulx {
                c.context_access(1, 19, 16, wide, false)?; // implicit RDX/EDX input
                if wide {
                    c.emit(0x9b017c09)?; // mul x9,x0,x1: low product
                    c.emit(0x9bc17c0a)?; // umulh x10,x0,x1: high product
                } else {
                    c.emit(0x9ba17c09)?; // umull x9,w0,w1
                    c.emit(0xd360fd2a)?; // lsr x10,x9,#32
                    c.emit(0x2a0903e9)?; // mov w9,w9: zero-extend low half
                }
                c.context_access(9, 19, register(i.op1_register(), wide)? * 8, true, true)?;
                c.context_access(10, 19, dst * 8, true, true)?; // high half wins for equal destinations
                continue; // MULX preserves guest flags.
            }
            if i.mnemonic() == Mnemonic::Rorx {
                let count = u32::from(i.immediate8()) & if wide { 63 } else { 31 };
                c.emit((if wide { 0x93c00000 } else { 0x13800000 }) | (count << 10))?; // ror x0/w0,x0/w0,#count
                c.context_access(0, 19, dst * 8, true, true)?;
                continue; // RORX does not modify guest flags.
            }
            c.context_access(
                1,
                19,
                register(i.op_register(other_operand), wide)? * 8,
                wide,
                false,
            )?;
            if andn {
                c.emit(if wide { 0x8a210000 } else { 0x0a210000 })?; // bic x0/w0,x0/w0,x1/w1
                c.context_access(0, 19, dst * 8, true, true)?;
                c.context_access(9, 19, 136, true, false)?;
                c.load_u64(10, 0x8c1)?; // CF/OF clear, SF/ZF from result; retain undefined AF/PF
                c.emit(0x8a2a0129)?;
                c.emit(0xf100001f)?;
                c.emit(0x9a9f17ea)?;
                c.emit(0xaa0a1929)?; // ZF
                c.emit(if wide { 0xd37ffc0a } else { 0xd35f7c0a })?; // source-width sign bit
                c.emit(0xaa0a1d29)?; // orr x9,x9,x10,lsl #7
                c.context_access(9, 19, 136, true, true)?;
                continue;
            }
            if let Some(opcode) = bmi_shift(i) {
                // ARM W/X variable shifts mask counts to five/six bits, matching BMI.
                c.emit(opcode | if wide { 0x80000000 } else { 0 })?;
                c.context_access(0, 19, dst * 8, true, true)?;
                continue; // BMI shifts preserve guest flags, including for a zero count.
            }
            c.emit(0xd3401c29)?; // ubfx x9,x1,#0,#8: start
            c.emit(0xd3483c2a)?; // ubfx x10,x1,#8,#8: length
            c.load_u64(11, if wide { 64 } else { 32 })?;
            c.emit(0xcb09016c)?; // sub x12,x11,x9: remaining bits
            c.emit(0xeb0b013f)?; // cmp x9,x11
            c.emit(0x9a9f318c)?; // csel x12,x12,xzr,lo
            c.emit(0xeb0c015f)?; // cmp x10,x12
            c.emit(0x9a8c314a)?; // csel x10,x10,x12,lo: clamp length
            c.load_u64(11, 64)?;
            c.emit(0xcb0a016b)?; // sub x11,x11,x10
            c.load_u64(12, u64::MAX)?;
            c.emit(0x9acb258c)?; // lsrv x12,x12,x11
            c.emit(0xf100015f)?; // cmp x10,#0
            c.emit(0x9a9f118c)?; // csel x12,x12,xzr,ne: zero-length mask
            c.emit(0x9ac92400)?; // lsrv x0,x0,x9
            c.emit(0x8a0c0000)?; // and x0,x0,x12
            c.context_access(0, 19, dst * 8, true, true)?;
            c.context_access(9, 19, 136, true, false)?;
            c.load_u64(10, 0x841)?; // clear CF/OF/ZF; retain undefined AF/SF/PF
            c.emit(0x8a2a0129)?; // bic x9,x9,x10
            c.emit(0xf100001f)?; // cmp x0,#0
            c.emit(0x9a9f17ea)?; // cset x10,eq
            c.emit(0xaa0a1929)?; // orr x9,x9,x10,lsl #6
            c.context_access(9, 19, 136, true, true)?;
            continue;
        }
        if matches!(
            i.code(),
            Code::Not_rm8
                | Code::Not_rm16
                | Code::Not_rm32
                | Code::Not_rm64
                | Code::Neg_rm8
                | Code::Neg_rm16
                | Code::Neg_rm32
                | Code::Neg_rm64
        ) {
            let negate = i.mnemonic() == Mnemonic::Neg;
            if i.op0_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
                c.load_u64(2, 0)?;
                c.load_u64(3, i.memory_size().size() as u64)?;
                c.load_u64(4, if negate { 6 } else { 5 })?;
                c.call_absolute(
                    crate::guest_memory::ffi::native_ps5_module_integer_rmw_v1 as *const () as u64,
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
            } else {
                let operand = i.op0_register();
                let size = operand.size();
                let reg = register(operand.full_register(), true)?;
                if size == 1 {
                    c.emit(0x39400260 | (byte_offset(operand)? << 10))?;
                } else if size == 2 {
                    c.emit(0x79400260 | ((reg as u32 * 4) << 10))?;
                } else {
                    c.context_access(0, 19, reg * 8, size == 8, false)?;
                }
                if negate {
                    c.emit(0xaa0003e2)?; // mov x2,x0: retain original operand for flags
                    c.emit(if size == 8 { 0xcb0003e0 } else { 0x4b0003e0 })?; // neg x0/w0
                } else {
                    c.emit(if size == 8 { 0xaa2003e0 } else { 0x2a2003e0 })?; // mvn x0/w0
                }
                if size == 1 {
                    c.emit(0x39000260 | (byte_offset(operand)? << 10))?;
                } else if size == 2 {
                    c.emit(0x79000260 | ((reg as u32 * 4) << 10))?;
                } else {
                    c.context_access(0, 19, reg * 8, true, true)?;
                }
                if negate {
                    c.emit(0xaa0003e3)?; // mov x3,x0: result
                    c.context_access(0, 19, 136, true, false)?;
                    c.load_u64(1, 0)?;
                    c.load_u64(4, size as u64 * 8)?;
                    c.load_u64(5, 0)?; // flags for zero minus operand
                    c.call_absolute(
                        crate::arithmetic::native_ps5_arithmetic_flags_v1 as *const () as u64,
                    )?;
                    c.context_access(0, 19, 136, true, true)?;
                    c.context_access(1, 19, 152, true, true)?;
                }
            }
            continue;
        }
        if carry_arithmetic(i) {
            let size = i.op0_register().size();
            let dst = register(i.op0_register().full_register(), true)?;
            if i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
                c.load_u64(2, size as u64)?;
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
                c.emit(0xaa0003e2)?;
            } else if i.op1_kind() == OpKind::Register {
                let src = register(i.op1_register().full_register(), true)?;
                if size == 1 {
                    c.emit(0x39400262 | (byte_offset(i.op1_register())? << 10))?;
                } else if size == 2 {
                    c.emit(0x79400262 | ((src as u32 * 4) << 10))?;
                } else {
                    c.context_access(2, 19, src * 8, size == 8, false)?;
                }
            } else {
                c.load_u64(2, i.immediate(1))?;
            }
            if size == 1 {
                c.emit(0x39400261 | (byte_offset(i.op0_register())? << 10))?;
            } else if size == 2 {
                c.emit(0x79400261 | ((dst as u32 * 4) << 10))?;
            } else {
                c.context_access(1, 19, dst * 8, size == 8, false)?;
            }
            c.context_access(0, 19, 136, true, false)?;
            c.emit(0xd3400009)?; // ubfx x9,x0,#0,#1: incoming CF
            let subtract = i.mnemonic() == Mnemonic::Sbb;
            let width = if size == 8 { 0x80000000 } else { 0 };
            c.emit((if subtract { 0x4b020023 } else { 0x0b020023 }) | width)?;
            c.emit((if subtract { 0x4b090063 } else { 0x0b090063 }) | width)?;
            if size == 1 {
                c.emit(0x39000263 | (byte_offset(i.op0_register())? << 10))?;
            } else if size == 2 {
                c.emit(0x79000263 | ((dst as u32 * 4) << 10))?;
            } else {
                c.context_access(3, 19, dst * 8, true, true)?;
            }
            c.load_u64(4, size as u64 * 8)?;
            c.load_u64(5, if subtract { 4 } else { 7 })?;
            c.call_absolute(crate::arithmetic::native_ps5_arithmetic_flags_v1 as *const () as u64)?;
            c.context_access(0, 19, 136, true, true)?;
            c.context_access(1, 19, 152, true, true)?;
            continue;
        }
        if memory_arithmetic(i) {
            let size = i.memory_size().size();
            memory_address(&mut c, i)?;
            if i.op1_kind() == OpKind::Register {
                if size == 1 {
                    c.emit(0x39400262 | (byte_offset(i.op1_register())? << 10))?;
                } else {
                    let src = register(i.op1_register().full_register(), true)?;
                    if size == 2 {
                        c.emit(0x79400262 | ((src as u32 * 4) << 10))?;
                    } else {
                        c.context_access(2, 19, src * 8, size == 8, false)?;
                    }
                }
            } else {
                c.load_u64(2, i.immediate(1))?;
            }
            c.emit(0xaa1303e0)?;
            c.load_u64(3, size as u64)?;
            c.load_u64(
                4,
                match i.mnemonic() {
                    Mnemonic::Add => 0,
                    Mnemonic::Adc => 7,
                    Mnemonic::Sbb => 8,
                    Mnemonic::Sub => 1,
                    Mnemonic::And => 2,
                    Mnemonic::Or => 3,
                    Mnemonic::Xor => 4,
                    _ => unreachable!(),
                },
            )?;
            c.call_absolute(
                crate::guest_memory::ffi::native_ps5_module_integer_rmw_v1 as *const () as u64,
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
            continue;
        }
        if byte_arithmetic(i) {
            let destination = if i.op0_kind() == OpKind::Register {
                byte_offset(i.op0_register())?
            } else {
                0
            };
            if i.op0_kind() == OpKind::Memory || i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
                c.load_u64(2, 1)?;
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
                if i.op0_kind() == OpKind::Memory {
                    c.emit(0xaa0003e1)?; // mov x1,x0; memory is first operand
                    if i.op1_kind() == OpKind::Register {
                        c.emit(0x39400262 | (byte_offset(i.op1_register())? << 10))?;
                    } else {
                        c.load_u64(2, u64::from(i.immediate8()))?;
                    }
                } else {
                    c.emit(0xaa0003e2)?; // mov x2,x0; memory is second operand
                    c.emit(0x39400261 | (destination << 10))?;
                }
            } else {
                c.emit(0x39400261 | (destination << 10))?;
                if i.op1_kind() == OpKind::Register {
                    c.emit(0x39400262 | (byte_offset(i.op1_register())? << 10))?;
                } else {
                    c.load_u64(2, u64::from(i.immediate8()))?;
                }
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
        if conditional_move(i) {
            let size = i.op0_register().size();
            let dst = register(i.op0_register().full_register(), true)?;
            if i.op1_kind() == OpKind::Memory {
                // CMOV reads its source even when the condition is false.
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?;
                c.load_u64(2, size as u64)?;
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
            } else {
                let src = register(i.op1_register().full_register(), true)?;
                if size == 2 {
                    c.emit(0x79400260 | ((src as u32 * 4) << 10))?; // ldrh w0
                } else {
                    c.context_access(0, 19, src * 8, size == 8, false)?;
                }
            }
            // Preserve the source across the condition helper on an aligned host frame.
            c.emit(0xa9bf7fe0)?; // stp x0,xzr,[sp,#-16]!
            c.emit(0xaa1303e0)?;
            c.load_u64(1, i.condition_code() as u64 - 1)?;
            c.call_absolute(crate::condition::ffi::native_ps5_condition_v1 as *const () as u64)?;
            c.emit(0xf100001f)?; // cmp x0,#0
            c.emit(0xa8c17fe9)?; // ldp x9,xzr,[sp],#16
            c.context_access(10, 19, dst * 8, size == 8, false)?;
            c.emit(0x9a8a1129)?; // csel x9,x9,x10,ne
            if size == 2 {
                c.emit(0x79000269 | ((dst as u32 * 4) << 10))?; // strh w9
            } else {
                // CMOV32 clears the upper half even when the condition is false.
                c.context_access(9, 19, dst * 8, true, true)?;
            }
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
        if register_shift(i) {
            let wide = i.op0_register().size() == 8;
            let destination = register(i.op0_register(), wide)?;
            c.context_access(1, 19, destination * 8, wide, false)?;
            if i.op1_kind() == OpKind::Register {
                c.context_access(2, 19, 8, false, false)?; // ECX, captured before a possible ECX/RCX write
                c.emit(if wide { 0x53001442 } else { 0x53001042 })?; // UBFX w2,w2,#0,#6/#5
            } else {
                let count = u64::from(i.immediate8()) & if wide { 63 } else { 31 };
                c.load_u64(2, count)?;
            }
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
        if let Some((source_size, signed)) = extending_move(i) {
            if i.op1_kind() == OpKind::Memory {
                memory_address(&mut c, i)?;
                c.emit(0xaa1303e0)?; // mov x0,x19
                c.load_u64(2, source_size)?;
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
            } else if source_size == 1 {
                c.emit(0x39400260 | (byte_offset(i.op1_register())? << 10))?; // ldrb w0,[x19,#offset]
            } else if source_size == 2 {
                let r = i.op1_register() as u32;
                if !(Register::AX as u32..=Register::R15W as u32).contains(&r) {
                    return Err("Invalid direct word register".into());
                }
                c.emit(0x79400260 | (((r - Register::AX as u32) * 4) << 10))?; // ldrh w0,[x19,#reg*8]
            } else {
                c.context_access(0, 19, register(i.op1_register(), false)? * 8, false, false)?;
            }
            let destination_size = i.op0_register().size();
            if signed {
                // SBFM aliases SXTB/SXTH/SXTW, with W writes zeroing host upper bits.
                c.emit(
                    (if destination_size == 8 {
                        0x93400000
                    } else {
                        0x13000000
                    }) | (((source_size as u32 * 8) - 1) << 10),
                )?;
            }
            let dst = register(i.op0_register().full_register(), true)?;
            if destination_size == 2 {
                c.emit(0x79000260 | ((dst as u32 * 4) << 10))?; // strh w0
            } else {
                c.context_access(0, 19, dst * 8, true, true)?;
            }
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
            let size = if memory_first {
                i.memory_size().size()
            } else {
                i.op0_register().size()
            };
            let wide = if memory_first {
                size == 8
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
                c.load_u64(2, size as u64)?;
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
                    if i.op1_kind() == OpKind::Register {
                        let src = register(i.op1_register().full_register(), true)?;
                        if size == 2 {
                            c.emit(0x79400262 | ((src as u32 * 4) << 10))?;
                        } else {
                            c.context_access(2, 19, src * 8, wide, false)?;
                        }
                    } else {
                        c.load_u64(2, i.immediate(1))?;
                    }
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
            c.load_u64(4, size as u64 * 8)?;
            c.load_u64(5, operation)?;
            // Valid constant width/operation make this integer-only helper
            // infallible; operands and result were captured before alias writes.
            c.call_absolute(crate::arithmetic::native_ps5_arithmetic_flags_v1 as *const () as u64)?;
            c.context_access(0, 19, 136, true, true)?;
            c.context_access(1, 19, 152, true, true)?;
            continue;
        }
        match i.code() {
            Code::Nopw
            | Code::Nopd
            | Code::Nopq
            | Code::Nop_rm16
            | Code::Nop_rm32
            | Code::Nop_rm64 => {}
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
