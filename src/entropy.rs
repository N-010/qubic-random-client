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
