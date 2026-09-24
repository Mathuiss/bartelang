//! A small pseudo-random number generator for `Rnd` / `Randomize`.
//!
//! Deliberately dependency-free: this is the successor to VB6's `Rnd`, which was
//! a weak linear congruential generator, and it is **not** suitable for anything
//! cryptographic.

use std::time::{SystemTime, UNIX_EPOCH};

/// xorshift64*, seeded from the clock, the process id and the address of a heap
/// allocation, so two runs started in the same nanosecond still differ.
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    pub fn seeded() -> Self {
        let mut rng = Rng { state: entropy() };
        // Discard the first few outputs; xorshift needs a moment to mix.
        for _ in 0..4 {
            rng.next_f64();
        }
        rng
    }

    pub fn reseed(&mut self) {
        self.state = entropy();
        for _ in 0..4 {
            self.next_f64();
        }
    }

    /// The next value in `[0, 1)`, matching the range of VB6's `Rnd`.
    pub fn next_f64(&mut self) -> f64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        let mixed = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // 53 bits, which is exactly a f64 mantissa.
        ((mixed >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

impl Default for Rng {
    fn default() -> Self {
        Self::seeded()
    }
}

fn entropy() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let heap = Box::new(0u8);
    let address = (&*heap as *const u8) as u64;
    let mixed = nanos ^ (address << 17) ^ ((std::process::id() as u64) << 41);
    if mixed == 0 { 0x9E37_79B9_7F4A_7C15 } else { mixed }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn values_stay_inside_the_unit_interval() {
        let mut rng = Rng::seeded();
        for _ in 0..10_000 {
            let value = rng.next_f64();
            assert!((0.0..1.0).contains(&value), "{value} is outside [0, 1)");
        }
    }

    #[test]
    fn values_are_not_all_the_same() {
        let mut rng = Rng::seeded();
        let seen: HashSet<u64> = (0..1_000).map(|_| (rng.next_f64() * 1e9) as u64).collect();
        assert!(seen.len() > 900, "only {} distinct values", seen.len());
    }

    #[test]
    fn two_generators_disagree() {
        let mut first = Rng::seeded();
        let mut second = Rng::seeded();
        // Reseeding from the same clock tick is possible, so compare sequences
        // rather than single values.
        let a: Vec<u64> = (0..8).map(|_| (first.next_f64() * 1e9) as u64).collect();
        let b: Vec<u64> = (0..8).map(|_| (second.next_f64() * 1e9) as u64).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn reseeding_restarts_the_sequence() {
        let mut rng = Rng::seeded();
        let first = rng.next_f64();
        rng.reseed();
        let after = rng.next_f64();
        assert!((0.0..1.0).contains(&after));
        assert_ne!(first, after);
    }
}
