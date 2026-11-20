//! Past sessions for the recent view: a light index of every transcript, joined
//! with the desktop app's metadata and grouped by project (worktrees fold into
//! their repo), the way tapestry's board does it.
//!
//! Transcripts can be hundreds of megabytes, so only the first and last
//! `CHUNK` bytes are read, and only when a file's size or mtime changed.

use crate::desktop::{DesktopIndex, DesktopSession, Pr};
use crate::util::{one_line, parse_rfc3339_ms};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

const CHUNK: u64 = 128 * 1024;

#[derive(Debug, Clone, Default)]
pub struct TranscriptInfo {
    pub session_id: String,
    pub cwd: Option<String>,
    pub entrypoint: Option<String>,
    pub branch: Option<String>,
    pub first_prompt: Option<String>,
    pub title: Option<String>,
    pub start_ms: u64,
    pub last_ms: u64,
}

#[derive(Debug, Clone)]
pub struct RecentSession {
    pub session_id: String,
    pub desktop_id: Option<String>,
    pub title: String,
    /// Where the session started: the desktop's origin folder, else the transcript's cwd.
    pub cwd: String,
    /// Where it works now; a worktree path for worktree sessions.
    pub work_cwd: String,
    pub branch: Option<String>,
    /// "desktop" for Code-tab sessions, otherwise the transcript's entrypoint.
    pub source: String,
    pub status: Option<String>,
    pub status_detail: Option<String>,
    pub archived: bool,
    pub needs_attention: bool,
    pub last_ms: u64,
    pub prs: Vec<Pr>,
}

#[derive(Debug, Clone)]
pub struct RecentProject {
    pub root: String,
    pub name: String,
    pub sessions: Vec<RecentSession>,
}

/// Strip `<repo>/.claude/worktrees/<name>` back to `<repo>`.
pub fn project_root(cwd: &str) -> String {
    match cwd.find("/.claude/worktrees/") {
        Some(i) => cwd[..i].to_string(),
        None => cwd.trim_end_matches('/').to_string(),
    }
}

/// The text a person typed, or None for tool results, injected context and slash-command wrappers.
fn prompt_text(o: &Value) -> Option<String> {
    if o.get("isMeta").and_then(Value::as_bool) == Some(true)
        || o.get("isSidechain").and_then(Value::as_bool) == Some(true)
        || o.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
        || o.get("toolUseResult").is_some()
    {
        return None;
    }
    let content = o.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    Some(one_line(text))
}

fn ingest(info: &mut TranscriptInfo, buf: &[u8], head: bool) {
    for line in buf.split(|b| *b == b'\n') {
        if line.starts_with(br#"{"type":"file-history"#) || line.starts_with(br#"{"type":"api-request"#) {
            continue;
        }
        let Ok(o) = serde_json::from_slice::<Value>(line) else { continue };
        let kind = o.get("type").and_then(Value::as_str).unwrap_or("");
        let get = |k: &str| o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        match kind {
            "user" | "assistant" | "system" => {
                if let Some(ts) = get("timestamp").and_then(|t| parse_rfc3339_ms(&t)) {
                    if head && info.start_ms == 0 {
                        info.start_ms = ts;
                    }
                    info.last_ms = info.last_ms.max(ts);
                }
                if info.cwd.is_none() {
                    info.cwd = get("cwd");
                }
                if info.entrypoint.is_none() {
                    info.entrypoint = get("entrypoint");
                }
                if let Some(b) = get("gitBranch") {
                    info.branch = Some(b);
                }
                if kind == "user" && head && info.first_prompt.is_none() {
                    info.first_prompt = prompt_text(&o);
                }
            }
            "custom-title" => info.title = get("customTitle").or(info.title.take()),
            "ai-title" if info.title.is_none() => info.title = get("aiTitle"),
            _ => {}
        }
    }
}

/// :param path: Path a `<session>.jsonl`; :return: TranscriptInfo from its first and last 128 KiB
pub fn read_info(path: &Path) -> TranscriptInfo {
    let mut info = TranscriptInfo {
        session_id: path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string(),
        ..Default::default()
    };
    let Ok(mut f) = File::open(path) else { return info };
    let len = f.metadata().map_or(0, |m| m.len());
    let mut head = Vec::with_capacity(CHUNK.min(len) as usize);
    let _ = (&mut f).take(CHUNK).read_to_end(&mut head);
    ingest(&mut info, &head, true);
    if len > CHUNK {
        let start = len.saturating_sub(CHUNK).max(CHUNK);
        let mut tail = Vec::new();
        if f.seek(SeekFrom::Start(start)).is_ok() {
            let _ = f.read_to_end(&mut tail);
        }
        // The first tail line is usually cut in half; json parsing skips it.
        ingest(&mut info, &tail, false);
    }
    info
}

type StatKey = (SystemTime, u64);

/// Stat-keyed cache over `<projects>/*/*.jsonl`.
#[derive(Default)]
pub struct TranscriptIndex {
    files: HashMap<PathBuf, (StatKey, TranscriptInfo)>,
}

impl TranscriptIndex {
    pub fn refresh(&mut self, projects: &Path) {
        let mut next = HashMap::new();
        let Ok(rd) = std::fs::read_dir(projects) else {
            self.files = next;
            return;
        };
        for dir in rd.flatten() {
            let Ok(inner) = std::fs::read_dir(dir.path()) else { continue };
            for ent in inner.flatten() {
                let p = ent.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(meta) = ent.metadata() else { continue };
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let key = (mtime, meta.len());
                let entry = match self.files.remove(&p) {
                    Some((k, v)) if k == key => (k, v),
                    _ => {
                        let mut info = read_info(&p);
                        let mtime_ms = mtime.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                        info.last_ms = info.last_ms.max(mtime_ms);
                        (key, info)
                    }
                };
                next.insert(p, entry);
            }
        }
        self.files = next;
    }

    pub fn infos(&self) -> impl Iterator<Item = &TranscriptInfo> {
        self.files.values().map(|(_, v)| v)
    }
}

fn status_rank(s: &RecentSession) -> u8 {
    match (s.archived, s.status.as_deref()) {
        (true, _) => 9,
        (_, Some("blocked")) => 0,
        (_, Some("review_ready")) => 1,
        (_, Some("completed")) => 2,
        _ => 3,
    }
}

fn session_from(d: Option<&DesktopSession>, t: Option<&TranscriptInfo>) -> RecentSession {
    let session_id = d
        .and_then(|d| d.cli_id.clone())
        .or_else(|| t.map(|t| t.session_id.clone()))
        .or_else(|| d.map(|d| d.local_id.clone()))
        .unwrap_or_default();
    let title = d
        .and_then(|d| d.title.clone())
        .or_else(|| t.and_then(|t| t.title.clone()))
        .or_else(|| t.and_then(|t| t.first_prompt.clone()))
        .unwrap_or_else(|| session_id.chars().take(8).collect());
    let cwd = d
        .and_then(|d| d.origin_cwd.clone().or(d.cwd.clone()))
        .or_else(|| t.and_then(|t| t.cwd.clone()))
        .unwrap_or_default();
    let work_cwd = d.and_then(|d| d.cwd.clone()).or_else(|| t.and_then(|t| t.cwd.clone())).unwrap_or_else(|| cwd.clone());
    RecentSession {
        work_cwd,
        desktop_id: d.map(|d| d.local_id.clone()),
        title,
        cwd,
        branch: d.and_then(|d| d.branch.clone()).or_else(|| t.and_then(|t| t.branch.clone())),
        source: if d.is_some() { "desktop".into() } else { t.and_then(|t| t.entrypoint.clone()).unwrap_or_else(|| "cli".into()) },
        status: d.and_then(|d| d.status.clone()),
        status_detail: d.and_then(|d| d.status_detail.clone().or(d.needs_action.clone())),
        archived: d.is_some_and(|d| d.archived),
        needs_attention: d.is_some_and(|d| d.needs_attention()),
        last_ms: d.map_or(0, |d| d.last_activity_ms).max(t.map_or(0, |t| t.last_ms)),
        prs: d.map(|d| d.prs.clone()).unwrap_or_default(),
        session_id,
    }
}

/// Join transcripts with desktop metadata and group them into projects.
///
/// Headless Agent SDK runs (entrypoint `sdk-*`) are left out unless they have desktop metadata.
pub fn build(transcripts: &TranscriptIndex, desktop: &DesktopIndex) -> Vec<RecentProject> {
    let by_id: HashMap<&str, &TranscriptInfo> = transcripts.infos().map(|t| (t.session_id.as_str(), t)).collect();
    let mut used: HashSet<&str> = HashSet::new();
    let mut sessions = Vec::new();
    for d in desktop.sessions() {
        let ids: Vec<&str> = d.cli_id.iter().chain(d.prior_cli_ids.iter()).map(String::as_str).collect();
        let t = ids.iter().find_map(|i| by_id.get(i).copied());
        used.extend(ids);
        sessions.push(session_from(Some(d), t));
    }
    for t in transcripts.infos() {
        if used.contains(t.session_id.as_str()) || t.entrypoint.as_deref().is_some_and(|e| e.starts_with("sdk")) {
            continue;
        }
        sessions.push(session_from(None, Some(t)));
    }
    let mut projects: HashMap<String, Vec<RecentSession>> = HashMap::new();
    for s in sessions {
        let root = if s.cwd.is_empty() { "(unknown)".to_string() } else { project_root(&s.cwd) };
        projects.entry(root).or_default().push(s);
    }
    let mut out: Vec<RecentProject> = projects
        .into_iter()
        .map(|(root, mut sessions)| {
            sessions.sort_by(|a, b| status_rank(a).cmp(&status_rank(b)).then(b.last_ms.cmp(&a.last_ms)));
            let name = root.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or(&root).to_string();
            RecentProject { root, name, sessions }
        })
        .collect();
    let newest = |p: &RecentProject| p.sessions.iter().map(|s| s.last_ms).max().unwrap_or(0);
    let attention = |p: &RecentProject| p.sessions.iter().filter(|s| s.needs_attention).count();
    out.sort_by(|a, b| attention(b).cmp(&attention(a)).then(newest(b).cmp(&newest(a))));
    out
}

pub type Shared = Arc<Mutex<Option<Arc<Vec<RecentProject>>>>>;

/// Rebuild the recent list on its own thread every `every`, so the first full scan never stalls the live view.
pub fn spawn(projects: PathBuf, desktop_dir: PathBuf, every: Duration) -> Shared {
    let shared: Shared = Arc::default();
    let out = shared.clone();
    std::thread::Builder::new()
        .name("history".into())
        .spawn(move || {
            let mut transcripts = TranscriptIndex::default();
            let mut desktop = DesktopIndex::default();
            loop {
                transcripts.refresh(&projects);
                desktop.refresh(&desktop_dir);
                let built = Arc::new(build(&transcripts, &desktop));
                if let Ok(mut g) = out.lock() {
                    *g = Some(built);
                }
                std::thread::sleep(every);
            }
        })
        .expect("spawn history");
    shared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_title_prompt_and_cwd() {
        let dir = std::env::temp_dir().join(format!("weft-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sid.jsonl");
        std::fs::write(
            &p,
            concat!(
                r#"{"type":"user","timestamp":"2026-10-09T13:44:00.000Z","cwd":"/r/.claude/worktrees/w","entrypoint":"cli","message":{"content":"<command-name>x</command-name>"}}"#, "\n",
                r#"{"type":"user","timestamp":"2026-10-09T13:44:01.000Z","message":{"content":[{"type":"text","text":"fix  the\nbug"}]}}"#, "\n",
                r#"{"type":"ai-title","aiTitle":"Bug fix"}"#, "\n",
            ),
        )
        .unwrap();
        let info = read_info(&p);
        assert_eq!(info.first_prompt.as_deref(), Some("fix the bug"));
        assert_eq!(info.title.as_deref(), Some("Bug fix"));
        assert_eq!(project_root(info.cwd.as_deref().unwrap()), "/r");
        let _ = std::fs::remove_dir_all(dir);
    }
}
