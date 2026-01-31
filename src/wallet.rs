use tiny_keccak::{Hasher, IntoXof, KangarooTwelve, Xof};

use crate::four_q::consts::{CURVE_ORDER_0, CURVE_ORDER_1, CURVE_ORDER_2, CURVE_ORDER_3, MONTGOMERY_R_PRIME, ONE};
use crate::four_q::ops::{
    addcarry_u64, ecc_mul_fixed, encode, montgomery_multiply_mod_order, subborrow_u64,
};
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

#[derive(Debug, Clone, Copy)]
pub struct Signature(pub [u8; 64]);

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

    pub fn sign_bytes(&self, message: &[u8]) -> Signature {
        let mut digest = [0u8; 32];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(message);
        kg.into_xof().squeeze(&mut digest);
        self.sign_raw(digest)
    }

    pub fn sign_raw(&self, message_digest: [u8; 32]) -> Signature {
        let mut k = [0u8; 64];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(&self.subseed);
        kg.into_xof().squeeze(&mut k);

        let mut temp = [0u8; 96];
        temp[32..64].copy_from_slice(&k[32..64]);
        temp[64..96].copy_from_slice(&message_digest);

        let mut r_bytes = [0u8; 64];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(&temp[32..]);
        kg.into_xof().squeeze(&mut r_bytes);

        let mut r_point = PointAffine::default();
        let r_u64 = bytes_to_u64x8(&r_bytes);
        ecc_mul_fixed(&r_u64, &mut r_point);

        let mut signature = [0u8; 64];
        encode(&mut r_point, &mut signature);

        temp[0..32].copy_from_slice(&signature[0..32]);
        temp[32..64].copy_from_slice(&self.public_key.0);

        let mut h_bytes = [0u8; 64];
        let mut kg = KangarooTwelve::new(b"");
        kg.update(&temp);
        kg.into_xof().squeeze(&mut h_bytes);

        let k_u64 = bytes_to_u64x8(&k);
        let mut r_u64 = r_u64;
        let mut h_u64 = bytes_to_u64x8(&h_bytes);
        let mut signature_u64 = bytes_to_u64x8(&signature);

        montgomery_multiply_mod_order(&r_u64[0..4], &MONTGOMERY_R_PRIME, &mut r_u64[0..4]);
        let r_i = r_u64;
        montgomery_multiply_mod_order(&r_i[0..4], &ONE, &mut r_u64[0..4]);

        montgomery_multiply_mod_order(&h_u64[0..4], &MONTGOMERY_R_PRIME, &mut h_u64[0..4]);
        let h_i = h_u64;
        montgomery_multiply_mod_order(&h_i[0..4], &ONE, &mut h_u64[0..4]);

        montgomery_multiply_mod_order(&k_u64[0..4], &MONTGOMERY_R_PRIME, &mut signature_u64[4..8]);
        let h_i = h_u64;
        montgomery_multiply_mod_order(&h_i[0..4], &MONTGOMERY_R_PRIME, &mut h_u64[0..4]);
        let mut s_i = [0u64; 4];
        s_i.copy_from_slice(&signature_u64[4..8]);
        montgomery_multiply_mod_order(&s_i, &h_u64[0..4], &mut signature_u64[4..8]);
        s_i.copy_from_slice(&signature_u64[4..8]);
        montgomery_multiply_mod_order(&s_i, &ONE, &mut signature_u64[4..8]);

        let borrow = subborrow_u64(
            subborrow_u64(
                subborrow_u64(
                    subborrow_u64(0, r_u64[0], signature_u64[4], &mut signature_u64[4]),
                    r_u64[1],
                    signature_u64[5],
                    &mut signature_u64[5],
                ),
                r_u64[2],
                signature_u64[6],
                &mut signature_u64[6],
            ),
            r_u64[3],
            signature_u64[7],
            &mut signature_u64[7],
        );

        if borrow != 0 {
            addcarry_u64(
                addcarry_u64(
                    addcarry_u64(
                        addcarry_u64(0, signature_u64[4], CURVE_ORDER_0, &mut signature_u64[4]),
                        signature_u64[5],
                        CURVE_ORDER_1,
                        &mut signature_u64[5],
                    ),
                    signature_u64[6],
                    CURVE_ORDER_2,
                    &mut signature_u64[6],
                ),
                signature_u64[7],
                CURVE_ORDER_3,
                &mut signature_u64[7],
            );
        }

        signature = u64x8_to_bytes(&signature_u64);
        Signature(signature)
    }
}

fn bytes_to_u64x8(bytes: &[u8; 64]) -> [u64; 8] {
    [
        u64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
        u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
        u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
        u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
    ]
}

fn u64x8_to_bytes(words: &[u64; 8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    for (idx, word) in words.iter().enumerate() {
        out[idx * 8..(idx + 1) * 8].copy_from_slice(&word.to_le_bytes());
    }
    out
}
