//! Integer quantities used by gdtoa's refinement loop (no floating arithmetic).
//! Derived from David M. Gay's gdtoa; see GDTOA-LICENSE.
use super::big::Big;
#[derive(Clone)]
pub struct Integer(Big);
impl Integer {
    pub fn one() -> Self {
        Self(Big::new(1))
    }
    pub fn from_u64(v: u64) -> Self {
        let mut b = Big::new((v >> 32) as u32);
        b = b.shl(32);
        b.small(1, v as u32);
        Self(b)
    }
    pub fn decimal(digits: &[u8]) -> Self {
        let mut b = Big::new(0);
        for d in digits {
            b.small(10, u32::from(d - b'0'));
        }
        Self(b)
    }
    pub fn bits(&self) -> usize {
        self.0.bits()
    }
    pub fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
    pub fn shift(&self, n: usize) -> Self {
        Self(self.0.shl(n))
    }
    pub fn pow5(&self, n: usize) -> Self {
        let mut b = self.0.clone();
        for _ in 0..n {
            b.small(5, 0)
        }
        Self(b)
    }
    pub fn multiply(&self, other: &Self) -> Self {
        let mut out = vec![0u32; self.0 .0.len() + other.0 .0.len()];
        for (i, a) in self.0 .0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, b) in other.0 .0.iter().enumerate() {
                let n = u64::from(*a) * u64::from(*b) + u64::from(out[i + j]) + carry;
                out[i + j] = n as u32;
                carry = n >> 32;
            }
            out[i + other.0 .0.len()] = carry as u32;
        }
        while out.last() == Some(&0) {
            out.pop();
        }
        Self(Big(out))
    }
    pub fn difference(&self, other: &Self) -> (Self, bool) {
        let negative = self.cmp(other).is_lt();
        let (mut a, b) = if negative {
            (other.0.clone(), &self.0)
        } else {
            (self.0.clone(), &other.0)
        };
        a.sub(b);
        (Self(a), negative)
    }
    /// b2d truncates (does not round) to 53 leading bits, with exponent 0.
    pub fn leading_binary64(&self) -> u64 {
        assert!(self.bits() != 0);
        let bits = self.bits();
        let shift = bits.saturating_sub(53);
        let mut q = 0u64;
        for i in (shift..bits).rev() {
            q = (q << 1) | u64::from((self.0 .0[i / 32] >> (i % 32)) & 1);
        }
        if bits < 53 {
            q <<= 53 - bits;
        }
        0x3ff0_0000_0000_0000 | (q & ((1 << 52) - 1))
    }
    #[allow(dead_code)]
    pub fn hex(&self) -> String {
        if self.bits() == 0 {
            return "0".into();
        }
        let mut s = String::new();
        for (i, x) in self.0 .0.iter().rev().enumerate() {
            if i == 0 {
                s += &format!("{x:x}")
            } else {
                s += &format!("{x:08x}")
            }
        }
        s
    }
}
pub struct Delta {
    pub delta: Integer,
    pub unit: Integer,
    pub below: bool,
    pub lsb: u64,
}
/// strtod.c bb/bd/bs construction through diff(bb,bd), including scaled ULP.
pub fn delta(coefficient: &Integer, exponent: i32, rv: u64, scale: i32, directed: bool) -> Delta {
    let encoded = ((rv >> 52) & 2047) as i32;
    let mut fraction = rv & ((1 << 52) - 1);
    if encoded != 0 {
        fraction |= 1 << 52;
    }
    let trailing = fraction.trailing_zeros();
    fraction = fraction.checked_shr(trailing).unwrap_or(0);
    let bbe = if encoded == 0 { -1074 } else { encoded - 1075 } + trailing as i32;
    let bbbits = 64 - fraction.leading_zeros() as i32;
    let (mut bb2, bb5, mut bd2, bd5) = if exponent >= 0 {
        (0, 0, exponent, exponent)
    } else {
        (-exponent, -exponent, 0, 0)
    };
    if bbe >= 0 {
        bb2 += bbe;
    } else {
        bd2 -= bbe;
    }
    let mut bs2 = bb2 + i32::from(directed);
    let log = bbe - scale + bbbits - 1;
    let mut j = 54 - bbbits;
    let mut lsb = 1u64;
    if log < -1022 {
        let i = -1022 - log;
        j -= i;
        lsb = if i < 32 {
            u64::from(1u32.wrapping_shl(i as u32))
        } else {
            (u64::from(1u32.wrapping_shl((i - 32) as u32)) << 32) | 1
        };
    }
    bb2 += j;
    bd2 += j + scale;
    let common = bb2.min(bd2).min(bs2);
    if common > 0 {
        bb2 -= common;
        bd2 -= common;
        bs2 -= common;
    }
    let mut unit = Integer::one();
    let mut bb = Integer::from_u64(fraction);
    if bb5 > 0 {
        unit = unit.pow5(bb5 as usize);
        bb = unit.multiply(&bb);
    }
    if bb2 > 0 {
        bb = bb.shift(bb2 as usize);
    }
    let mut bd = coefficient.pow5(bd5 as usize);
    if bd2 > 0 {
        bd = bd.shift(bd2 as usize);
    }
    if bs2 > 0 {
        unit = unit.shift(bs2 as usize);
    }
    let (delta, below) = bb.difference(&bd);
    Delta {
        delta,
        unit,
        below,
        lsb,
    }
}
