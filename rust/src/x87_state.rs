//! Raw x87 stack storage and stack-fault transitions, independent of Cpu ABI.
//! Intel SDM Vol. 1 §§8.1.2, 8.1.3, 8.1.7, 8.5.1.1:
//! https://cdrdv2-public.intel.com/671436/253665-sdm-vol-1.pdf
//! CW and SW stay in the caller: TOP has exactly one source of truth, SW[13:11].
//! Raw transfers never round, normalize pseudo-denormals, or quiet NaNs. An
//! instruction adapter must implement operand/conversion exceptions separately.
//! Caller validates supported CW encodings before execution. No arithmetic,
//! FXSAVE/FRSTOR format, instruction pointers or #MF delivery.

pub type Raw80 = [u8; 10];
pub const INDEFINITE: Raw80 = [0, 0, 0, 0, 0, 0, 0, 0xc0, 0xff, 0xff];
const TOP_MASK: u16 = 7 << 11;
const IE: u16 = 1;
const SF: u16 = 1 << 6;
const ES: u16 = 1 << 7;
const C1: u16 = 1 << 9;
const BUSY: u16 = 1 << 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Zero,
    Normal,
    Denormal,
    PseudoDenormal,
    Infinity,
    QuietNaN,
    SignalingNaN,
    Unsupported,
}
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Valid = 0,
    Zero = 1,
    Special = 2,
    Empty = 3,
}
pub fn classify(value: Raw80) -> Class {
    let significand = u64::from_le_bytes(value[..8].try_into().unwrap());
    let exponent = u16::from_le_bytes([value[8], value[9]]) & 0x7fff;
    let integer = significand >> 63 != 0;
    let fraction = significand & 0x7fff_ffff_ffff_ffff;
    match (exponent, integer, fraction) {
        (0, false, 0) => Class::Zero,
        (0, false, _) => Class::Denormal,
        (0, true, _) => Class::PseudoDenormal,
        (0x7fff, true, 0) => Class::Infinity,
        (0x7fff, true, f) if f & (1 << 62) != 0 => Class::QuietNaN,
        (0x7fff, true, _) => Class::SignalingNaN,
        (_, false, _) => Class::Unsupported,
        _ => Class::Normal,
    }
}
fn value_tag(value: Raw80) -> Tag {
    match classify(value) {
        Class::Zero => Tag::Zero,
        Class::Normal => Tag::Valid,
        _ => Tag::Special,
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    PendingUnmasked,
    StackOverflow,
    StackUnderflow,
    InvalidIndex,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    pub value: Raw80,
    /// A masked stack fault returned indefinite. Operand faults are not modeled.
    pub masked_stack_fault: bool,
}
/// Physical register order R0..R7, with two tag bits per physical register.
/// `repr(C)` gives an 82-byte appendable payload; this does not change Cpu yet.
#[repr(C)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stack {
    registers: [Raw80; 8],
    tags: u16,
}
impl Default for Stack {
    fn default() -> Self {
        Self {
            registers: [[0; 10]; 8],
            tags: 0xffff,
        }
    }
}
pub fn top(status: u16) -> usize {
    usize::from((status >> 11) & 7)
}
fn set_top(status: &mut u16, value: usize) {
    *status = (*status & !TOP_MASK) | ((value as u16 & 7) << 11);
}
pub fn pending(control: u16, status: u16) -> bool {
    status & !control & 0x3f != 0
}
fn waiting(control: u16, status: u16) -> Result<(), Fault> {
    if pending(control, status) {
        Err(Fault::PendingUnmasked)
    } else {
        Ok(())
    }
}
/// FNCLEX-style exception clearing; preserves TOP and all condition codes.
pub fn clear_exceptions(status: &mut u16) {
    *status &= !0x80ff;
}
fn stack_fault(control: u16, status: &mut u16, overflow: bool) -> Result<(), Fault> {
    *status = (*status & !C1) | IE | SF | if overflow { C1 } else { 0 };
    if control & IE == 0 {
        *status |= ES | BUSY;
        Err(if overflow {
            Fault::StackOverflow
        } else {
            Fault::StackUnderflow
        })
    } else {
        Ok(())
    }
}
impl Stack {
    pub fn tag_word(&self) -> u16 {
        self.tags
    }
    pub fn physical_tag(&self, index: usize) -> Result<Tag, Fault> {
        if index >= 8 {
            return Err(Fault::InvalidIndex);
        }
        Ok(match (self.tags >> (index * 2)) & 3 {
            0 => Tag::Valid,
            1 => Tag::Zero,
            2 => Tag::Special,
            _ => Tag::Empty,
        })
    }
    fn set_tag(&mut self, index: usize, tag: Tag) {
        let shift = index * 2;
        self.tags = (self.tags & !(3 << shift)) | ((tag as u16) << shift);
    }
    /// Debug/state inspection only: does not execute a waiting instruction.
    pub fn peek(&self, status: u16, logical: usize) -> Result<Option<Raw80>, Fault> {
        if logical >= 8 {
            return Err(Fault::InvalidIndex);
        }
        let index = (top(status) + logical) & 7;
        Ok((self.physical_tag(index)? != Tag::Empty).then_some(self.registers[index]))
    }
    /// FFREE's tag operation: does not rotate TOP or erase physical payload.
    /// Caller handles waiting and instruction-specific condition-code effects.
    pub fn free(&mut self, status: u16, logical: usize) -> Result<(), Fault> {
        if logical >= 8 {
            return Err(Fault::InvalidIndex);
        }
        self.set_tag((top(status) + logical) & 7, Tag::Empty);
        Ok(())
    }
    /// Waiting raw push. Checks the destination slot, not a count of live tags.
    /// Masked overflow rotates TOP and replaces that slot with indefinite.
    pub fn push_raw(
        &mut self,
        control: u16,
        status: &mut u16,
        value: Raw80,
    ) -> Result<Transfer, Fault> {
        waiting(control, *status)?;
        let index = (top(*status) + 7) & 7;
        let overflow = self.physical_tag(index)? != Tag::Empty;
        if overflow {
            stack_fault(control, status, true)?;
        } else {
            *status &= !C1;
        }
        let value = if overflow { INDEFINITE } else { value };
        self.registers[index] = value;
        self.set_tag(index, value_tag(value));
        set_top(status, index);
        Ok(Transfer {
            value,
            masked_stack_fault: overflow,
        })
    }
    /// Waiting raw ST0 extraction/pop (the register part of an extended store).
    /// Prevalidate any memory destination before calling this consuming method.
    /// Masked empty extraction returns indefinite and advances TOP; unmasked
    /// faults retain every register, tag and TOP, while setting status flags.
    pub fn pop_raw(&mut self, control: u16, status: &mut u16) -> Result<Transfer, Fault> {
        waiting(control, *status)?;
        let index = top(*status);
        let empty = self.physical_tag(index)? == Tag::Empty;
        if empty {
            stack_fault(control, status, false)?;
        } else {
            *status &= !C1;
        }
        let value = if empty {
            INDEFINITE
        } else {
            self.registers[index]
        };
        self.set_tag(index, Tag::Empty);
        set_top(status, (index + 1) & 7);
        Ok(Transfer {
            value,
            masked_stack_fault: empty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(significand: u64, exponent: u16) -> Raw80 {
        let mut value = [0; 10];
        value[..8].copy_from_slice(&significand.to_le_bytes());
        value[8..].copy_from_slice(&exponent.to_le_bytes());
        value
    }
    #[test]
    fn classification_keeps_explicit_integer_bit_and_payloads() {
        for (significand, exponent, class) in [
            (0, 0, Class::Zero),
            (0, 0x8000, Class::Zero),
            (1, 0, Class::Denormal),
            (1 << 63, 0, Class::PseudoDenormal),
            (1 << 63, 1, Class::Normal),
            (u64::MAX, 0x7ffe, Class::Normal),
            (1 << 63, 0xffff, Class::Infinity),
            (0xc000_0000_0000_0042, 0x7fff, Class::QuietNaN),
            (0x8000_0000_0000_0042, 0x7fff, Class::SignalingNaN),
            (0, 1, Class::Unsupported),
            (1, 0x7fff, Class::Unsupported),
        ] {
            assert_eq!(classify(raw(significand, exponent)), class);
        }
        assert_eq!(classify(INDEFINITE), Class::QuietNaN);
    }
    #[test]
    fn all_eight_registers_round_trip_raw80_without_rounding() {
        let mut stack = Stack::default();
        let mut status = 0x4500;
        assert_eq!(stack.tag_word(), 0xffff);
        for i in 0..8 {
            stack
                .push_raw(0x007f, &mut status, raw(0x8000_0000_0000_0001 + i, 0x4010))
                .unwrap();
        }
        assert_eq!(top(status), 0);
        assert_eq!(stack.tag_word(), 0);
        for i in (0..8).rev() {
            assert_eq!(
                stack.pop_raw(0x007f, &mut status).unwrap().value,
                raw(0x8000_0000_0000_0001 + i, 0x4010)
            );
        }
        assert_eq!(stack.tag_word(), 0xffff);
        assert_eq!(status, 0x4500);
        assert_eq!(std::mem::size_of::<Stack>(), 82);
    }
    #[test]
    fn physical_tags_and_holes_are_not_a_depth_counter() {
        let mut stack = Stack::default();
        let mut status = 0;
        stack.push_raw(0x37f, &mut status, raw(0, 0)).unwrap();
        assert_eq!(stack.physical_tag(7), Ok(Tag::Zero));
        stack.push_raw(0x37f, &mut status, raw(1, 0)).unwrap();
        assert_eq!(stack.physical_tag(6), Ok(Tag::Special));
        stack.free(status, 1).unwrap();
        assert_eq!(stack.peek(status, 1), Ok(None));
        assert_eq!(top(status), 6);
        assert_eq!(stack.peek(status, 8), Err(Fault::InvalidIndex));
        assert_eq!(stack.free(status, 8), Err(Fault::InvalidIndex));
    }
    #[test]
    fn push_checks_target_slot_even_when_other_slots_are_empty() {
        let mut stack = Stack::default();
        let mut status = 0;
        stack
            .push_raw(0x37f, &mut status, raw(1 << 63, 0x3fff))
            .unwrap();
        // FINCSTP-style TOP rotation does not free the occupied physical R7.
        set_top(&mut status, 0);
        let result = stack.push_raw(0x37f, &mut status, [0; 10]).unwrap();
        assert!(result.masked_stack_fault);
        assert_eq!(stack.physical_tag(7), Ok(Tag::Special));
        assert_eq!(stack.physical_tag(6), Ok(Tag::Empty));
    }
    #[test]
    fn masked_overflow_and_underflow_return_indefinite() {
        let mut stack = Stack::default();
        let mut status = 0;
        for _ in 0..8 {
            stack
                .push_raw(0x37f, &mut status, raw(1 << 63, 0x3fff))
                .unwrap();
        }
        let pushed = stack.push_raw(0x37f, &mut status, raw(0, 0)).unwrap();
        assert!(pushed.masked_stack_fault);
        assert_eq!(pushed.value, INDEFINITE);
        assert_eq!(top(status), 7);
        assert_eq!(status & (IE | SF | C1 | ES | BUSY), IE | SF | C1);
        for _ in 0..8 {
            stack.pop_raw(0x37f, &mut status).unwrap();
        }
        let popped = stack.pop_raw(0x37f, &mut status).unwrap();
        assert!(popped.masked_stack_fault);
        assert_eq!(popped.value, INDEFINITE);
        assert_eq!(top(status), 0);
        assert_eq!(status & (IE | SF | C1), IE | SF);
    }
    #[test]
    fn unmasked_faults_preserve_stack_then_wait_and_clear() {
        let mut stack = Stack::default();
        let mut status = 0x4500;
        let before = stack.clone();
        assert_eq!(
            stack.pop_raw(0x37e, &mut status),
            Err(Fault::StackUnderflow)
        );
        assert_eq!(stack, before);
        assert_eq!(top(status), 0);
        assert_eq!(status & (IE | SF | ES | BUSY), IE | SF | ES | BUSY);
        let faulted = status;
        assert_eq!(
            stack.push_raw(0x37e, &mut status, [0; 10]),
            Err(Fault::PendingUnmasked)
        );
        assert_eq!(status, faulted);
        assert_eq!(stack, before);
        clear_exceptions(&mut status);
        assert_eq!(status, 0x4500);
        for _ in 0..8 {
            stack.push_raw(0x37e, &mut status, [0; 10]).unwrap();
        }
        let full = stack.clone();
        let before_top = top(status);
        assert_eq!(
            stack.push_raw(0x37e, &mut status, [1; 10]),
            Err(Fault::StackOverflow)
        );
        assert_eq!(stack, full);
        assert_eq!(top(status), before_top);
        assert_ne!(status & C1, 0);
    }
}
