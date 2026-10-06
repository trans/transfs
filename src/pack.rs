//! MCHPACK2 packs from merkle-champ (its FORMAT.md, section 11), with errors
//! mapped to transfs's.
use crate::{Error, Result};
use merkle_champ::{Identity, Objects};

pub use merkle_champ::pack::Pack;

/// A pack of `objects` rooted at `roots`. Every object must be reachable from
/// the roots.
pub fn encode(roots: &[Identity], objects: &Objects) -> Result<Vec<u8>> {
    merkle_champ::pack::encode(roots, objects).map_err(|e| Error::Storage(e.to_string()))
}

/// Decode and fully verify a pack.
pub fn decode(bytes: &[u8]) -> Result<Pack> {
    merkle_champ::pack::decode(bytes).map_err(|e| Error::Storage(e.to_string()))
}
