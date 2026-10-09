//! Deterministic PRNG (xorshift64*) for fixture modes.
//!
//! Every mode's output must be a pure function of its arguments, so the
//! fixture carries its own generator instead of depending on `rand`
//! (`06-verification.md` §2: deterministic byte patterns / seeded variation).

/// xorshift64* — small, fast, deterministic across platforms. NOT
/// cryptographically secure; it only needs reproducibility.
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    /// A zero seed is remapped to a fixed nonzero constant (xorshift needs a
    /// nonzero state).
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Fill `buf` from the generator stream (little-endian u64 words).
    pub fn fill(&mut self, buf: &mut [u8]) {
        let (words, rest) = buf.as_chunks_mut::<8>();
        for word in words {
            word.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        if !rest.is_empty() {
            let bytes = self.next_u64().to_le_bytes();
            rest.copy_from_slice(&bytes[..rest.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream_and_zero_seed_is_stable() {
        let mut a = XorShift64::new(42);
        let mut b = XorShift64::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let mut c = XorShift64::new(0);
        assert_ne!(c.next_u64(), 0);
        // No all-zero output windows for a nonzero fill.
        let mut buf = [0u8; 33]; // intentionally not word-aligned
        XorShift64::new(7).fill(&mut buf);
        assert!(buf.iter().any(|b| *b != 0));
    }

    #[test]
    fn different_seeds_diverge() {
        assert_ne!(XorShift64::new(1).next_u64(), XorShift64::new(2).next_u64());
    }
}
