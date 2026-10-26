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
    fn commit_digest_matches_core_512_byte_vectors() {
        // Core 98102e74d50db4c21473ef3e9ad21cdca6fd69aa, KangarooTwelve(input, 512, out, 32).
        let cases = [
            (
                [0; 512],
                "38985b291880374b4f65e49d89c4dab74662e5e825f17d453928e3e3214e510c",
            ),
            (
                [255; 512],
                "5aa2de31d540be38274c10dcfbc68f1f3767f59c54b0294582058365e2efdb32",
            ),
            (
                std::array::from_fn(|i| i as u8),
                "e170854728928b1551a2ddedcd39e453605ef6fb4c8020c5d3efd2bfc4001a6e",
            ),
        ];
        for (input, expected_hex) in cases {
            let expected: [u8; 32] = std::array::from_fn(|i| {
                u8::from_str_radix(&expected_hex[i * 2..i * 2 + 2], 16).unwrap()
            });
            assert_eq!(commit_digest(&input), expected);
        }
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
