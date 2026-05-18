//! The strict total order (§5.1). `(counter, NodeId, commitSHA)` — compare
//! counter, then NodeId bytewise, then commit SHA bytewise. The SHA
//! tiebreaker makes this a *true* total order with no ties ever, even under
//! same-NodeId concurrency (§5.1). It is the sole basis for fold order and
//! conflict tiebreak and is never wall-clock dependent.

use crate::oid::Oid;
use serde::{Deserialize, Serialize};

/// A node's stable identity: the 32-byte ed25519 public key (§5.1, §10).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub [u8; 32]);

impl NodeId {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
    pub fn from_hex(s: &str) -> Option<NodeId> {
        let v = hex::decode(s).ok()?;
        if v.len() != 32 {
            return None;
        }
        let mut a = [0u8; 32];
        a.copy_from_slice(&v);
        Some(NodeId(a))
    }
}

impl std::fmt::Debug for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NodeId({}…)", &self.to_hex()[..12])
    }
}

/// The strict-total-order key for a primitive commit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OrderKey {
    pub counter: u64,
    pub node: NodeId,
    pub sha: Oid,
}

impl PartialOrd for OrderKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.counter
            .cmp(&other.counter)
            .then(self.node.0.cmp(&other.node.0))
            .then(self.sha.0.cmp(&other.sha.0))
    }
}
