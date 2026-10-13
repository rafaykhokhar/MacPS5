//! Position-independent branch fixups for the direct ARM64 backend.
use crate::Result;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug)]
pub enum Branch {
    Jump,
    Call,
    /// ARM condition encoding 0..=13 (EQ through LE); AL/NV rejected.
    Conditional(u8),
    CompareZero {
        register: u8,
        wide: bool,
        nonzero: bool,
    },
    TestBit {
        register: u8,
        bit: u8,
        nonzero: bool,
    },
}
impl Branch {
    fn encoding(self) -> Result<(u32, u8, u8)> {
        Ok(match self {
            Self::Jump => (0x14000000, 26, 0),
            Self::Call => (0x94000000, 26, 0),
            Self::Conditional(condition) if condition < 14 => {
                (0x54000000 | u32::from(condition), 19, 5)
            }
            Self::CompareZero {
                register,
                wide,
                nonzero,
            } if register < 32 => (
                0x34000000
                    | (u32::from(wide) << 31)
                    | (u32::from(nonzero) << 24)
                    | u32::from(register),
                19,
                5,
            ),
            Self::TestBit {
                register,
                bit,
                nonzero,
            } if register < 32 && bit < 64 => (
                0x36000000
                    | (u32::from(bit >> 5) << 31)
                    | (u32::from(nonzero) << 24)
                    | (u32::from(bit & 31) << 19)
                    | u32::from(register),
                14,
                5,
            ),
            _ => return Err("Invalid ARM64 branch operand".into()),
        })
    }
    fn resolve(self, from: usize, to: usize) -> Result<u32> {
        let (base, bits, shift) = self.encoding()?;
        // Addresses are instruction indices, so displacement is already /4.
        let distance = to as i128 - from as i128;
        let bound = 1i128 << (bits - 1);
        if !(-bound..bound).contains(&distance) {
            return Err("ARM64 branch target is out of range".into());
        }
        Ok(base | (((distance as u32) & ((1u32 << bits) - 1)) << shift))
    }
}

/// Labels are caller-chosen IDs scoped to one buffer. No executable memory is
/// changed until finish succeeds and its complete result is published.
pub struct Arm64Code {
    words: Vec<u32>,
    labels: BTreeMap<u32, usize>,
    fixups: Vec<(usize, u32, Branch)>,
    limit: usize,
}
impl Arm64Code {
    pub fn new(instruction_limit: usize) -> Self {
        Self {
            words: Vec::new(),
            labels: BTreeMap::new(),
            fixups: Vec::new(),
            limit: instruction_limit,
        }
    }
    pub fn emit(&mut self, word: u32) -> Result<()> {
        if self.words.len() >= self.limit {
            return Err("ARM64 emission budget exceeded".into());
        }
        self.words.push(word);
        Ok(())
    }
    pub fn bind(&mut self, label: u32) -> Result<()> {
        if self.labels.contains_key(&label) {
            return Err("Duplicate ARM64 label".into());
        }
        self.labels.insert(label, self.words.len());
        Ok(())
    }
    /// Unsigned-offset LDR/STR for trusted host context fields, not guest
    /// addresses. X31 base means SP; register 31 means WZR/XZR.
    pub fn context_access(
        &mut self,
        register: u8,
        base: u8,
        offset: usize,
        wide: bool,
        store: bool,
    ) -> Result<()> {
        let scale = if wide { 8 } else { 4 };
        if register > 31 || base > 31 || offset % scale != 0 || offset / scale > 4095 {
            return Err("Invalid ARM64 context access".into());
        }
        let opcode = match (wide, store) {
            (false, false) => 0xb9400000,
            (false, true) => 0xb9000000,
            (true, false) => 0xf9400000,
            (true, true) => 0xf9000000,
        };
        self.emit(
            opcode
                | (((offset / scale) as u32) << 10)
                | (u32::from(base) << 5)
                | u32::from(register),
        )
    }
    /// Materialize all 64 bits with MOVZ/MOVK, without changing flags.
    /// X31 is excluded because this API promises a stored register value.
    pub fn load_u64(&mut self, register: u8, value: u64) -> Result<()> {
        if register >= 31 {
            return Err("ARM64 constant destination must be X0..X30".into());
        }
        self.reserve_words(4)?;
        for half in 0..4 {
            let opcode = if half == 0 { 0xd2800000 } else { 0xf2800000 };
            self.words.push(
                opcode
                    | (half << 21)
                    | (((value >> (half * 16)) as u32 & 0xffff) << 5)
                    | u32::from(register),
            );
        }
        Ok(())
    }
    /// Call a trusted absolute host target using ABI scratch register X16.
    /// The caller must save LR, align SP and marshal arguments before this
    /// sequence, and preserve any caller-saved state needed after it.
    /// Target lifetime/ABI validity are checked by the publisher, not here.
    pub fn call_absolute(&mut self, address: u64) -> Result<()> {
        if address == 0 || address % 4 != 0 {
            return Err("ARM64 call target must be nonzero and aligned".into());
        }
        self.reserve_words(5)?;
        self.load_u64(16, address)?;
        self.words.push(0xd63f0200); // BLR X16
        Ok(())
    }
    fn reserve_words(&self, count: usize) -> Result<()> {
        if self
            .words
            .len()
            .checked_add(count)
            .is_none_or(|end| end > self.limit)
        {
            return Err("ARM64 emission budget exceeded".into());
        }
        Ok(())
    }
    pub fn branch(&mut self, target: u32, kind: Branch) -> Result<()> {
        kind.encoding()?;
        let at = self.words.len();
        self.emit(0)?;
        self.fixups.push((at, target, kind));
        Ok(())
    }
    pub fn finish(mut self) -> Result<Vec<u32>> {
        if self.words.is_empty() {
            return Err("Empty ARM64 code buffer".into());
        }
        for (at, label, kind) in self.fixups {
            let target = *self.labels.get(&label).ok_or("Unresolved ARM64 label")?;
            if target >= self.words.len() {
                return Err("ARM64 label has no target instruction".into());
            }
            self.words[at] = kind.resolve(at, target)?;
        }
        Ok(self.words)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn context_address_rejections_do_not_emit() {
        let mut code = Arm64Code::new(1);
        for (reg, base, offset, wide) in [
            (32, 0, 0, true),
            (0, 32, 0, true),
            (0, 0, 1, false),
            (0, 0, 4, true),
            (0, 0, 32768, true),
            (0, 0, 16384, false),
        ] {
            assert!(code.context_access(reg, base, offset, wide, false).is_err());
        }
        code.context_access(0, 0, 0, true, false).unwrap();
        assert_eq!(code.finish().unwrap(), [0xf9400000]);
    }
    #[test]
    fn multiword_emission_is_atomic_on_rejection() {
        let mut code = Arm64Code::new(5);
        code.emit(0xd503201f).unwrap();
        assert!(code.call_absolute(4).is_err());
        assert!(code.load_u64(31, 0).is_err());
        assert!(code.call_absolute(0).is_err());
        assert!(code.call_absolute(3).is_err());
        code.load_u64(0, 0).unwrap();
        assert_eq!(
            code.finish().unwrap(),
            [0xd503201f, 0xd2800000, 0xf2a00000, 0xf2c00000, 0xf2e00000]
        );
    }
    #[test]
    fn signed_branch_limits_and_operand_validation() {
        for (kind, bits) in [
            (Branch::Jump, 26),
            (Branch::Call, 26),
            (Branch::Conditional(13), 19),
            (
                Branch::CompareZero {
                    register: 31,
                    wide: true,
                    nonzero: true,
                },
                19,
            ),
            (
                Branch::TestBit {
                    register: 31,
                    bit: 63,
                    nonzero: true,
                },
                14,
            ),
        ] {
            let half = 1usize << (bits - 1);
            assert!(kind.resolve(0, half - 1).is_ok());
            assert!(kind.resolve(half, 0).is_ok());
            assert!(kind.resolve(0, half).is_err());
            assert!(kind.resolve(half + 1, 0).is_err());
        }
        for kind in [
            Branch::Conditional(14),
            Branch::Conditional(255),
            Branch::CompareZero {
                register: 32,
                wide: false,
                nonzero: false,
            },
            Branch::TestBit {
                register: 0,
                bit: 64,
                nonzero: false,
            },
        ] {
            assert!(kind.encoding().is_err());
        }
    }
    #[test]
    fn labels_and_failed_emission_preserve_buffer() {
        let mut code = Arm64Code::new(2);
        code.bind(1).unwrap();
        assert!(code.bind(1).is_err());
        assert!(code.branch(1, Branch::Conditional(99)).is_err());
        code.emit(0xd503201f).unwrap();
        code.branch(1, Branch::Jump).unwrap();
        assert!(code.emit(0).is_err());
        assert_eq!(code.finish().unwrap(), [0xd503201f, 0x17ffffff]);
        let mut missing = Arm64Code::new(1);
        missing.branch(0, Branch::Jump).unwrap();
        assert!(missing.finish().is_err());
        let mut end = Arm64Code::new(1);
        end.branch(0, Branch::Jump).unwrap();
        end.bind(0).unwrap();
        assert!(end.finish().is_err());
        assert!(Arm64Code::new(0).finish().is_err());
    }
}
