use crate::Result;
use iced_x86::{Decoder, DecoderOptions, Instruction, OpKind};
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Debug, Serialize)]
pub struct Decoded {
    pub address: u64,
    pub end: u64,
    pub code: String,
    pub mnemonic: String,
    pub flow: String,
    pub operands: Vec<Value>,
}

pub fn instruction(bytes: &[u8], address: u64) -> Result<Instruction> {
    let mut decoder = Decoder::with_ip(64, bytes, address, DecoderOptions::NONE);
    let instruction = decoder.decode();
    if instruction.is_invalid() {
        return Err(format!(
            "Invalid or truncated x86 instruction at {address:#x}"
        ));
    }
    address
        .checked_add(instruction.len() as u64)
        .ok_or("Instruction address overflow")?;
    Ok(instruction)
}

pub fn describe(i: &Instruction) -> Result<Decoded> {
    let mut operands = Vec::new();
    for index in 0..i.op_count() {
        let operand = match i.op_kind(index) {
            OpKind::Register => {
                json!({"kind":"register","name":format!("{:?}",i.op_register(index)),"bytes":i.op_register(index).size()})
            }
            OpKind::Memory => json!({"kind":"memory","base":format!("{:?}",i.memory_base()),
                "index":format!("{:?}",i.memory_index()),"scale":i.memory_index_scale(),
                "displacement":i.memory_displacement64(),"ip_relative":i.is_ip_rel_memory_operand(),
                "segment":format!("{:?}",i.memory_segment()),"bytes":i.memory_size().size()}),
            OpKind::NearBranch16 | OpKind::NearBranch32 | OpKind::NearBranch64 => {
                json!({"kind":"branch","target":i.near_branch_target()})
            }
            OpKind::Immediate8
            | OpKind::Immediate8_2nd
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => {
                json!({"kind":"immediate","encoding":format!("{:?}",i.op_kind(index)),"value":i.immediate(index)})
            }
            other => return Err(format!("Operand description not implemented for {other:?}")),
        };
        operands.push(operand);
    }
    Ok(Decoded {
        address: i.ip(),
        end: i.next_ip(),
        code: format!("{:?}", i.code()),
        mnemonic: format!("{:?}", i.mnemonic()),
        flow: format!("{:?}", i.flow_control()),
        operands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_actual_mulx_and_byte_compare() {
        let i = instruction(&[0xc4, 0xe2, 0xfb, 0xf6, 0xc0], 0x38fe).unwrap();
        assert_eq!(i.next_ip(), 0x3903);
        assert_eq!(format!("{:?}", i.mnemonic()), "Mulx");
        let d = describe(&instruction(&[0x44, 0x3a, 0x04, 0x3e], 0x3965).unwrap()).unwrap();
        assert_eq!(d.operands[0]["name"], "R8L");
        assert_eq!(d.operands[1]["base"], "RSI");
        assert_eq!(d.operands[1]["index"], "RDI");
    }
    #[test]
    fn truncated_and_overflow_fail() {
        assert!(instruction(&[0x48, 0xb8, 1], 0).is_err());
        assert!(instruction(&[0xc3], u64::MAX).is_err());
    }
    #[test]
    fn distinguishes_high_byte_and_rex_low_byte() {
        let a = describe(&instruction(&[0x38, 0xe0], 0).unwrap()).unwrap();
        let b = describe(&instruction(&[0x40, 0x38, 0xe0], 0).unwrap()).unwrap();
        assert_eq!(a.operands[1]["name"], "AH");
        assert_eq!(b.operands[1]["name"], "SPL");
    }
}
