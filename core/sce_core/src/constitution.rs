use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constitution {
    pub constitution_version: String,
    pub project: Option<String>,
    pub intent: Option<serde_yaml::Value>,
    pub audio_contract: Option<serde_yaml::Value>,
}

pub fn load_constitution(path: &Path) -> anyhow::Result<Constitution> {
    let text = std::fs::read_to_string(path)?;
    let constitution: Constitution = serde_yaml::from_str(&text)?;
    Ok(constitution)
}

pub fn load_constitution_and_hash(path: &Path) -> anyhow::Result<(Constitution, String)> {
    let bytes = std::fs::read(path)?;
    let hash = crate::util::sha256_bytes(&bytes);
    let constitution: Constitution = serde_yaml::from_slice(&bytes)?;
    Ok((constitution, hash))
}
