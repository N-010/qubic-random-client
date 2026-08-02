use std::fmt;

pub const RANDOM_CONTRACT_INDEX: u32 = 3;
pub const REVEAL_AND_COMMIT_PROCEDURE: u16 = 1;
pub const GET_PROVIDER_STATUS_FUNCTION: u16 = 2;
pub const PROVIDER_STATUS_SIZE: usize = 680;
pub const MAX_PROVIDER_SLOTS: usize = 32;

const STREAMS_OFFSET: usize = 4;
const TIERS_OFFSET: usize = 132;
const COLLATERAL_OFFSET: usize = 264;
const CONTRIBUTED_OFFSET: usize = 520;
const LAST_UPDATE_OFFSET: usize = 552;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotKey {
    pub stream: u8,
    pub collateral_tier: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSlot {
    pub key: SlotKey,
    pub locked_collateral: u64,
    pub contributed_to_entropy: bool,
    pub last_update_tick: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStatus {
    pub slots: Vec<ProviderSlot>,
}

impl ProviderStatus {
    pub fn decode(bytes: &[u8]) -> Result<Self, ContractCodecError> {
        if bytes.len() != PROVIDER_STATUS_SIZE {
            return Err(ContractCodecError(format!(
                "provider status must be {PROVIDER_STATUS_SIZE} bytes, got {}",
                bytes.len()
            )));
        }
        let count = read_u32(bytes, 0)? as usize;
        if count > MAX_PROVIDER_SLOTS {
            return Err(ContractCodecError(format!(
                "provider status count exceeds {MAX_PROVIDER_SLOTS}: {count}"
            )));
        }

        let mut slots = Vec::with_capacity(count);
        for index in 0..count {
            let stream = read_u32(bytes, STREAMS_OFFSET + index * 4)?;
            let tier = read_u32(bytes, TIERS_OFFSET + index * 4)?;
            let contributed = bytes[CONTRIBUTED_OFFSET + index];
            if stream > 2 {
                return Err(ContractCodecError(format!(
                    "slot {index} has invalid stream {stream}"
                )));
            }
            if tier > 9 {
                return Err(ContractCodecError(format!(
                    "slot {index} has invalid collateral tier {tier}"
                )));
            }
            if contributed > 1 {
                return Err(ContractCodecError(format!(
                    "slot {index} has invalid contribution flag {contributed}"
                )));
            }
            slots.push(ProviderSlot {
                key: SlotKey {
                    stream: stream as u8,
                    collateral_tier: tier as u8,
                },
                locked_collateral: read_u64(bytes, COLLATERAL_OFFSET + index * 8)?,
                contributed_to_entropy: contributed == 1,
                last_update_tick: read_u32(bytes, LAST_UPDATE_OFFSET + index * 4)?,
            });
        }
        Ok(Self { slots })
    }

    pub fn slot(&self, key: SlotKey) -> Option<&ProviderSlot> {
        self.slots.iter().find(|slot| slot.key == key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevealAndCommitInput {
    pub reveal: [u8; 512],
    pub commit: [u8; 32],
}

impl RevealAndCommitInput {
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(544);
        output.extend_from_slice(&self.reveal);
        output.extend_from_slice(&self.commit);
        output
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractCodecError(String);

impl fmt::Display for ContractCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ContractCodecError {}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ContractCodecError> {
    bytes
        .get(offset..offset + 4)
        .and_then(|value| value.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| ContractCodecError(format!("missing uint32 at offset {offset}")))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, ContractCodecError> {
    bytes
        .get(offset..offset + 8)
        .and_then(|value| value.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or_else(|| ContractCodecError(format!("missing uint64 at offset {offset}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn encode_status(slots: &[ProviderSlot]) -> [u8; PROVIDER_STATUS_SIZE] {
        let mut bytes = [0; PROVIDER_STATUS_SIZE];
        bytes[..4].copy_from_slice(&(slots.len() as u32).to_le_bytes());
        for (index, slot) in slots.iter().enumerate() {
            bytes[STREAMS_OFFSET + index * 4..STREAMS_OFFSET + index * 4 + 4]
                .copy_from_slice(&u32::from(slot.key.stream).to_le_bytes());
            bytes[TIERS_OFFSET + index * 4..TIERS_OFFSET + index * 4 + 4]
                .copy_from_slice(&u32::from(slot.key.collateral_tier).to_le_bytes());
            bytes[COLLATERAL_OFFSET + index * 8..COLLATERAL_OFFSET + index * 8 + 8]
                .copy_from_slice(&slot.locked_collateral.to_le_bytes());
            bytes[CONTRIBUTED_OFFSET + index] = u8::from(slot.contributed_to_entropy);
            bytes[LAST_UPDATE_OFFSET + index * 4..LAST_UPDATE_OFFSET + index * 4 + 4]
                .copy_from_slice(&slot.last_update_tick.to_le_bytes());
        }
        bytes
    }

    prop_compose! {
        fn provider_slot_strategy()(
            stream in 0u8..=2,
            collateral_tier in 0u8..=9,
            locked_collateral in any::<u64>(),
            contributed_to_entropy in any::<bool>(),
            last_update_tick in any::<u32>(),
        ) -> ProviderSlot {
            ProviderSlot {
                key: SlotKey { stream, collateral_tier },
                locked_collateral,
                contributed_to_entropy,
                last_update_tick,
            }
        }
    }

    proptest! {
        #[test]
        fn valid_layout_roundtrips(
            slots in prop::collection::vec(provider_slot_strategy(), 0..=MAX_PROVIDER_SLOTS)
        ) {
            let decoded = ProviderStatus::decode(&encode_status(&slots)).unwrap();
            prop_assert_eq!(decoded, ProviderStatus { slots });
        }

        #[test]
        fn every_wrong_size_is_rejected(size in 0usize..1000) {
            prop_assume!(size != PROVIDER_STATUS_SIZE);
            prop_assert!(ProviderStatus::decode(&vec![0; size]).is_err());
        }
    }

    #[test]
    fn uses_qpi_padding_before_u64_array() {
        let slot = ProviderSlot {
            key: SlotKey {
                stream: 2,
                collateral_tier: 9,
            },
            locked_collateral: 0x0102_0304_0506_0708,
            contributed_to_entropy: true,
            last_update_tick: 42,
        };
        let bytes = encode_status(std::slice::from_ref(&slot));

        assert_eq!(&bytes[260..264], &[0; 4]);
        assert_eq!(
            ProviderStatus::decode(&bytes).unwrap(),
            ProviderStatus { slots: vec![slot] }
        );
    }

    #[test]
    fn rejects_invalid_ranges() {
        let mut bytes = [0; PROVIDER_STATUS_SIZE];
        bytes[..4].copy_from_slice(&1u32.to_le_bytes());
        bytes[STREAMS_OFFSET..STREAMS_OFFSET + 4].copy_from_slice(&3u32.to_le_bytes());
        assert!(
            ProviderStatus::decode(&bytes)
                .unwrap_err()
                .to_string()
                .contains("stream")
        );
    }
}
