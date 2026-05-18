//! Git object id: a 20-byte SHA-1 (§4 — SHA-1/sha1dc for stock-git
//! compatibility). The hash value is byte-identical to what stock git
//! produces, which is what makes the materialized repo `git`-coherent.

use crate::error::{CspError, CspResult};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Oid(pub [u8; 20]);

impl Oid {
    pub fn from_hex(s: &str) -> CspResult<Oid> {
        let bytes =
            hex::decode(s).map_err(|e| CspError::Malformed(format!("bad oid hex {s}: {e}")))?;
        if bytes.len() != 20 {
            return Err(CspError::Malformed(format!("oid must be 20 bytes, got {}", bytes.len())));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&bytes);
        Ok(Oid(a))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn as_bytes(&self) -> &[u8; 20] {
        &self.0
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.to_hex())
    }
}
