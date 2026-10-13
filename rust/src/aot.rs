//! Native ARM64 emission. Guest accesses continue through the guarded runtime ABI.
use crate::{Result, decode};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register as R};
use std::collections::BTreeSet;

/// Emit for the Rust module memory ABI. Remaining legacy helpers are explicit
/// unsupported boundaries; this entry point never mixes memory-context layouts.
pub fn translate_rust_memory(bytes: &[u8], address: u64, symbol: &str) -> Result<String> {
    rust_memory_helpers(translate(bytes, address, symbol, false, false)?.0)
}
/// Emit a resumable block whose direct edges may leave its byte range.
pub fn translate_rust_memory_region(bytes: &[u8], address: u64, symbol: &str) -> Result<String> {
    rust_memory_helpers(translate(bytes, address, symbol, true, true)?.0)
}
pub(crate) fn rust_memory_helpers(assembly: String) -> Result<String> {
    let assembly = assembly
        .replace("bl _guest_load\n", "bl _native_ps5_module_load_v1\n")
        .replace("bl _guest_store\n", "bl _native_ps5_module_store_v1\n")
        .replace(
            "bl _guest_vector_load\n",
            "bl _native_ps5_module_vector_load_v1\n",
        )
        .replace(
            "bl _guest_vector_store\n",
            "bl _native_ps5_module_vector_store_v1\n",
        )
        .replace(
            "bl _guest_vector_xor\n",
            "bl _native_ps5_module_vector_xor_v1\n",
        );
    if assembly.contains("bl _guest_") {
        return Err("Instruction requires an unmigrated runtime helper".into());
    }
    Ok(assembly)
}

#[cfg(test)]
mod rust_memory_tests {
    #[test]
    fn packed_compare_shift_exact_forms() {
        for bytes in [
            &[0xc5, 0xfd, 0x66, 0xc1][..],
            &[0xc5, 0xfd, 0x66, 0x07],
            &[0xc5, 0xfd, 0x72, 0xd0, 0x1f],
        ] {
            assert!(super::translate_rust_memory_region(bytes, 0, "packed").is_ok());
        }
        for bytes in [
            &[0xc5, 0xf9, 0x66, 0xc1][..],
            &[0xc5, 0xf9, 0x72, 0xd0, 0x1f],
            &[0xc5, 0xfd, 0xd2, 0xc1],
            &[0x66, 0x0f, 0x66, 0xc1],
            &[0x67, 0xc5, 0xfd, 0x66, 0x07],
        ] {
            assert!(super::translate_rust_memory_region(bytes, 0, "unsupported_packed").is_err());
        }
    }
    #[test]
    fn vpbroadcastd_exact_forms() {
        for bytes in [
            &[0xc4, 0xe2, 0x79, 0x58, 0xc0][..],
            &[0xc4, 0xe2, 0x7d, 0x58, 0x07],
        ] {
            assert!(super::translate_rust_memory_region(bytes, 0, "broadcast").is_ok());
        }
        for bytes in [
            &[0xc4, 0xe2, 0x79, 0x59, 0xc0][..],
            &[0xc4, 0xe2, 0x79, 0x78, 0xc0],
            &[0xc4, 0xe2, 0x79, 0x79, 0xc0],
            &[0x62, 0xf2, 0x7d, 0x08, 0x58, 0xc0],
            &[0x67, 0xc4, 0xe2, 0x79, 0x58, 0x00],
            &[0x64, 0xc4, 0xe2, 0x79, 0x58, 0x00],
        ] {
            assert!(
                super::translate_rust_memory_region(bytes, 0, "unsupported_broadcast").is_err()
            );
        }
    }
    #[test]
    fn lzcnt32_observed_encoding_and_guarded_variants() {
        for bytes in [
            &[0xf3, 0x0f, 0xbd, 0xf2][..],
            &[0xf3, 0x0f, 0xbd, 0xc0],
            &[0xf3, 0x45, 0x0f, 0xbd, 0xf7],
            &[0xf3, 0x0f, 0xbd, 0x36],
        ] {
            assert!(super::translate_rust_memory_region(bytes, 0x1b58c3, "lzcnt").is_ok());
        }
        for bytes in [
            &[0xf3, 0x66, 0x0f, 0xbd, 0xc0][..],
            &[0xf3, 0x48, 0x0f, 0xbd, 0xc0],
            &[0x0f, 0xbd, 0xc0],
            &[0xf0, 0xf3, 0x0f, 0xbd, 0x00],
            &[0x67, 0xf3, 0x0f, 0xbd, 0x00],
            &[0x64, 0xf3, 0x0f, 0xbd, 0x00],
        ] {
            assert!(
                super::translate_rust_memory_region(bytes, 0, "unsupported_lzcnt").is_err(),
                "{bytes:x?}"
            );
        }
    }
    #[test]
    fn memory_contexts_cannot_fall_back_to_legacy_helpers() {
        let assembly =
            super::translate_rust_memory(&[0x50, 0x58, 0xc3], 0x100, "rust_stack").unwrap();
        assert!(assembly.contains("bl _native_ps5_module_load_v1"));
        assert!(assembly.contains("bl _native_ps5_module_store_v1"));
        assert!(!assembly.contains("bl _guest_"));
        let assembly =
            super::translate_rust_memory(&[0xc5, 0xf8, 0x10, 0x07, 0xc3], 0x100, "vector").unwrap();
        assert!(assembly.contains("bl _native_ps5_module_vector_load_v1"));
        assert!(super::rust_memory_helpers("bl _guest_unknown\n".into()).is_err());
    }
}

fn gpr(r: R) -> Result<(usize, usize)> {
    let high = matches!(r, R::AH | R::CH | R::DH | R::BH);
    let full = r.full_register();
    if !(R::RAX..=R::R15).contains(&full) {
        return Err(format!("Unsupported register {r:?}"));
    }
    Ok((
        (full as usize - R::RAX as usize) * 8 + usize::from(high),
        r.size() * 8,
    ))
}
fn vector(r: R) -> Result<usize> {
    let full = r.full_register();
    if !(R::ZMM0..=R::ZMM15).contains(&full) {
        return Err(format!("Unsupported vector {r:?}"));
    }
    Ok(full as usize - R::ZMM0 as usize)
}
fn bits(i: &Instruction, n: u32) -> usize {
    if i.op_kind(n) == OpKind::Register {
        i.op_register(n).size() * 8
    } else {
        i.memory_size().size() * 8
    }
}
fn condition(i: &Instruction) -> Result<u32> {
    let c = i.condition_code() as u32;
    c.checked_sub(1)
        .ok_or_else(|| "Missing condition code".into())
}
pub fn terminal(i: &Instruction) -> bool {
    i.is_jcc_short_or_near()
        || i.is_jmp_short_or_near()
        || i.is_jmp_near_indirect()
        || i.mnemonic() == M::Call
        || i.mnemonic() == M::Ret
}
pub fn successors(i: &Instruction) -> Vec<u64> {
    if i.mnemonic() == M::Ret || i.is_jmp_near_indirect() {
        vec![]
    } else if i.is_jmp_short_or_near() {
        vec![i.near_branch_target()]
    } else if i.is_jcc_short_or_near() || i.is_call_near() {
        vec![i.next_ip(), i.near_branch_target()]
    } else {
        vec![i.next_ip()]
    }
}

struct Emitter {
    lines: Vec<String>,
}
impl Emitter {
    fn emit(&mut self, text: impl AsRef<str>) {
        self.lines.extend(text.as_ref().lines().map(str::to_owned));
    }
    fn immediate(&mut self, r: &str, v: u64) {
        self.emit(format!("movz {r}, #{}", v & 65535));
        for shift in [16, 32, 48] {
            if v >> shift & 65535 != 0 {
                self.emit(format!("movk {r}, #{}, lsl #{shift}", v >> shift & 65535));
            }
        }
    }
    fn fault(&mut self) {
        self.emit("ldr x9, [x19, #152]\ncbnz x9, .Lexit");
    }
    fn address(&mut self, i: &Instruction) -> Result<()> {
        if matches!(i.memory_segment(), R::FS | R::GS) {
            return Err("FS/GS addressing is not implemented".into());
        }
        if i.is_ip_rel_memory_operand() {
            if i.memory_base() != R::RIP {
                return Err("32-bit relative addressing is not implemented".into());
            }
            self.emit("ldr x1, [x19, #144]");
            self.immediate("x9", i.ip_rel_memory_address());
            self.emit("add x1, x1, x9");
            return Ok(());
        }
        let base = i.memory_base();
        let index = i.memory_index();
        if base == R::None {
            self.emit("mov x1, #0");
        } else {
            let (off, width) = gpr(base)?;
            if width != 64 {
                return Err("32-bit addressing is not implemented".into());
            }
            self.emit(format!("ldr x1, [x19, #{off}]"));
        }
        if index != R::None {
            let (off, width) = gpr(index)?;
            if width != 64 {
                return Err("32-bit indexing is not implemented".into());
            }
            self.emit(format!(
                "ldr x9, [x19, #{off}]\nadd x1, x1, x9, lsl #{}",
                i.memory_index_scale().trailing_zeros()
            ));
        }
        if i.memory_displacement64() != 0 {
            self.immediate("x9", i.memory_displacement64());
            self.emit("add x1, x1, x9");
        }
        Ok(())
    }
    fn read(&mut self, i: &Instruction, n: u32, width: usize, r: u32) -> Result<()> {
        match i.op_kind(n) {
            OpKind::Register => {
                let (off, actual) = gpr(i.op_register(n))?;
                if actual != width {
                    return Err(format!("Register width {actual} differs from {width}"));
                }
                let (op, p) = match width {
                    8 => ("ldrb", "w"),
                    16 => ("ldrh", "w"),
                    32 => ("ldr", "w"),
                    64 => ("ldr", "x"),
                    _ => return Err("Invalid scalar width".into()),
                };
                self.emit(format!("{op} {p}{r}, [x19, #{off}]"));
            }
            OpKind::Memory => {
                self.address(i)?;
                self.emit(format!(
                    "mov x0, x19\nmov x2, #{}\nbl _guest_load",
                    width / 8
                ));
                self.fault();
                self.emit(format!("mov x{r}, x0"));
            }
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate64
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
            | OpKind::Immediate8to64
            | OpKind::Immediate32to64 => self.immediate(&format!("x{r}"), i.immediate(n)),
            _ => return Err("Unsupported scalar operand".into()),
        }
        Ok(())
    }
    fn write(&mut self, i: &Instruction, n: u32, width: usize, r: u32) -> Result<()> {
        match i.op_kind(n) {
            OpKind::Register => {
                let (off, actual) = gpr(i.op_register(n))?;
                if actual != width {
                    return Err("Destination width mismatch".into());
                }
                match width {
                    8 | 16 => self.emit(format!(
                        "{} w{r}, [x19, #{off}]",
                        if width == 8 { "strb" } else { "strh" }
                    )),
                    32 | 64 => {
                        if width == 32 {
                            self.emit(format!("mov w{r}, w{r}"));
                        }
                        self.emit(format!("str x{r}, [x19, #{off}]"));
                    }
                    _ => return Err("Invalid scalar destination".into()),
                }
            }
            OpKind::Memory => {
                self.address(i)?;
                self.emit(format!(
                    "mov x0, x19\nmov x2, x{r}\nmov x3, #{}\nbl _guest_store",
                    width / 8
                ));
                self.fault();
            }
            _ => return Err("Invalid destination".into()),
        }
        Ok(())
    }
    fn stop(&mut self, kind: u32, target: u64) {
        self.immediate("x9", kind as u64);
        self.emit("str x9, [x19, #168]");
        self.immediate("x9", target);
        if kind == 1 {
            self.emit("ldr x10, [x19, #144]\nadd x9, x9, x10");
        }
        self.emit("str x9, [x19, #176]\nb .Lexit");
    }
    fn instruction(&mut self, i: &Instruction) -> Result<()> {
        let locked_xadd32 = i.has_lock_prefix()
            && i.mnemonic() == M::Xadd
            && i.op0_kind() == OpKind::Memory
            && i.op1_kind() == OpKind::Register
            && bits(i, 0) == 32
            && i.op1_register().size() == 4;
        let lzcnt32 = i.code() == iced_x86::Code::Lzcnt_r32_rm32;
        if (i.has_lock_prefix() && !locked_xadd32)
            || (i.has_rep_prefix() && !lzcnt32)
            || i.has_repne_prefix()
        {
            return Err("LOCK/REP prefixes are not implemented".into());
        }
        if locked_xadd32 {
            let (offset, width) = gpr(i.op1_register())?;
            if width != 32 || offset % 8 != 0 {
                return Err("Invalid LOCK XADD32 source".into());
            }
            self.address(i)?;
            self.emit(format!(
                "mov x0, x19\nmov x2, #{}\nbl _native_ps5_module_atomic_xadd32_v1",
                offset / 8
            ));
            self.fault();
            return Ok(());
        }
        let m = i.mnemonic();
        let width = bits(i, 0);
        let p = if width == 32 { "w" } else { "x" };
        if i.is_jcc_short_or_near() {
            self.emit(format!(
                "mov x0, x19\nmov x1, #{}\nbl _native_ps5_condition_v1",
                condition(i)?
            ));
            self.fault();
            self.emit(format!("cbnz x0, .Lpc_{:x}", i.near_branch_target()));
            return Ok(());
        }
        if i.is_jmp_short_or_near() {
            self.emit(format!("b .Lpc_{:x}", i.near_branch_target()));
            return Ok(());
        }
        if matches!(
            m,
            M::Cmovo
                | M::Cmovno
                | M::Cmovb
                | M::Cmovae
                | M::Cmove
                | M::Cmovne
                | M::Cmovbe
                | M::Cmova
                | M::Cmovs
                | M::Cmovns
                | M::Cmovp
                | M::Cmovnp
                | M::Cmovl
                | M::Cmovge
                | M::Cmovle
                | M::Cmovg
        ) {
            if ![32, 64].contains(&width) {
                return Err("CMOV width is not implemented".into());
            }
            self.read(i, 1, width, 20)?;
            self.read(i, 0, width, 21)?;
            self.emit(format!(
                "mov x0, x19\nmov x1, #{}\nbl _native_ps5_condition_v1",
                condition(i)?
            ));
            self.fault();
            self.emit("cmp x0, #0\ncsel x20, x20, x21, ne");
            self.write(i, 0, width, 20)?;
            return Ok(());
        }
        if matches!(
            m,
            M::Seto
                | M::Setno
                | M::Setb
                | M::Setae
                | M::Sete
                | M::Setne
                | M::Setbe
                | M::Seta
                | M::Sets
                | M::Setns
                | M::Setp
                | M::Setnp
                | M::Setl
                | M::Setge
                | M::Setle
                | M::Setg
        ) {
            self.emit(format!(
                "mov x0, x19\nmov x1, #{}\nbl _native_ps5_condition_v1",
                condition(i)?
            ));
            self.fault();
            self.emit("mov x20, x0");
            self.write(i, 0, 8, 20)?;
            return Ok(());
        }
        match m {
            M::Lzcnt => {
                if !lzcnt32 {
                    return Err("Only LZCNT r32,r/m32 is implemented".into());
                }
                // AMD APM Vol3 LZCNT: CF=input zero; ZF=count zero. Other
                // arithmetic flags are undefined; preserve them deterministically.
                // Read/fault before either architectural destination is changed.
                self.read(i, 1, 32, 20)?;
                self.emit("clz w21, w20\ncmp w20, #0\ncset x10, eq\ncmp w21, #0\ncset x11, eq\nlsl x11, x11, #6\nldr x9, [x19, #136]\nmov x12, #65\nbic x9, x9, x12\norr x9, x9, x10\norr x9, x9, x11");
                self.write(i, 0, 32, 21)?;
                self.emit("str x9, [x19, #136]");
            }
            M::Fstp => {
                if i.code() != iced_x86::Code::Fstp_m80fp {
                    return Err("Only FSTP m80fp is implemented".into());
                }
                self.address(i)?;
                self.emit("mov x0, x19\nbl _native_ps5_x87_store80_v1");
                self.fault();
            }
            M::Fldcw => {
                self.emit("mov x0, x19\nbl _native_ps5_x87_wait_v1");
                self.fault();
                self.read(i, 0, 16, 20)?;
                self.emit("mov x0, x19\nmov x1, x20\nbl _native_ps5_x87_load_control_v1");
                self.fault();
            }
            M::Fnstcw => {
                self.emit("ldrh w20, [x19, #204]");
                self.write(i, 0, 16, 20)?;
            }
            M::Wait => {
                self.emit("mov x0, x19\nbl _native_ps5_x87_wait_v1");
                self.fault();
            }
            M::Ldmxcsr | M::Vldmxcsr => {
                self.read(i, 0, 32, 20)?;
                self.emit("mov x0, x19\nmov x1, x20\nbl _native_ps5_load_mxcsr_v1");
                self.fault();
            }
            M::Stmxcsr | M::Vstmxcsr => {
                self.emit("ldr w20, [x19, #200]");
                self.write(i, 0, 32, 20)?;
            }
            M::Vfmadd213sd => {
                if i.code() != iced_x86::Code::VEX_Vfmadd213sd_xmm_xmm_xmmm64 {
                    return Err("Only VEX VFMADD213SD is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                let first = vector(i.op_register(1))?;
                if i.op_kind(2) == OpKind::Register {
                    let second = vector(i.op_register(2))?;
                    self.emit(format!("ldr x20, [x19, #{}]", 208 + second * 32));
                } else {
                    self.read(i, 2, 64, 20)?;
                }
                // Intel VFMADD213SD: (old destination * source1) + source2,
                // with one rounding. Bits127:64 preserve the OLD destination.
                // Snapshot every aliased source before any destination commit.
                self.emit(format!(
                    "ldr x21, [x19, #{}]\nldr x1, [x19, #{}]\nldr x2, [x19, #{}]\nmov x3, x20\nmov x0, x19\nbl _native_ps5_fma213_binary64_v1",
                    216 + dst * 32, 208 + dst * 32, 208 + first * 32
                ));
                self.fault();
                self.emit(format!(
                    "str x0, [x19, #{}]\nstr x21, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                    208 + dst * 32, 216 + dst * 32, 224 + dst * 32, 232 + dst * 32
                ));
            }
            M::Vmulsd => {
                if i.code() != iced_x86::Code::VEX_Vmulsd_xmm_xmm_xmmm64 {
                    return Err("Only VEX scalar binary64 VMULSD is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                let first = vector(i.op_register(1))?;
                if i.op_kind(2) == OpKind::Register {
                    let second = vector(i.op_register(2))?;
                    self.emit(format!("ldr x20, [x19, #{}]", 208 + second * 32));
                } else {
                    self.read(i, 2, 64, 20)?;
                }
                // Snapshot both source operands and the source1 merge lane
                // before any destination write, including fully aliased forms.
                self.emit(format!(
                    "ldr x21, [x19, #{}]\nldr x1, [x19, #{}]\nmov x2, x20\nmov x0, x19\nbl _native_ps5_multiply_binary64_v1",
                    216 + first * 32, 208 + first * 32
                ));
                self.fault();
                self.emit(format!(
                    "str x0, [x19, #{}]\nstr x21, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                    208 + dst * 32, 216 + dst * 32, 224 + dst * 32, 232 + dst * 32
                ));
            }
            M::Vcvttsd2si => {
                if i.code() != iced_x86::Code::VEX_Vcvttsd2si_r32_xmmm64 {
                    return Err("Only signed32 VEX VCVTTSD2SI is implemented".into());
                }
                if i.op_kind(1) == OpKind::Register {
                    let source = vector(i.op_register(1))?;
                    self.emit(format!("ldr x20, [x19, #{}]", 208 + source * 32));
                } else {
                    self.read(i, 1, 64, 20)?;
                }
                self.emit("mov x0, x19\nmov x1, x20\nbl _native_ps5_truncate_i32_v1");
                self.fault();
                self.emit("mov w20, w0");
                self.write(i, 0, 32, 20)?;
            }
            M::Vcvtsi2sd => {
                let source_width = match i.code() {
                    iced_x86::Code::VEX_Vcvtsi2sd_xmm_xmm_rm32 => 32,
                    iced_x86::Code::VEX_Vcvtsi2sd_xmm_xmm_rm64 => 64,
                    _ => return Err("Only signed32/64 VEX VCVTSI2SD is implemented".into()),
                };
                let dst = vector(i.op_register(0))?;
                let merge = vector(i.op_register(1))?;
                self.read(i, 2, source_width, 20)?;
                if source_width == 64 {
                    self.emit(format!(
                        "ldr x21, [x19, #{}]\nmov x0, x19\nmov x1, x20\nbl _native_ps5_convert_i64_binary64_v1",
                        216 + merge * 32
                    ));
                    self.fault();
                    self.emit(format!(
                        "str x0, [x19, #{}]\nstr x21, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                        208 + dst * 32, 216 + dst * 32, 224 + dst * 32, 232 + dst * 32
                    ));
                    return Ok(());
                }
                // Every signed32 integer fits exactly in binary64, independent
                // of rounding mode. Snapshot merge before any aliased write.
                self.emit(format!(
                    "ldr x21, [x19, #{}]\nscvtf d0, w20\nstr d0, [x19, #{}]\nstr x21, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                    216 + merge * 32, 208 + dst * 32, 216 + dst * 32,
                    224 + dst * 32, 232 + dst * 32
                ));
            }
            M::Vpcmpgtd => {
                if i.code() != iced_x86::Code::VEX_Vpcmpgtd_ymm_ymm_ymmm256 {
                    return Err("Only VEX256 VPCMPGTD is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                let first = vector(i.op_register(1))?;
                if i.op_kind(2) == OpKind::Memory {
                    self.address(i)?;
                    self.emit(format!("mov x0, x19\nmov x2, #{dst}\nmov x3, #{first}\nbl _native_ps5_module_vector_cmpgtd256_v1"));
                    self.fault();
                } else {
                    let second = vector(i.op_register(2))?;
                    self.emit(format!("ldr q0, [x19, #{}]\nldr q1, [x19, #{}]\nldr q2, [x19, #{}]\nldr q3, [x19, #{}]\ncmgt v0.4s, v0.4s, v1.4s\ncmgt v2.4s, v2.4s, v3.4s\nstr q0, [x19, #{}]\nstr q2, [x19, #{}]",208+first*32,208+second*32,224+first*32,224+second*32,208+dst*32,224+dst*32));
                }
            }
            M::Vpsrld => {
                if i.code() != iced_x86::Code::VEX_Vpsrld_ymm_ymm_imm8 {
                    return Err("Only VEX256 VPSRLD immediate is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                let src = vector(i.op_register(1))?;
                let count = i.immediate8();
                if count > 31 {
                    self.emit("movi v0.16b, #0\nmovi v1.16b, #0");
                } else {
                    self.emit(format!(
                        "ldr q0, [x19, #{}]\nldr q1, [x19, #{}]",
                        208 + src * 32,
                        224 + src * 32
                    ));
                    if count != 0 {
                        self.emit(format!(
                            "ushr v0.4s, v0.4s, #{count}\nushr v1.4s, v1.4s, #{count}"
                        ));
                    }
                }
                self.emit(format!(
                    "str q0, [x19, #{}]\nstr q1, [x19, #{}]",
                    208 + dst * 32,
                    224 + dst * 32
                ));
            }
            M::Vpbroadcastd => {
                let wide = match i.code() {
                    iced_x86::Code::VEX_Vpbroadcastd_xmm_xmmm32 => false,
                    iced_x86::Code::VEX_Vpbroadcastd_ymm_xmmm32 => true,
                    _ => {
                        return Err(
                            "Only VEX VPBROADCASTD XMM/YMM from XMM/m32 is implemented".into()
                        );
                    }
                };
                let dst = 208 + vector(i.op_register(0))? * 32;
                if i.op_kind(1) == OpKind::Register {
                    let src = 208 + vector(i.op_register(1))? * 32;
                    self.emit(format!("ldr w20, [x19, #{src}]"));
                } else {
                    self.read(i, 1, 32, 20)?;
                }
                // AMD APM Vol4 VPBROADCASTD: snapshot low32 before aliases,
                // replicate integer bits without FP effects; read/fault before commit.
                self.emit(format!("mov w20, w20\norr x20, x20, x20, lsl #32\nstr x20, [x19, #{dst}]\nstr x20, [x19, #{}]", dst + 8));
                let upper = if wide { "x20" } else { "xzr" };
                self.emit(format!(
                    "str {upper}, [x19, #{}]\nstr {upper}, [x19, #{}]",
                    dst + 16,
                    dst + 24
                ));
            }
            M::Vmovd => {
                if i.code() != iced_x86::Code::VEX_Vmovd_xmm_rm32 {
                    return Err("Only VEX VMOVD r/m32-to-XMM is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                self.read(i, 1, 32, 20)?;
                // Integer bit-copy; VEX clears all modeled bits above bit31.
                // Commit only after the complete guarded four-byte read.
                self.emit(format!(
                    "mov w20, w20\nstr x20, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                    208 + dst * 32, 216 + dst * 32, 224 + dst * 32, 232 + dst * 32
                ));
            }
            M::Vmovsd => {
                if i.code() != iced_x86::Code::VEX_Vmovsd_xmm_m64 {
                    return Err("Only VEX VMOVSD m64-to-XMM is implemented".into());
                }
                let dst = vector(i.op_register(0))?;
                self.read(i, 1, 64, 20)?;
                // Memory form clears bits255:64. Commit after guarded load;
                // integer bit-copy preserves NaN payloads without FP exceptions.
                self.emit(format!(
                    "str x20, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]\nstr xzr, [x19, #{}]",
                    208 + dst * 32, 216 + dst * 32, 224 + dst * 32, 232 + dst * 32
                ));
            }
            M::Movdqu => {
                if !matches!(
                    i.code(),
                    iced_x86::Code::Movdqu_xmm_xmmm128 | iced_x86::Code::Movdqu_xmmm128_xmm
                ) {
                    return Err("Only legacy128 MOVDQU forms are implemented".into());
                }
                if i.op_kind(0) == OpKind::Register && i.op_kind(1) == OpKind::Register {
                    let dst = vector(i.op_register(0))?;
                    let src = vector(i.op_register(1))?;
                    self.emit(format!(
                        "ldr q0, [x19, #{}]\nstr q0, [x19, #{}]",
                        208 + src * 32,
                        208 + dst * 32
                    ));
                } else {
                    let load = i.op_kind(1) == OpKind::Memory;
                    let reg = vector(i.op_register(if load { 0 } else { 1 }))?;
                    self.address(i)?;
                    // The shared guarded load has VEX zero-upper semantics.
                    // Preserve legacy YMM[255:128] in callee-saved GPRs and
                    // restore it on both success and fault. The helper checks
                    // the entire unaligned operand before changing any lane.
                    if load {
                        self.emit(format!(
                            "ldr x21, [x19, #{}]\nldr x22, [x19, #{}]",
                            224 + reg * 32,
                            232 + reg * 32
                        ));
                    }
                    self.emit(format!(
                        "mov x0, x19\nmov x2, #{reg}\nmov x3, #16\nmov x4, #0\nbl _guest_vector_{}",
                        if load { "load" } else { "store" }
                    ));
                    if load {
                        self.emit(format!(
                            "str x21, [x19, #{}]\nstr x22, [x19, #{}]",
                            224 + reg * 32,
                            232 + reg * 32
                        ));
                    }
                    self.fault();
                }
            }
            M::Vmovaps
            | M::Vmovups
            | M::Vmovdqa
            | M::Vmovdqu
            | M::Vxorps
            | M::Vxorpd
            | M::Vpxor => {
                // The pinned executable currently requires only VEX128 VPXOR.
                // Do not inherit wider forms merely because XOR lowering is shared.
                if m == M::Vpxor && i.code() != iced_x86::Code::VEX_Vpxor_xmm_xmm_xmmm128 {
                    return Err("Only VEX128 VPXOR is implemented".into());
                }
                if !matches!(i.encoding(), iced_x86::EncodingKind::VEX)
                    || ![128, 256].contains(&width)
                {
                    return Err("Only VEX128/256 vector forms are implemented".into());
                }
                if matches!(m, M::Vxorps | M::Vxorpd | M::Vpxor) {
                    let dst = vector(i.op_register(0))?;
                    let first = vector(i.op_register(1))?;
                    if i.op_kind(2) == OpKind::Memory {
                        self.address(i)?;
                        self.emit(format!("mov x0, x19\nmov x2, #{dst}\nmov x3, #{first}\nmov x4, #{}\nbl _guest_vector_xor",width/8));
                        self.fault();
                    } else {
                        let second = vector(i.op_register(2))?;
                        self.emit(format!(
                            "ldr q0, [x19, #{}]\nldr q1, [x19, #{}]\neor v0.16b, v0.16b, v1.16b",
                            208 + second * 32,
                            208 + first * 32
                        ));
                        if width == 256 {
                            self.emit(format!("ldr q2, [x19, #{}]\nldr q3, [x19, #{}]\neor v2.16b, v2.16b, v3.16b",224+second*32,224+first*32));
                        } else {
                            self.emit("movi v2.16b, #0");
                        }
                        self.emit(format!(
                            "str q0, [x19, #{}]\nstr q2, [x19, #{}]",
                            208 + dst * 32,
                            224 + dst * 32
                        ));
                    }
                } else if i.op_kind(0) == OpKind::Register && i.op_kind(1) == OpKind::Register {
                    let dst = vector(i.op_register(0))?;
                    let src = vector(i.op_register(1))?;
                    self.emit(format!("ldr q0, [x19, #{}]", 208 + src * 32));
                    if width == 256 {
                        self.emit(format!("ldr q1, [x19, #{}]", 224 + src * 32));
                    } else {
                        self.emit("movi v1.16b, #0");
                    }
                    self.emit(format!(
                        "str q0, [x19, #{}]\nstr q1, [x19, #{}]",
                        208 + dst * 32,
                        224 + dst * 32
                    ));
                } else {
                    let load = i.op_kind(1) == OpKind::Memory;
                    let reg = vector(i.op_register(if load { 0 } else { 1 }))?;
                    self.address(i)?;
                    self.emit(format!("mov x0, x19\nmov x2, #{reg}\nmov x3, #{}\nmov x4, #{}\nbl _guest_vector_{}",width/8,u8::from(matches!(m, M::Vmovaps | M::Vmovdqa)),if load {"load"} else {"store"}));
                    self.fault();
                }
            }
            M::Shlx | M::Shrx | M::Sarx | M::Rorx | M::Mulx | M::Andn | M::Bextr => {
                if ![32, 64].contains(&width) {
                    return Err("BMI width is not implemented".into());
                }
                match m {
                    M::Shlx | M::Shrx | M::Sarx => {
                        self.read(i, 2, width, 20)?;
                        self.read(i, 1, width, 21)?;
                        let op = match m {
                            M::Shlx => "lsl",
                            M::Shrx => "lsr",
                            _ => "asr",
                        };
                        self.emit(format!(
                            "and x20, x20, #{}\n{op} {p}22, {p}21, {p}20",
                            width - 1
                        ));
                        self.write(i, 0, width, 22)?;
                    }
                    M::Rorx => {
                        self.read(i, 1, width, 20)?;
                        self.emit(format!(
                            "ror {p}20, {p}20, #{}",
                            i.immediate(2) & (width as u64 - 1)
                        ));
                        self.write(i, 0, width, 20)?;
                    }
                    M::Mulx => {
                        self.emit(format!("ldr {p}20, [x19, #16]"));
                        self.read(i, 2, width, 21)?;
                        if width == 64 {
                            self.emit("mul x22, x20, x21\numulh x20, x20, x21");
                        } else {
                            self.emit("umull x22, w20, w21\nlsr x20, x22, #32");
                        }
                        self.write(i, 1, width, 22)?;
                        self.write(i, 0, width, 20)?;
                    }
                    M::Andn => {
                        self.read(i, 1, width, 20)?;
                        self.read(i, 2, width, 21)?;
                        self.emit(format!("bic {p}22, {p}21, {p}20"));
                        self.write(i, 0, width, 22)?;
                        self.emit(format!("ldr x9, [x19, #136]\nmov x10, #0x8c1\nbic x9, x9, x10\ncmp x22, #0\ncset x10, eq\norr x9, x9, x10, lsl #6\nubfx x10, x22, #{}, #1\norr x9, x9, x10, lsl #7\nstr x9, [x19, #136]",width-1));
                    }
                    M::Bextr => {
                        // XOP BEXTR has an immediate control with different semantics.
                        if i.encoding() != iced_x86::EncodingKind::VEX {
                            return Err("Only VEX BEXTR is implemented".into());
                        }
                        self.read(i, 2, width, 20)?;
                        self.read(i, 1, width, 21)?;
                        let zero = format!(".Lbextr_zero_{:x}", i.ip());
                        let done = format!(".Lbextr_done_{:x}", i.ip());
                        self.emit(format!("and x9, x20, #255\nubfx x10, x20, #8, #8\ncmp x9, #{width}\nb.hs {zero}\ncbz x10, {zero}\nmov x11, #{width}\nsub x11, x11, x9\ncmp x10, x11\ncsel x10, x10, x11, lo\nlsr x22, x21, x9\nmov x11, #-1\nmov x12, #64\nsub x12, x12, x10\nlsr x11, x11, x12\nand x22, x22, x11\nb {done}\n{zero}:\nmov x22, #0\n{done}:"));
                        self.write(i, 0, width, 22)?;
                        self.emit("ldr x9, [x19, #136]\nmov x10, #0x841\nbic x9, x9, x10\ncmp x22, #0\ncset x10, eq\norr x9, x9, x10, lsl #6\nstr x9, [x19, #136]");
                    }
                    _ => unreachable!(),
                }
            }
            M::Vzeroupper | M::Vzeroall => {
                self.emit("movi v0.16b, #0");
                for reg in 0..16 {
                    if m == M::Vzeroall {
                        self.emit(format!("str q0, [x19, #{}]", 208 + reg * 32));
                    }
                    self.emit(format!("str q0, [x19, #{}]", 224 + reg * 32));
                }
            }
            M::Cbw => self.emit("ldrsb x20, [x19]\nstrh w20, [x19]"),
            M::Cwde => self.emit("ldrsh w20, [x19]\nstr x20, [x19]"),
            M::Cdqe => self.emit("ldrsw x20, [x19]\nstr x20, [x19]"),
            M::Cwd => self.emit("ldrsh x20, [x19]\nasr x20, x20, #63\nstrh w20, [x19, #16]"),
            M::Cdq => self.emit("ldr w20, [x19]\nasr w20, w20, #31\nstr x20, [x19, #16]"),
            M::Cqo => self.emit("ldr x20, [x19]\nasr x20, x20, #63\nstr x20, [x19, #16]"),
            M::Nop => {}
            M::Push => {
                // The only supported form pushes a full 64-bit value.
                if i.op_kind(0) == OpKind::Register && width != 64 {
                    return Err("16-bit PUSH is not implemented".into());
                }
                if matches!(
                    i.code(),
                    iced_x86::Code::Push_imm16
                        | iced_x86::Code::Pushw_imm8
                        | iced_x86::Code::Push_rm16
                ) {
                    return Err("16-bit PUSH is not implemented".into());
                }
                self.read(i, 0, 64, 20)?;
                self.emit("ldr x21, [x19, #32]\nsub x21, x21, #8\nmov x1, x21\nmov x2, x20\nmov x3, #8\nmov x0, x19\nbl _guest_store");
                self.fault();
                self.emit("str x21, [x19, #32]");
            }
            M::Pop => {
                if width != 64 || i.op_kind(0) != OpKind::Register {
                    return Err("Only register POP64 is implemented".into());
                }
                self.emit("ldr x1, [x19, #32]\nmov x0, x19\nmov x2, #8\nbl _guest_load");
                self.fault();
                self.emit(
                    "mov x20, x0\nldr x21, [x19, #32]\nadd x21, x21, #8\nstr x21, [x19, #32]",
                );
                self.write(i, 0, 64, 20)?;
            }
            M::Mov | M::Movbe | M::Movzx | M::Movsx | M::Movsxd | M::Lea | M::Not => {
                if ![8, 16, 32, 64].contains(&width) {
                    return Err("Scalar width is not implemented".into());
                }
                if m == M::Lea {
                    self.address(i)?;
                    self.emit("mov x20, x1");
                } else if m == M::Not {
                    self.read(i, 0, width, 20)?;
                    self.emit(format!("mvn {p}20, {p}20"));
                } else {
                    let src = if matches!(m, M::Movzx | M::Movsx | M::Movsxd) {
                        bits(i, 1)
                    } else {
                        width
                    };
                    self.read(i, 1, src, 20)?;
                    if matches!(m, M::Movsx | M::Movsxd) {
                        let op = match src {
                            8 => "sxtb",
                            16 => "sxth",
                            32 => "sxtw",
                            _ => return Err("Invalid sign extension".into()),
                        };
                        self.emit(format!("{op} x20, w20"));
                    }
                    if m == M::Movbe {
                        self.emit(format!(
                            "{} {p}20, {p}20",
                            if width == 16 { "rev16" } else { "rev" }
                        ));
                    }
                }
                self.write(i, 0, width, 20)?;
            }
            M::Cmp
            | M::Sub
            | M::Add
            | M::Adc
            | M::Xor
            | M::Inc
            | M::Dec
            | M::Test
            | M::And
            | M::Or
            | M::Neg
            | M::Sbb => {
                if ![8, 16, 32, 64].contains(&width) {
                    return Err("Arithmetic width is not implemented".into());
                }
                if matches!(m, M::Inc | M::Dec) {
                    self.immediate("x20", 1);
                } else if m == M::Neg {
                    self.read(i, 0, width, 20)?;
                } else {
                    self.read(i, 1, width, 20)?;
                }
                if m == M::Neg {
                    self.immediate("x21", 0);
                } else {
                    self.read(i, 0, width, 21)?;
                }
                let (op, flags) = match m {
                    M::Cmp | M::Sub | M::Neg => ("sub", 0),
                    M::Sbb => ("sub", 4),
                    M::Add => ("add", 1),
                    M::Adc => ("add", 7),
                    M::Inc => ("add", 3),
                    M::Dec => ("sub", 5),
                    M::Xor => ("eor", 2),
                    M::Test | M::And => ("and", 2),
                    _ => ("orr", 2),
                };
                self.emit(format!("{op} {p}22, {p}21, {p}20"));
                if matches!(m, M::Sbb | M::Adc) {
                    let carry_op = if m == M::Adc { "add" } else { "sub" };
                    self.emit(format!(
                        "ldr x9, [x19, #136]\nand x9, x9, #1\n{carry_op} {p}22, {p}22, {p}9"
                    ));
                }
                if !matches!(m, M::Cmp | M::Test) {
                    self.write(i, 0, width, 22)?;
                }
                self.emit(format!("ldr x0, [x19, #136]\nmov x1, x21\nmov x2, x20\nmov x3, x22\nmov x4, #{width}\nmov x5, #{flags}\nbl _native_ps5_arithmetic_flags_v1\nstr x0, [x19, #136]\nstr x1, [x19, #152]"));
                self.fault();
            }
            M::Div | M::Idiv => {
                if ![16, 32, 64].contains(&width) {
                    return Err("Only DIV/IDIV16/32/64 is implemented".into());
                }
                self.read(i, 0, width, 20)?;
                self.emit(format!("mov x0, x19\nmov x1, x20\nmov x2, #{width}\nmov x3, #{}\nbl _native_ps5_divide_v1", u8::from(m == M::Idiv)));
                self.fault();
            }
            M::Imul if i.op_count() == 1 => {
                // Intel SDM IMUL: signed full-width product goes to AX or
                // DX:AX/EDX:EAX/RDX:RAX. Only CF/OF are defined.
                if ![8, 16, 32, 64].contains(&width) {
                    return Err("Invalid one-operand IMUL width".into());
                }
                self.read(i, 0, width, 20)?;
                self.emit("ldr x21, [x19, #0]");
                if width < 64 {
                    self.emit(format!(
                        "sbfx x20, x20, #0, #{width}\nsbfx x21, x21, #0, #{width}"
                    ));
                }
                self.emit("mul x22, x21, x20");
                if width == 64 {
                    self.emit("smulh x9, x21, x20\nstr x22, [x19, #0]\nstr x9, [x19, #16]");
                } else if width == 32 {
                    self.emit(
                        "lsr x9, x22, #32\nmov w22, w22\nstr x22, [x19, #0]\nstr x9, [x19, #16]",
                    );
                } else if width == 16 {
                    self.emit("lsr x9, x22, #16\nstrh w22, [x19, #0]\nstrh w9, [x19, #16]");
                } else {
                    self.emit("strh w22, [x19, #0]");
                }
                self.emit(format!("ldr x0, [x19, #136]\nmov x1, x21\nmov x2, x20\nmov x3, x22\nmov x4, #{width}\nmov x5, #6\nbl _native_ps5_arithmetic_flags_v1\nstr x0, [x19, #136]\nstr x1, [x19, #152]"));
                self.fault();
            }
            M::Imul => {
                if ![16, 32, 64].contains(&width)
                    || ![2, 3].contains(&i.op_count())
                    || i.op_kind(0) != OpKind::Register
                {
                    return Err("Only two/three-operand IMUL16/32/64 is implemented".into());
                }
                let first = if i.op_count() == 2 { 0 } else { 1 };
                self.read(i, first, width, 20)?;
                self.read(i, first + 1, width, 21)?;
                self.emit(format!("mul {p}22, {p}20, {p}21"));
                self.write(i, 0, width, 22)?;
                self.emit(format!("ldr x0, [x19, #136]\nmov x1, x20\nmov x2, x21\nmov x3, x22\nmov x4, #{width}\nmov x5, #6\nbl _native_ps5_arithmetic_flags_v1\nstr x0, [x19, #136]\nstr x1, [x19, #152]"));
                self.fault();
            }
            M::Shl | M::Shr | M::Sar | M::Rol | M::Ror => {
                if ![32, 64].contains(&width) {
                    return Err("Shift width is not implemented".into());
                }
                self.read(i, 1, 8, 20)?;
                self.emit(format!("and x20, x20, #{}", width - 1));
                self.read(i, 0, width, 21)?;
                let (op, flags) = match m {
                    M::Shl => ("lsl", 0),
                    M::Shr => ("lsr", 1),
                    M::Sar => ("asr", 2),
                    M::Rol => ("ror", 3),
                    _ => ("ror", 4),
                };
                let count = if m == M::Rol {
                    self.emit("neg x9, x20");
                    9
                } else {
                    20
                };
                self.emit(format!("{op} {p}22, {p}21, {p}{count}"));
                self.write(i, 0, width, 22)?;
                self.emit(format!("ldr x0, [x19, #136]\nmov x1, x21\nmov x2, x20\nmov x3, x22\nmov x4, #{width}\nmov x5, #{flags}\nbl _native_ps5_shift_flags_v1\nstr x0, [x19, #136]\nstr x1, [x19, #152]"));
                self.fault();
            }
            M::Jmp => {
                if !i.is_jmp_near_indirect() || width != 64 {
                    return Err("Only near indirect JMP64 is implemented".into());
                }
                self.read(i, 0, 64, 20)?;
                self.emit("str x20, [x19, #176]\nmov x9, #5\nstr x9, [x19, #168]\nb .Lexit");
            }
            M::Call => {
                self.immediate("x9", i.next_ip());
                self.emit("str x9, [x19, #192]");
                if i.is_call_near() {
                    self.stop(1, i.near_branch_target());
                } else if i.is_call_near_indirect() {
                    self.read(i, 0, 64, 20)?;
                    self.emit("str x20, [x19, #176]\nmov x9, #1\nstr x9, [x19, #168]\nb .Lexit");
                } else {
                    return Err("Far CALL is not implemented".into());
                }
            }
            M::Ret => {
                if i.op_count() != 0 || i.code() != iced_x86::Code::Retnq {
                    return Err("Only near RET64 is implemented".into());
                }
                self.stop(3, 0);
            }
            _ => {
                return Err(format!(
                    "ARM64 lowering is not implemented for {:?}",
                    i.code()
                ));
            }
        }
        Ok(())
    }
}

/// Fail before publishing output if any instruction cannot be lowered.
pub fn supported(i: &Instruction) -> Result<()> {
    Emitter { lines: vec![] }.instruction(i)
}

pub fn translate(
    code: &[u8],
    address: u64,
    symbol: &str,
    region: bool,
    external: bool,
) -> Result<(String, Vec<Instruction>)> {
    translate_with_trace(
        code,
        address,
        symbol,
        region,
        external,
        &crate::trace::selected()?,
    )
}
pub fn translate_with_trace(
    code: &[u8],
    address: u64,
    symbol: &str,
    region: bool,
    external: bool,
    trace_pcs: &BTreeSet<u64>,
) -> Result<(String, Vec<Instruction>)> {
    if symbol.is_empty()
        || !symbol
            .bytes()
            .enumerate()
            .all(|(n, c)| c == b'_' || c.is_ascii_alphabetic() || (n > 0 && c.is_ascii_digit()))
    {
        return Err("Invalid assembly symbol".into());
    }
    if external && !region {
        return Err("External branches require a resumable region".into());
    }
    let mut instructions = vec![];
    let mut pos = 0;
    while pos < code.len() {
        let pc = address
            .checked_add(pos as u64)
            .ok_or("Code address overflow")?;
        let i = decode::instruction(&code[pos..], pc)?;
        supported(&i).map_err(|e| format!("{pc:#x}: {e}"))?;
        pos += i.len();
        instructions.push(i);
        if !region && (matches!(i.mnemonic(), M::Call | M::Ret) || i.is_jmp_near_indirect()) {
            break;
        }
    }
    let last = instructions.last().ok_or("Empty translation")?;
    if !external && !(matches!(last.mnemonic(), M::Call | M::Ret) || last.is_jmp_near_indirect()) {
        return Err("Prefix must end at CALL/RET or indirect JMP".into());
    }
    let boundaries: BTreeSet<_> = instructions.iter().map(Instruction::ip).collect();
    let mut exits = BTreeSet::new();
    for i in &instructions {
        if i.is_jcc_short_or_near() || i.is_jmp_short_or_near() {
            let target = i.near_branch_target();
            if !boundaries.contains(&target) {
                if !external || (address..last.next_ip()).contains(&target) {
                    return Err("Branch exits prefix or enters instruction interior".into());
                }
                exits.insert(target);
            }
        }
    }
    if external {
        exits.insert(last.next_ip());
    }
    let mut e = Emitter { lines: vec![] };
    e.emit(".text\n.p2align 2\n.globl _translated_start\n_translated_start:\nstp x19, x20, [sp, #-48]!\nstp x21, x22, [sp, #16]\nstr x30, [sp, #32]\nmov x19, x0");
    e.fault();
    if region {
        e.emit("ldr x10, [x19, #128]");
        for i in &instructions {
            e.immediate("x9", i.ip());
            e.emit(format!("cmp x10, x9\nb.eq .Lpc_{:x}", i.ip()));
        }
        e.emit("mov x9, #7\nstr x9, [x19, #152]\nb .Lexit");
    }
    for i in &instructions {
        e.emit(format!(".Lpc_{:x}:", i.ip()));
        e.immediate("x9", i.ip());
        e.emit("str x9, [x19, #128]\nldr x9, [x19, #160]\ncbz x9, .Lfuel\nsub x9, x9, #1\nstr x9, [x19, #160]");
        if trace_pcs.contains(&i.ip()) {
            e.immediate("x0", i.ip());
            e.emit("ldr x1, [x19, #0]\nldr x2, [x19, #56]\nldr x3, [x19, #48]\nldr x4, [x19, #16]\nldr x5, [x19, #32]\nldr x6, [x19, #136]\nbl _native_ps5_trace_v1");
        }
        e.instruction(i)?;
    }
    if external {
        e.emit(format!("b .Lpc_{:x}", last.next_ip()));
        for target in exits {
            e.emit(format!(".Lpc_{target:x}:"));
            e.immediate("x9", target);
            e.emit("str x9, [x19, #128]");
            e.stop(2, target);
        }
    }
    e.emit(".Lfuel:\nmov x9, #4\nstr x9, [x19, #168]\n.Lexit:\nldr x30, [sp, #32]\nldp x21, x22, [sp, #16]\nldp x19, x20, [sp], #48\nret");
    Ok((
        (e.lines.join("\n") + "\n")
            .replace("_translated_start", &format!("_{symbol}"))
            .replace(".L", &format!(".L{symbol}_")),
        instructions,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unsupported_instead_of_silently_skipping() {
        for bytes in [
            &[0xf7, 0xe0, 0xc3][..],
            &[0xf6, 0xf0, 0xc3],
            &[0xf0, 0x01, 0x00, 0xc3],
            &[0x64, 0x48, 0x8b, 0x00, 0xc3],
            &[0x67, 0x8b, 0x00, 0xc3],
            &[0xc2, 8, 0],
        ] {
            assert!(translate(bytes, 0, "test", false, false).is_err());
        }
    }
    #[test]
    fn indirect_tail_jump_ends_discovery_without_fallthrough() {
        let (_, decoded) = translate(&[0xff, 0x27, 0xc3], 0, "tail", false, false).unwrap();
        assert_eq!(decoded.len(), 1);
        assert!(terminal(&decoded[0]));
        assert!(successors(&decoded[0]).is_empty());
        assert!(translate(&[0xff, 0x2f], 0, "far_tail", false, false).is_err());
    }
    #[test]
    fn rejects_interior_branch_and_bad_symbols() {
        assert!(translate(&[0xeb, 1, 0x48, 0x89, 0xc0, 0xc3], 0, "test", true, true).is_err());
        assert!(translate(&[0xc3], 0, "bad\n_symbol", false, false).is_err());
    }
    #[test]
    fn guards_and_byte_register_offsets_survive_lowering() {
        let (s, _) = translate(
            &[0x88, 0xe0, 0x40, 0x88, 0xe0, 0xc3],
            0,
            "byte_test",
            false,
            false,
        )
        .unwrap();
        assert!(s.contains("ldrb w20, [x19, #1]"));
        assert!(s.contains("ldrb w20, [x19, #32]"));
        assert!(s.contains("cbz x9, .Lbyte_test_fuel"));
    }
}
#[cfg(test)]
mod scalar_vmovsd_tests {
    use super::*;
    #[test]
    fn observed_scalar_memory_load_only() {
        for bytes in [
            &[0xc5, 0xfb, 0x10, 0x05, 0x2a, 0xdd, 0x1c, 0][..],
            &[0xc5, 0x7b, 0x10, 0x3f][..],
        ] {
            let instruction = decode::instruction(bytes, 0xc3f2e).unwrap();
            assert_eq!(instruction.code(), iced_x86::Code::VEX_Vmovsd_xmm_m64);
            assert!(supported(&instruction).is_ok());
        }
        // Scalar store and register merge forms remain explicitly unsupported.
        assert!(translate_rust_memory_region(&[0xc5, 0xfb, 0x11, 0x07], 0, "no_store").is_err());
        assert!(translate_rust_memory_region(&[0xc5, 0xf3, 0x10, 0xc2], 0, "no_merge").is_err());
    }
}
#[cfg(test)]
mod scalar_integer_conversion_tests {
    use super::*;
    #[test]
    fn exact_signed32_and_signed64_vex_forms_only() {
        for bytes in [
            &[0xc4, 0xc1, 0x03, 0x2a, 0xe1][..],
            &[0xc4, 0xe1, 0x03, 0x2a, 0x27][..],
        ] {
            let i = decode::instruction(bytes, 0xc3f64).unwrap();
            assert_eq!(i.code(), iced_x86::Code::VEX_Vcvtsi2sd_xmm_xmm_rm32);
            assert!(supported(&i).is_ok());
        }
        let i = decode::instruction(&[0xc4, 0xc1, 0x83, 0x2a, 0xe1], 0).unwrap();
        assert_eq!(i.code(), iced_x86::Code::VEX_Vcvtsi2sd_xmm_xmm_rm64);
        assert!(supported(&i).is_ok());
        assert!(
            translate_rust_memory_region(&[0xf2, 0x48, 0x0f, 0x2a, 0xc0], 0, "legacy").is_err()
        );
    }
}
#[cfg(test)]
mod mxcsr_control_tests {
    use super::*;
    #[test]
    fn legacy_and_vex_control_forms() {
        for bytes in [
            &[0x0f, 0xae, 0x17][..],
            &[0x0f, 0xae, 0x1f][..],
            &[0xc5, 0xf8, 0xae, 0x17][..],
            &[0xc5, 0xf8, 0xae, 0x1f][..],
        ] {
            let i = decode::instruction(bytes, 0x100).unwrap();
            assert!(supported(&i).is_ok());
        }
        assert!(decode::instruction(&[0xc5, 0xfc, 0xae, 0x17], 0x100).is_err());
    }
}

#[cfg(test)]
mod truncate_tests {
    use super::*;
    #[test]
    fn truncate_exact_vex32_forms() {
        for bytes in [
            &[0xc5, 0xfb, 0x2c, 0xc0][..],
            &[0xc4, 0x41, 0x7b, 0x2c, 0xcf][..],
            &[0xc5, 0xfb, 0x2c, 0x07][..],
        ] {
            let instruction = decode::instruction(bytes, 0xc3fca).unwrap();
            assert_eq!(
                instruction.code(),
                iced_x86::Code::VEX_Vcvttsd2si_r32_xmmm64
            );
            assert!(supported(&instruction).is_ok());
        }
        for bytes in [
            &[0xf2, 0x0f, 0x2c, 0xc0][..],
            &[0xc4, 0xe1, 0xfb, 0x2c, 0xc0][..],
        ] {
            assert!(translate_rust_memory_region(bytes, 0, "unsupported_truncate").is_err());
        }
    }
}

#[cfg(test)]
mod vmulsd_tests {
    use super::*;
    #[test]
    fn scalar_vex_multiply_forms_only() {
        for bytes in [
            &[0xc5, 0xdb, 0x59, 0xe0][..],
            &[0xc5, 0xdb, 0x59, 0xd1][..],
            &[0xc5, 0xdb, 0x59, 0x17][..],
            &[0xc4, 0x41, 0x83, 0x59, 0xfe][..],
        ] {
            let i = decode::instruction(bytes, 0xc3f76).unwrap();
            assert_eq!(i.code(), iced_x86::Code::VEX_Vmulsd_xmm_xmm_xmmm64);
            assert!(supported(&i).is_ok());
        }
        for bytes in [&[0xf2, 0x0f, 0x59, 0xc1][..], &[0xc5, 0xf9, 0x59, 0xc1][..]] {
            assert!(translate_rust_memory_region(bytes, 0, "unsupported_multiply").is_err());
        }
    }
}
#[cfg(test)]
mod fma213_tests {
    use super::*;
    #[test]
    fn exact_fma213_scalar_vex_forms() {
        for bytes in [
            &[0xc4, 0xe2, 0xe1, 0xa9, 0xe2][..],
            &[0xc4, 0xe2, 0xe1, 0xa9, 0x27][..],
            &[0xc4, 0x42, 0x89, 0xa9, 0xfd][..],
        ] {
            let i = decode::instruction(bytes, 0xc3fb4).unwrap();
            assert_eq!(i.code(), iced_x86::Code::VEX_Vfmadd213sd_xmm_xmm_xmmm64);
            assert!(supported(&i).is_ok());
        }
        for bytes in [
            &[0xc4, 0xe2, 0xe1, 0x99, 0xe2][..],
            &[0xc4, 0xe2, 0xe1, 0xb9, 0xe2][..],
            &[0xc4, 0xe2, 0x61, 0xa9, 0xe2][..],
        ] {
            assert!(translate_rust_memory_region(bytes, 0, "unsupported_fma").is_err());
        }
    }
}
