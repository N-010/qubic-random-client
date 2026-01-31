use tiny_keccak::{Hasher, IntoXof, KangarooTwelve, Xof};

use crate::four_q::ops::ecc_mul_fixed;
use crate::four_q::types::PointAffine;

#[derive(Debug, Clone, Copy)]
pub struct QubicId(pub [u8; 32]);

impl QubicId {
    pub fn get_identity(&self) -> String {
        let mut identity = [0u8; 60];
        for i in 0..4 {
            let mut fragment =
                u64::from_le_bytes(self.0[i << 3..(i << 3) + 8].try_into().unwrap());
            for j in 0..14 {
                identity[i * 14 + j] = (fragment % 26) as u8 + b'A';
                fragment /= 26;
            }
        }

        let mut checksum = [0u8; 3];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(&self.0);
        kg.into_xof().squeeze(&mut checksum);

        let mut checksum =
            (checksum[0] as u64) | ((checksum[1] as u64) << 8) | ((checksum[2] as u64) << 16);
        checksum &= 0x3FFFF;
        for i in 0..4 {
            identity[56 + i] = (checksum % 26) as u8 + b'A';
            checksum /= 26;
        }

        String::from_utf8(identity.to_vec()).unwrap()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QubicWallet {
    private_key: [u8; 32],
    subseed: [u8; 32],
    pub public_key: QubicId,
}

impl QubicWallet {
    pub fn from_seed(seed: &str) -> Result<Self, String> {
        let subseed = Self::get_subseed(seed)?;
        let private_key = Self::get_private_key(&subseed);
        let public_key = Self::get_public_key(&private_key);

        Ok(Self {
            private_key,
            subseed,
            public_key: QubicId(public_key),
        })
    }

    pub fn get_subseed(seed: &str) -> Result<[u8; 32], String> {
        if seed.len() != 55 {
            return Err("seed must be 55 characters".to_string());
        }
        if !seed.chars().all(|c| c.is_ascii_lowercase()) {
            return Err("seed must contain only a-z characters".to_string());
        }

        let mut seed_bytes = [0u8; 55];
        for (i, b) in seed.as_bytes().iter().copied().enumerate() {
            seed_bytes[i] = b - b'a';
        }

        let mut subseed = [0u8; 32];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(&seed_bytes);
        kg.into_xof().squeeze(&mut subseed);
        Ok(subseed)
    }

    pub fn get_private_key(subseed: &[u8; 32]) -> [u8; 32] {
        let mut pk = [0u8; 32];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(subseed);
        kg.into_xof().squeeze(&mut pk);
        pk
    }

    pub fn get_public_key(private_key: &[u8; 32]) -> [u8; 32] {
        let mut p = PointAffine::default();
        let scalar: [u64; 4] = [
            u64::from_le_bytes(private_key[0..8].try_into().unwrap()),
            u64::from_le_bytes(private_key[8..16].try_into().unwrap()),
            u64::from_le_bytes(private_key[16..24].try_into().unwrap()),
            u64::from_le_bytes(private_key[24..32].try_into().unwrap()),
        ];
        ecc_mul_fixed(&scalar, &mut p);

        let mut encoded = *private_key;
        crate::four_q::ops::encode(&mut p, &mut encoded);
        encoded
    }

    pub fn get_identity(&self) -> String {
        self.public_key.get_identity()
    }
}
