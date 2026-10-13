//! Initial direct x86-to-ARM64 lowering using the shared Cpu dispatcher ABI.
use crate::{
    Result,
    arm64_code::{Arm64Code, Branch},
    decode,
};
use iced_x86::{Code, OpKind, Register};
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
            Code::Mov_r32_rm32 | Code::Mov_rm32_r32 | Code::Mov_r64_rm64 | Code::Mov_rm64_r64
                if i.op0_kind() == OpKind::Register && i.op1_kind() == OpKind::Register => {}
            _ => return Err(format!("Direct lowering unsupported: {:?}", i.code())),
        }
        offset += i.len();
        if i.code() == Code::Retnq && offset != bytes.len() {
            return Err("Direct block bytes continue after RET".into());
        }
        decoded.push(i);
    }
    if decoded.last().is_none_or(|i| i.code() != Code::Retnq) {
        return Err("Direct block must end in RET64".into());
    }
    let limit = decoded
        .len()
        .checked_mul(48)
        .and_then(|n| n.checked_add(32))
        .ok_or("Direct emission budget overflow")?;
    let mut c = Arm64Code::new(limit);
    const EXIT: u32 = u32::MAX;
    const FUEL: u32 = u32::MAX - 1;
    if decoded.len() >= FUEL as usize {
        return Err("Too many direct labels".into());
    }
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
    c.bind(FUEL)?;
    c.load_u64(9, 4)?;
    c.context_access(9, 19, 168, true, true)?;
    c.bind(EXIT)?;
    c.emit(0xa8c17bf3)?;
    c.emit(0xd65f03c0)?;
    c.finish()
}
