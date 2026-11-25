//! Deterministic data generation: keys, values, request distributions.

/// `SplitMix64`: small, fast, deterministic; every run of a profile sees the same data.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`).
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box–Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(f64::MIN_POSITIVE);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Key of record `i`: 8 bytes of hashed prefix then the index, so inserts land at random
/// positions of every ordered index (the common case for UUID-like identifiers).
#[must_use]
pub fn key(i: u64) -> [u8; 16] {
    let mut h = i ^ 0x5851_F42D_4C95_7F2D;
    h = (h ^ (h >> 33)).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
    h = (h ^ (h >> 33)).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
    h ^= h >> 33;
    let mut k = [0u8; 16];
    k[..8].copy_from_slice(&h.to_be_bytes());
    k[8..].copy_from_slice(&i.to_be_bytes());
    k
}

/// Value of record `i`: `size` bytes, compressible like real payloads (not all zeros, not random).
#[must_use]
pub fn value(i: u64, size: usize) -> Vec<u8> {
    let mut rng = Rng::new(i.wrapping_mul(0x2545_F491_4F6C_DD1D));
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789 ,.:{}\"";
    (0..size).map(|_| alphabet[rng.below(alphabet.len() as u64) as usize]).collect()
}

/// YCSB scrambled Zipfian generator over `0..n` (Gray et al., theta = 0.99).
#[derive(Debug, Clone)]
pub struct Zipf {
    n: u64,
    theta: f64,
    alpha: f64,
    zetan: f64,
    eta: f64,
}

impl Zipf {
    #[must_use]
    pub fn new(n: u64) -> Self {
        let theta = 0.99;
        let zeta = |count: u64| (1..=count).map(|i| 1.0 / (i as f64).powf(theta)).sum::<f64>();
        let zetan = zeta(n);
        let zeta2 = zeta(2);
        Self {
            n,
            theta,
            alpha: 1.0 / (1.0 - theta),
            zetan,
            eta: (1.0 - (2.0 / n as f64).powf(1.0 - theta)) / (1.0 - zeta2 / zetan),
        }
    }

    /// Next rank in `0..n`, hottest first; callers scramble it with [`scramble`].
    pub fn next(&mut self, rng: &mut Rng) -> u64 {
        let u = rng.unit();
        let uz = u * self.zetan;
        if uz < 1.0 {
            return 0;
        }
        if uz < 1.0 + 0.5f64.powf(self.theta) {
            return 1;
        }
        let rank = (self.n as f64 * (self.eta * u - self.eta + 1.0).powf(self.alpha)) as u64;
        rank.min(self.n - 1)
    }
}

/// Spreads Zipfian ranks over the key space so hot keys are not adjacent.
#[must_use]
pub fn scramble(rank: u64, n: u64) -> u64 {
    let mut h = rank.wrapping_mul(0xCBF2_9CE4_8422_2325) ^ 0x1000_0000_01B3;
    h ^= h >> 29;
    h % n
}
