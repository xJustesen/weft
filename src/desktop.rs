//! Read-only reader for the Claude desktop app's per-session metadata
//! (`claude-code-sessions/<account>/<org>/local_*.json`).
//!
//! The format is internal and undocumented, so every field is optional and
//! unreadable files are skipped. Files are re-parsed only when their size or
//! modification time changes.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pr {
    pub url: String,
    pub number: Option<u64>,
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DesktopSession {
    /// The `local_…` id; the live registry's `hostSessionId` points at it.
    pub local_id: String,
    pub cli_id: Option<String>,
    pub prior_cli_ids: Vec<String>,
    pub title: Option<String>,
    pub archived: bool,
    pub created_ms: u64,
    pub last_activity_ms: u64,
    pub cwd: Option<String>,
    pub origin_cwd: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub completed_turns: Option<u64>,
    /// `postTurnSummary.status_category`: blocked, review_ready, completed, …
    pub status: Option<String>,
    pub status_detail: Option<String>,
    pub needs_action: Option<String>,
    pub prs: Vec<Pr>,
}

impl DesktopSession {
    /// :return: bool whether the desktop app's turn summary says this session waits on the user
    pub fn needs_attention(&self) -> bool {
        !self.archived && matches!(self.status.as_deref(), Some("blocked") | Some("review_ready"))
    }
}

/// :return: PathBuf the desktop app's session-metadata folder for this platform
pub fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        return home.join("Library/Application Support/Claude/claude-code-sessions");
    }
    if cfg!(windows) {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData/Roaming"));
        return appdata.join("Claude/claude-code-sessions");
    }
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"));
    config.join("Claude/claude-code-sessions")
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()).map(str::to_string)
}

/// :param bytes: &[u8] one metadata file; :return: Option[DesktopSession] None when it is not a session object
pub fn parse(bytes: &[u8], fallback_id: &str) -> Option<DesktopSession> {
    let d: Value = serde_json::from_slice(bytes).ok()?;
    if !d.is_object() {
        return None;
    }
    let summary = d.get("postTurnSummary").cloned().unwrap_or(Value::Null);
    let prs = d
        .get("prs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some(Pr {
                        url: s(p, "url")?,
                        number: p.get("prNumber").and_then(Value::as_u64),
                        state: s(p, "state"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Some(DesktopSession {
        local_id: s(&d, "sessionId").unwrap_or_else(|| fallback_id.to_string()),
        cli_id: s(&d, "cliSessionId"),
        prior_cli_ids: d
            .get("priorCliSessionIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        title: s(&d, "title"),
        archived: d.get("isArchived").and_then(Value::as_bool) == Some(true),
        created_ms: d.get("createdAt").and_then(Value::as_f64).map_or(0, |v| v as u64),
        last_activity_ms: d.get("lastActivityAt").and_then(Value::as_f64).map_or(0, |v| v as u64),
        cwd: s(&d, "cwd"),
        origin_cwd: s(&d, "originCwd"),
        branch: s(&d, "branch"),
        model: s(&d, "model"),
        completed_turns: d.get("completedTurns").and_then(Value::as_u64),
        status: s(&summary, "status_category"),
        status_detail: s(&summary, "status_detail"),
        needs_action: s(&summary, "needs_action"),
        prs,
    })
}

type StatKey = (SystemTime, u64);

/// Stat-keyed cache of every metadata file under the folder.
#[derive(Default)]
pub struct DesktopIndex {
    files: HashMap<PathBuf, (StatKey, Option<DesktopSession>)>,
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, StatKey)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        let Ok(meta) = ent.metadata() else { continue };
        if meta.is_dir() {
            walk(&p, out);
            continue;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with("local_") && name.ends_with(".json") {
            out.push((p, (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len())));
        }
    }
}

impl DesktopIndex {
    /// Re-read changed files and drop deleted ones.
    pub fn refresh(&mut self, dir: &Path) {
        let mut found = Vec::new();
        walk(dir, &mut found);
        let mut next = HashMap::with_capacity(found.len());
        for (p, key) in found {
            let entry = match self.files.remove(&p) {
                Some((k, v)) if k == key => (k, v),
                _ => {
                    let stem = p.file_stem().and_then(|x| x.to_str()).unwrap_or("").to_string();
                    let parsed = std::fs::read(&p).ok().and_then(|b| parse(&b, &stem));
                    (key, parsed)
                }
            };
            next.insert(p, entry);
        }
        self.files = next;
    }

    pub fn sessions(&self) -> impl Iterator<Item = &DesktopSession> {
        self.files.values().filter_map(|(_, v)| v.as_ref())
    }

    /// :return: HashMap[str, DesktopSession] keyed by local id and by every CLI session id it has had
    pub fn by_any_id(&self) -> HashMap<String, DesktopSession> {
        let mut out = HashMap::new();
        for d in self.sessions() {
            out.insert(d.local_id.clone(), d.clone());
            for id in d.cli_id.iter().chain(d.prior_cli_ids.iter()) {
                out.insert(id.clone(), d.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_prs_and_ids() {
        let raw = br#"{"sessionId":"local_abc","cliSessionId":"c2","priorCliSessionIds":["c1"],"title":"Fix it",
            "isArchived":false,"lastActivityAt":1700000000000,"postTurnSummary":{"status_category":"blocked","needs_action":"approve"},
            "prs":[{"url":"https://x/pull/3","prNumber":3,"state":"open"},{"nourl":1}]}"#;
        let d = parse(raw, "local_file").unwrap();
        assert_eq!(d.local_id, "local_abc");
        assert_eq!(d.prior_cli_ids, vec!["c1"]);
        assert_eq!(d.last_activity_ms, 1_700_000_000_000);
        assert!(d.needs_attention());
        assert_eq!(d.prs.len(), 1);
        assert_eq!(d.prs[0].number, Some(3));
    }
}
