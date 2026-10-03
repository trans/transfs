//! Compatibility wrapper for the shared merkle-champ pack layer.
use crate::{Error, Result};
use merkle_champ::Identity;

pub fn encode(objects: &[(Identity, Vec<u8>)]) -> Result<Vec<u8>> {
    merkle_champ_pack::encode(objects).map_err(|e| Error::Storage(e.to_string()))
}

pub fn decode(bytes: &[u8]) -> Result<Vec<(Identity, Vec<u8>)>> {
    merkle_champ_pack::decode(bytes).map_err(|e| Error::Storage(e.to_string()))
}
