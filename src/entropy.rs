use blake3::Hasher;
use tiny_keccak::{Hasher as K12Hasher, KangarooTwelve};

#[derive(Debug)]
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        let seed = if seed == 0 { 0x9e3779b97f4a7c15 } else { seed };
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_bytes(&mut self, out: &mut [u8]) {
        for chunk in out.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let len = chunk.len();
            chunk.copy_from_slice(&v[..len]);
        }
    }
}

pub fn commit_digest(bits: &[u8; 512]) -> [u8; 32] {
    let mut hasher = Hasher::new();
    hasher.update(bits);
    hasher.finalize().into()
}

pub fn seed_to_subseed(seed: &str) -> Result<[u8; 32], String> {
    if seed.len() != 55 {
        return Err("seed must be 55 characters".to_string());
    }
    let mut seed_bytes = [0u8; 55];
    for (idx, b) in seed.as_bytes().iter().copied().enumerate() {
        if !(b'a'..=b'z').contains(&b) {
            return Err("seed must contain only a-z characters".to_string());
        }
        seed_bytes[idx] = b - b'a';
    }

    let mut hasher = KangarooTwelve::new(b"");
    hasher.update(&seed_bytes);
    let mut out = [0u8; 32];
    hasher.finalize(&mut out);
    Ok(out)
}

pub fn seed_to_rng_seed(seed: &str) -> Result<u64, String> {
    let subseed = seed_to_subseed(seed)?;
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&subseed[..8]);
    Ok(u64::from_le_bytes(bytes))
}
