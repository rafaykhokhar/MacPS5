//! Initial direct x86-to-ARM64 lowering using the shared Cpu dispatcher ABI.
use crate::{
    Result,
    arm64_code::{Arm64Code, Branch},
    decode,
};
use iced_x86::{Code, Instruction, Mnemonic, OpKind, Register};
fn register_arithmetic(i: &Instruction) -> bool {
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
            OpKind::Register => i.op0_register().size() == i.op1_register().size(),
            OpKind::Immediate32 | OpKind::Immediate8to32 => i.op0_register().size() == 4,
            OpKind::Immediate32to64 | OpKind::Immediate8to64 => i.op0_register().size() == 8,
            _ => false,
        }
}
fn memory_address(c: &mut Arm64Code, i: &Instruction) -> Result<()> {
    if matches!(i.memory_segment(), Register::FS | Register::GS) {
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
pub fn translate(bytes: &[u8], address: u64, instruction_limit: usize) -> Result<Vec<u32>> {
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
            Code::Nopd | Code::Nopq | Code::Retnq => {}
            Code::Mov_r32_imm32 | Code::Mov_r64_imm64 => {}
            Code::Mov_r32_rm32 | Code::Mov_rm32_r32 | Code::Mov_r64_rm64 | Code::Mov_rm64_r64 => {}
            _ if i.is_jcc_short_or_near() || i.is_jmp_short_or_near() => {}
            _ if register_arithmetic(&i) => {}
            _ => return Err(format!("Direct lowering unsupported: {:?}", i.code())),
        }
        offset += i.len();
        if i.code() == Code::Retnq && offset != bytes.len() {
            return Err("Direct block bytes continue after RET".into());
        }
        decoded.push(i);
    }
    if decoded.last().is_none_or(|i| {
        i.code() != Code::Retnq && !i.is_jmp_short_or_near() && !i.is_jcc_short_or_near()
    }) {
        return Err("Direct block must end in RET64 or a supported branch".into());
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
    if decoded.last().unwrap().is_jcc_short_or_near() {
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
        .checked_mul(64)
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
        if register_arithmetic(i) {
            let wide = i.op0_register().size() == 8;
            let destination = register(i.op0_register(), wide)?;
            c.context_access(1, 19, destination * 8, wide, false)?;
            if i.op1_kind() == OpKind::Register {
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
            let (opcode, operation) = match i.mnemonic() {
                Mnemonic::Add => (0x0b020023, 1),
                Mnemonic::Sub | Mnemonic::Cmp => (0x4b020023, 0),
                Mnemonic::And | Mnemonic::Test => (0x0a020023, 2),
                Mnemonic::Or => (0x2a020023, 2),
                Mnemonic::Xor => (0x4a020023, 2),
                _ => unreachable!(),
            };
            c.emit(opcode | if wide { 0x80000000 } else { 0 })?;
            if !matches!(i.mnemonic(), Mnemonic::Cmp | Mnemonic::Test) {
                c.context_access(3, 19, destination * 8, true, true)?;
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
            Code::Nopd | Code::Nopq => {}
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
    if decoded.last().unwrap().is_jcc_short_or_near() {
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
