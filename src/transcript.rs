//! Incremental tailer for a Claude Code transcript (`<session>.jsonl`).
//!
//! Only the bytes appended since the last poll are read. Lines are pre-filtered
//! with a SIMD substring search before any JSON is parsed, so multi-megabyte
//! transcripts full of file snapshots cost almost nothing.

use crate::util::{one_line, parse_rfc3339_ms, short_tool, tool_summary};
use memchr::{memchr, memmem};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct RunningTool {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub since_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Context currently held by the model = input + cache read + cache write of the latest message.
    pub ctx_tokens: u64,
    pub turns: u32,
    pub prompts: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub thinking_ms: u64,
    pub model: String,
    pub effort: Option<String>,
    pub git_branch: String,
    pub title: Option<String>,
    pub first_prompt: String,
    pub last_prompt: String,
    pub last_reply: String,
    pub last_activity_ms: u64,
    pub last_entry_was_user: bool,
    pub last_stop_reason: Option<String>,
    pub tools: HashMap<String, u32>,
    pub running: Vec<RunningTool>,
    pub last_tool: Option<(String, String)>,
    pub compactions: u32,
    // de-dup state for split assistant entries
    last_msg_id: Option<String>,
    last_usage: Usage,
}

impl Stats {
    pub fn input_total(&self) -> u64 {
        self.input_tokens + self.cache_read + self.cache_write
    }
    pub fn top_tools(&self, n: usize) -> Vec<(String, u32)> {
        let mut v: Vec<(String, u32)> = self.tools.iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
    Other(serde::de::IgnoredAny),
}

#[derive(Deserialize)]
struct Msg {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    content: Option<Content>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    message: Option<Msg>,
    #[serde(rename = "customTitle", default)]
    custom_title: Option<String>,
    #[serde(rename = "agentName", default)]
    agent_name: Option<String>,
    #[serde(rename = "lastPrompt", default)]
    last_prompt: Option<String>,
    #[serde(rename = "gitBranch", default)]
    git_branch: Option<String>,
    #[serde(default)]
    effort: Option<Value>,
    #[serde(rename = "thinkingDurationMs", default)]
    thinking_ms: Option<u64>,
    #[serde(default)]
    subtype: Option<String>,
}

const NEEDLES: &[&[u8]] = &[
    b"assistant\"",
    b"user\"",
    b"custom-title\"",
    b"agent-name\"",
    b"last-prompt\"",
    b"summary\"",
    b"system\"",
];

pub struct Tailer {
    pub path: PathBuf,
    offset: u64,
    partial: Vec<u8>,
    pub stats: Stats,
    pub loaded: bool,
    pub bytes: u64,
    finder: memmem::Finder<'static>,
}

impl Tailer {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            partial: Vec::new(),
            stats: Stats::default(),
            loaded: false,
            bytes: 0,
            finder: memmem::Finder::new(b"\"type\":\""),
        }
    }

    /// Read any newly appended bytes. Returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return false;
        };
        let len = meta.len();
        self.bytes = len;
        if len < self.offset {
            // truncated / rewritten: start over
            self.offset = 0;
            self.partial.clear();
            self.stats = Stats::default();
        }
        if len == self.offset {
            self.loaded = true;
            return false;
        }
        let Ok(mut f) = File::open(&self.path) else {
            return false;
        };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }
        let mut buf = Vec::with_capacity((len - self.offset) as usize + 1);
        if f.read_to_end(&mut buf).is_err() {
            return false;
        }
        self.offset += buf.len() as u64;
        let mut data = std::mem::take(&mut self.partial);
        data.extend_from_slice(&buf);
        let mut start = 0;
        while let Some(nl) = memchr(b'\n', &data[start..]) {
            let line = &data[start..start + nl];
            self.ingest(line);
            start += nl + 1;
        }
        self.partial = data[start..].to_vec();
        self.loaded = true;
        true
    }

    fn interesting(&self, line: &[u8]) -> bool {
        for pos in self.finder.find_iter(line) {
            let rest = &line[pos + 8..];
            if NEEDLES.iter().any(|n| rest.starts_with(n)) {
                return true;
            }
        }
        false
    }

    fn ingest(&mut self, line: &[u8]) {
        if line.is_empty() || !self.interesting(line) {
            return;
        }
        let Ok(e) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let ts = e.timestamp.as_deref().and_then(parse_rfc3339_ms);
        let s = &mut self.stats;
        if let Some(t) = ts {
            if t > s.last_activity_ms {
                s.last_activity_ms = t;
            }
        }
        match e.kind.as_str() {
            "assistant" => {
                s.last_entry_was_user = false;
                if let Some(b) = e.git_branch {
                    s.git_branch = if b == "HEAD" { String::new() } else { b };
                }
                if let Some(ms) = e.thinking_ms {
                    s.thinking_ms += ms;
                }
                if let Some(eff) = e.effort {
                    let v = match eff {
                        Value::String(x) => Some(x),
                        Value::Object(o) => o.get("level").and_then(Value::as_str).map(String::from),
                        _ => None,
                    };
                    if v.is_some() {
                        s.effort = v;
                    }
                }
                let Some(m) = e.message else { return };
                if let Some(model) = m.model {
                    if !model.is_empty() && model != "<synthetic>" {
                        s.model = model;
                    }
                }
                if let Some(u) = m.usage {
                    let same = m.id.is_some() && m.id == s.last_msg_id;
                    if same {
                        let lu = s.last_usage;
                        s.input_tokens -= lu.input_tokens;
                        s.output_tokens -= lu.output_tokens;
                        s.cache_read -= lu.cache_read_input_tokens;
                        s.cache_write -= lu.cache_creation_input_tokens;
                    } else {
                        s.turns += 1;
                    }
                    s.input_tokens += u.input_tokens;
                    s.output_tokens += u.output_tokens;
                    s.cache_read += u.cache_read_input_tokens;
                    s.cache_write += u.cache_creation_input_tokens;
                    s.ctx_tokens = u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens;
                    s.last_usage = u;
                    s.last_msg_id = m.id.clone();
                }
                if m.stop_reason.is_some() {
                    s.last_stop_reason = m.stop_reason;
                }
                if let Some(Content::Blocks(blocks)) = m.content {
                    for b in blocks {
                        match b.kind.as_str() {
                            "tool_use" => {
                                let raw = b.name.unwrap_or_default();
                                let summary = tool_summary(&raw, b.input.as_ref());
                                let name = short_tool(&raw);
                                *s.tools.entry(name.clone()).or_insert(0) += 1;
                                s.tool_calls += 1;
                                s.last_tool = Some((name.clone(), summary.clone()));
                                s.running.push(RunningTool {
                                    id: b.id.unwrap_or_default(),
                                    name,
                                    summary,
                                    since_ms: ts.unwrap_or(0),
                                });
                                if s.running.len() > 32 {
                                    s.running.remove(0);
                                }
                            }
                            "text" => {
                                if let Some(t) = b.text {
                                    let t = one_line(&t);
                                    if !t.is_empty() {
                                        s.last_reply = t;
                                    }
                                }
                            }
                            // progress notes between tool calls arrive as short, non-empty thinking blocks
                            "thinking" => {
                                if let Some(t) = b.thinking {
                                    let t = one_line(&t);
                                    if !t.is_empty() {
                                        s.last_reply = t;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "user" => {
                s.last_entry_was_user = true;
                let Some(m) = e.message else { return };
                match m.content {
                    Some(Content::Text(t)) => {
                        s.prompts += 1;
                        let t = one_line(&t);
                        if s.first_prompt.is_empty() {
                            s.first_prompt = t.clone();
                        }
                        s.last_prompt = t;
                    }
                    Some(Content::Blocks(blocks)) => {
                        let mut had_text = false;
                        for b in blocks {
                            match b.kind.as_str() {
                                "tool_result" => {
                                    if b.is_error == Some(true) {
                                        s.tool_errors += 1;
                                    }
                                    if let Some(id) = b.tool_use_id {
                                        s.running.retain(|r| r.id != id);
                                    }
                                }
                                "text" => {
                                    if let Some(t) = b.text {
                                        let t = one_line(&t);
                                        if !t.is_empty() && !t.starts_with("<system-reminder>") && !t.starts_with("<local-command") {
                                            if !had_text {
                                                s.prompts += 1;
                                                had_text = true;
                                            }
                                            if s.first_prompt.is_empty() {
                                                s.first_prompt = t.clone();
                                            }
                                            s.last_prompt = t;
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            "custom-title" => {
                if let Some(t) = e.custom_title {
                    s.title = Some(t);
                }
            }
            "agent-name" => {
                if s.title.is_none() {
                    s.title = e.agent_name;
                }
            }
            "last-prompt" => {
                if let Some(p) = e.last_prompt {
                    s.last_prompt = one_line(&p);
                }
            }
            "summary" => {
                s.compactions += 1;
            }
            "system" => {
                if e.subtype.as_deref() == Some("compact_boundary") {
                    s.compactions += 1;
                }
            }
            _ => {}
        }
    }
}

/// Encode a cwd the way Claude Code names its project directories.
pub fn encode_cwd(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_lines(lines: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weft-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{}.jsonl", crate::util::now_ms()));
        let mut f = File::create(&p).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        p
    }

    #[test]
    fn dedups_split_assistant_entries_and_tracks_tools() {
        let a1 = r#"{"parentUuid":null,"isSidechain":false,"type":"assistant","timestamp":"2026-10-09T13:44:05.000Z","gitBranch":"main","message":{"id":"msg_1","model":"claude-fable-5-1","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50},"content":[{"type":"thinking","thinking":"..."}]}}"#;
        let a2 = r#"{"parentUuid":null,"isSidechain":false,"type":"assistant","timestamp":"2026-10-09T13:44:06.000Z","message":{"id":"msg_1","model":"claude-fable-5-1","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50},"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"sleep 1"}}]}}"#;
        let user_prompt = r#"{"type":"user","timestamp":"2026-10-09T13:44:00.000Z","message":{"role":"user","content":"do the thing"}}"#;
        let attachment = r#"{"parentUuid":"x","isSidechain":false,"attachment":{"type":"environment","snapshot":{"note":"\"type\":\"assistant\" inside pasted text"}},"type":"attachment"}"#;
        let p = write_lines(&[user_prompt, a1, a2, attachment]);
        let mut t = Tailer::new(p.clone());
        assert!(t.poll());
        assert_eq!(t.stats.turns, 1);
        assert_eq!(t.stats.input_tokens, 2);
        assert_eq!(t.stats.cache_read, 1000);
        assert_eq!(t.stats.output_tokens, 50);
        assert_eq!(t.stats.ctx_tokens, 1102);
        assert_eq!(t.stats.tool_calls, 1);
        assert_eq!(t.stats.running.len(), 1);
        assert_eq!(t.stats.running[0].summary, "sleep 1");
        assert_eq!(t.stats.prompts, 1);
        assert_eq!(t.stats.git_branch, "main");
        assert_eq!(t.stats.last_activity_ms, 1_791_553_446_000);
        assert!(!t.poll(), "no new bytes");

        // append a tool result -> running tool is cleared
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, r#"{{"type":"user","timestamp":"2026-10-09T13:44:07.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"boom"}}]}}}}"#).unwrap();
        assert!(t.poll());
        assert!(t.stats.running.is_empty());
        assert_eq!(t.stats.tool_errors, 1);
        assert_eq!(t.stats.prompts, 1, "tool results are not prompts");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn encodes_cwd_like_claude() {
        assert_eq!(
            encode_cwd("/Users/me@example.com/src/acme.api"),
            "-Users-me-example-com-src-acme-api"
        );
    }
}
