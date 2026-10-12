use std::cmp::Ordering;
#[derive(Clone)]
pub(super) struct Big(pub Vec<u32>);
impl Big {
    pub fn new(v: u32) -> Self {
        Self(if v == 0 { vec![] } else { vec![v] })
    }
    pub fn bits(&self) -> usize {
        self.0.last().map_or(0, |v| {
            (self.0.len() - 1) * 32 + 32 - v.leading_zeros() as usize
        })
    }
    pub fn small(&mut self, m: u32, a: u32) {
        let mut c = a as u64;
        for x in &mut self.0 {
            c += *x as u64 * m as u64;
            *x = c as u32;
            c >>= 32;
        }
        if c != 0 {
            self.0.push(c as u32);
        }
    }
    pub fn shl(&self, n: usize) -> Self {
        if self.bits() == 0 {
            return Self::new(0);
        }
        let mut v = vec![0; n / 32];
        let mut c = 0u64;
        for &x in &self.0 {
            let z = ((x as u64) << (n % 32)) | c;
            v.push(z as u32);
            c = z >> 32;
        }
        if c != 0 {
            v.push(c as u32)
        }
        Self(v)
    }
    pub fn cmp(&self, b: &Self) -> Ordering {
        self.0
            .len()
            .cmp(&b.0.len())
            .then_with(|| self.0.iter().rev().cmp(b.0.iter().rev()))
    }
    pub fn sub(&mut self, b: &Self) {
        let mut borrow = 0u64;
        for i in 0..self.0.len() {
            let rhs = b.0.get(i).copied().unwrap_or(0) as u64 + borrow;
            let lhs = self.0[i] as u64;
            self.0[i] = lhs.wrapping_sub(rhs) as u32;
            borrow = u64::from(lhs < rhs);
        }
        assert_eq!(borrow, 0);
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    pub fn divide_small_quotient(mut self, d: &Self) -> (u64, Self) {
        let mut q = 0;
        if self.cmp(d).is_lt() {
            return (q, self);
        }
        let shift = self.bits() - d.bits();
        assert!(shift < 64);
        for i in (0..=shift).rev() {
            let b = d.shl(i);
            if !self.cmp(&b).is_lt() {
                self.sub(&b);
                q |= 1 << i;
            }
        }
        (q, self)
    }
}
