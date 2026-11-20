//! Reads the live session registry Claude Code maintains in `~/.claude/sessions/<pid>.json`.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct RegEntry {
    pub pid: u32,
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub started_at: u64,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub name_source: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub status_updated_at: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<u64>,
    /// The desktop app's `local_…` id for sessions it hosts.
    #[serde(default)]
    pub host_session_id: Option<String>,
}

pub fn read(dir: &Path) -> Vec<RegEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        if let Ok(e) = serde_json::from_slice::<RegEntry>(&bytes) {
            out.push(e);
        }
    }
    out
}
