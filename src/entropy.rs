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

#[cfg(test)]
mod tests {
    use super::{commit_digest, fill_secure_bits};

    #[test]
    fn commit_digest_is_deterministic() {
        // Same input bytes must yield the same 32-byte digest.
        let mut bits = [0u8; 512];
        bits[0] = 1;
        let first = commit_digest(&bits);
        let second = commit_digest(&bits);
        assert_eq!(first, second);
        assert_eq!(first.len(), 32);
    }

    #[test]
    fn fill_secure_bits_writes_data() {
        // OS RNG should produce some non-zero bytes.
        let mut bits = [0u8; 512];
        fill_secure_bits(&mut bits);
        let has_non_zero = bits.iter().any(|b| *b != 0);
        assert!(has_non_zero);
    }
}
