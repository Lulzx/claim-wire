//! Provenance Lockfile schema (`.claimwire/evidence.lock.json`, `spec.md` §8.1).

use anyhow::Result;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLockfile {
    pub schema: String,
    #[serde(default)]
    pub producers: IndexMap<String, LockedProducer>,
    #[serde(default)]
    pub evaluated_claims: IndexMap<String, LockedClaimValue>,
    #[serde(default)]
    pub locations: IndexMap<String, LockedLocation>,
}

impl Default for EvidenceLockfile {
    fn default() -> Self {
        Self {
            schema: "claimwire-lock/v3".to_string(),
            producers: IndexMap::new(),
            evaluated_claims: IndexMap::new(),
            locations: IndexMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedProducer {
    pub epochs: IndexMap<String, LockedEpoch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedEpoch {
    pub run_id: String,
    pub git_commit: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub git_dirty: bool,
    pub input_set_digest: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LockedClaimValue {
    pub value: serde_json::Value,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedLocation {
    pub rendered: String,
    pub rendered_digest: String,
    pub context_digest: String,
}

impl EvidenceLockfile {
    pub fn load(workspace_root: &Path) -> Option<Self> {
        let path = workspace_root.join(".claimwire/evidence.lock.json");
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub fn to_pretty_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }
}
