use rand_core::{OsRng, RngCore};
use tiny_keccak::{Hasher as K12Hasher, IntoXof, KangarooTwelve, Xof};

pub fn fill_secure_bits(out: &mut [u8; 512]) {
    OsRng.fill_bytes(out);
}

pub fn commit_digest(bits: &[u8; 512]) -> [u8; 32] {
    let mut hasher = KangarooTwelve::new(b"");
    hasher.update(bits);
    let mut out = [0u8; 32];
    hasher.into_xof().squeeze(&mut out);
    out
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
