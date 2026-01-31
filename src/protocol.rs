#[derive(Debug, Clone)]
pub struct RevealAndCommitInput {
    pub revealed_bits: [u8; 512],
    pub committed_digest: [u8; 32],
}
