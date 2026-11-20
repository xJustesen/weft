//! Small formatting / parsing helpers shared by the collector and the UI.

use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 1234 -> "1.2k", 1_234_567 -> "1.2M"
pub fn human_num(n: u64) -> String {
    let f = n as f64;
    let s = if f >= 1e9 {
        format!("{:.1}G", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1}M", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.1}k", f / 1e3)
    } else {
        return n.to_string();
    };
    s.replace(".0", "")
}

pub fn human_rate(v: f32) -> String {
    if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else {
        format!("{:.0}", v)
    }
}

pub fn human_bytes(n: u64) -> String {
    let f = n as f64;
    if f >= 1e9 {
        format!("{:.1}G", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.0}M", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0}k", f / 1e3)
    } else {
        format!("{n}B")
    }
}

/// Compact duration: 12s, 3m, 2h13m, 1d4h
pub fn human_dur(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 36_000 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d{}h", s / 86_400, (s % 86_400) / 3600)
    }
}

/// "claude-fable-5-1" -> "fable-5.1", "claude-opus-5-5[1m]" -> "opus-5.5[1m]"
pub fn short_model(m: &str) -> String {
    let (base, suffix) = match m.find('[') {
        Some(i) => (&m[..i], &m[i..]),
        None => (m, ""),
    };
    let base = base.strip_prefix("claude-").unwrap_or(base);
    // collapse trailing "-N-M" or "-N-M-YYYYMMDD" into "N.M"
    let parts: Vec<&str> = base.split('-').collect();
    let mut name = Vec::new();
    let mut nums = Vec::new();
    for p in parts {
        if p.chars().all(|c| c.is_ascii_digit()) && p.len() <= 2 {
            nums.push(p);
        } else if !(p.chars().all(|c| c.is_ascii_digit()) && p.len() == 8) {
            name.push(p);
        }
    }
    let mut out = name.join("-");
    if !nums.is_empty() {
        out.push('-');
        out.push_str(&nums.join("."));
    }
    out.push_str(suffix);
    out
}

/// Context window for a model id. Current-generation models (4.6+) have 1M;
/// the 4.5 family and older are 200k. `[1m]` suffixed ids are always 1M.
pub fn context_limit(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    if m.is_empty() {
        return 1_000_000;
    }
    if m.contains("[1m]") {
        return 1_000_000;
    }
    let legacy = ["-4-5", "-4-1", "-4-0", "-3-7", "-3-5", "-3-0", "claude-3", "claude-2", "-opus-4-", "-sonnet-4-", "-haiku-4-"];
    let small = legacy.iter().any(|l| m.contains(l))
        && !["-4-6", "-4-7", "-4-8", "-4-9"].iter().any(|n| m.contains(n));
    if small {
        200_000
    } else {
        1_000_000
    }
}

/// Parse "2026-10-09T13:44:05.793Z" (or with +00:00 offset) into unix milliseconds.
pub fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(b.get(r)?).ok()?.parse::<i64>().ok()
    };
    let y = num(0..4)?;
    let mo = num(5..7)?;
    let d = num(8..10)?;
    let h = num(11..13)?;
    let mi = num(14..16)?;
    let se = num(17..19)?;
    // fractional seconds
    let mut i = 19;
    let mut frac_ms: i64 = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let mut digits = 0;
        let mut v: i64 = 0;
        while let Some(c) = b.get(i) {
            if !c.is_ascii_digit() {
                break;
            }
            if digits < 3 {
                v = v * 10 + (c - b'0') as i64;
                digits += 1;
            }
            i += 1;
        }
        while digits < 3 {
            v *= 10;
            digits += 1;
        }
        frac_ms = v;
    }
    // offset
    let mut off_min: i64 = 0;
    match b.get(i) {
        Some(b'Z') | Some(b'z') | None => {}
        Some(sign @ (b'+' | b'-')) => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6).unwrap_or(0);
            off_min = oh * 60 + om;
            if *sign == b'-' {
                off_min = -off_min;
            }
        }
        _ => {}
    }
    let days = days_from_civil(y, mo, d);
    let secs = days * 86_400 + h * 3600 + mi * 60 + se - off_min * 60;
    Some((secs * 1000 + frac_ms).max(0) as u64)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Truncate to a display width, appending an ellipsis when cut.
pub fn truncate(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Pad / truncate to exactly `width` cells.
pub fn fit(s: &str, width: usize, right: bool) -> String {
    let t = truncate(s, width);
    let w = t.width();
    let pad = width.saturating_sub(w);
    if right {
        format!("{}{}", " ".repeat(pad), t)
    } else {
        format!("{}{}", t, " ".repeat(pad))
    }
}

/// One-line, whitespace-collapsed preview of a longer text.
pub fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(400));
    let mut last_space = true;
    for c in s.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' && last_space {
            continue;
        }
        last_space = c == ' ';
        out.push(c);
        if out.len() > 400 {
            break;
        }
    }
    out.trim().to_string()
}

/// Project label from a cwd: last path component, or "repo:worktree" for Claude worktrees.
pub fn project_label(cwd: &str) -> String {
    if let Some(i) = cwd.find("/.claude/worktrees/") {
        let repo = cwd[..i].rsplit('/').next().unwrap_or("");
        let wt = cwd[i + "/.claude/worktrees/".len()..]
            .split('/')
            .next()
            .unwrap_or("");
        // strip trailing "-<6 hex>" hash
        let wt_short = match wt.rfind('-') {
            Some(j) if wt.len() - j - 1 == 6 && wt[j + 1..].chars().all(|c| c.is_ascii_hexdigit()) => &wt[..j],
            _ => wt,
        };
        return format!("{repo}:{wt_short}");
    }
    cwd.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(cwd)
        .to_string()
}

/// A short human summary of a tool call's input, like btop's command column.
pub fn tool_summary(name: &str, input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    let get = |k: &str| input.get(k).and_then(Value::as_str).map(one_line);
    let s = match name {
        "Bash" => get("command"),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => get("file_path").map(|p| shorten_path(&p)),
        "Grep" => match (get("pattern"), get("path")) {
            (Some(p), Some(d)) => Some(format!("{p}  in {}", shorten_path(&d))),
            (p, _) => p,
        },
        "Glob" => get("pattern"),
        "Agent" | "Task" => get("description").or_else(|| get("prompt")),
        "Skill" => get("skill"),
        "WebFetch" => get("url"),
        "WebSearch" => get("query"),
        "TodoWrite" => Some("update todos".into()),
        _ => None,
    };
    s.or_else(|| {
        // first string-valued field
        input.as_object().and_then(|o| {
            o.values()
                .find_map(|v| v.as_str().map(one_line))
        })
    })
    .unwrap_or_default()
}

/// "mcp__terminal__read_terminal" -> "terminal:read_terminal"
pub fn short_tool(name: &str) -> String {
    match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", ":", 1),
        None => name.to_string(),
    }
}

pub fn shorten_path(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = p.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}

pub fn pct(num: u64, den: u64) -> f32 {
    if den == 0 {
        0.0
    } else {
        (num as f64 / den as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339_ms("2026-10-09T13:44:05.793Z"), Some(1_791_553_445_793));
        assert_eq!(parse_rfc3339_ms("2026-10-09T13:44:05Z"), Some(1_791_553_445_000));
        assert_eq!(parse_rfc3339_ms("2026-10-09T15:44:05.793+02:00"), Some(1_791_553_445_793));
        assert_eq!(parse_rfc3339_ms("garbage"), None);
    }

    #[test]
    fn models() {
        assert_eq!(short_model("claude-fable-5-1"), "fable-5.1");
        assert_eq!(short_model("claude-opus-5-5[1m]"), "opus-5.5[1m]");
        assert_eq!(short_model("claude-sonnet-4-5-20250929"), "sonnet-4.5");
        assert_eq!(context_limit("claude-fable-5-1"), 1_000_000);
        assert_eq!(context_limit("claude-opus-5-5"), 1_000_000);
        assert_eq!(context_limit("claude-haiku-4-5"), 200_000);
        assert_eq!(context_limit("claude-sonnet-4-5-20250929"), 200_000);
        assert_eq!(context_limit("claude-sonnet-4-5[1m]"), 1_000_000);
        assert_eq!(context_limit("claude-opus-4-6"), 1_000_000);
    }

    #[test]
    fn labels() {
        assert_eq!(project_label("/Users/me/src/acme-api"), "acme-api");
        assert_eq!(
            project_label("/Users/me/src/acme-api/.claude/worktrees/jovial-murdock-37e7dd"),
            "acme-api:jovial-murdock"
        );
        assert_eq!(short_tool("mcp__terminal__read_terminal"), "terminal:read_terminal");
        assert_eq!(short_tool("Bash"), "Bash");
    }

    #[test]
    fn durations_and_numbers() {
        assert_eq!(human_dur(12_000), "12s");
        assert_eq!(human_dur(3 * 60_000), "3m");
        assert_eq!(human_dur(2 * 3_600_000 + 13 * 60_000), "2h13m");
        assert_eq!(human_dur(22 * 3_600_000 + 4 * 60_000), "22h");
        assert_eq!(human_dur(7 * 86_400_000), "7d0h");
        assert_eq!(human_num(999), "999");
        assert_eq!(human_num(1_234), "1.2k");
        assert_eq!(human_num(1_000_000), "1M");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(fit("ab", 4, true), "  ab");
    }

    #[test]
    fn tool_summaries() {
        let v: Value = serde_json::from_str(r#"{"command":"ls  -la\n~/x","description":"List"}"#).unwrap();
        assert_eq!(tool_summary("Bash", Some(&v)), "ls -la ~/x");
        let v: Value = serde_json::from_str(r#"{"pattern":"foo","path":"/tmp"}"#).unwrap();
        assert_eq!(tool_summary("Grep", Some(&v)), "foo  in /tmp");
    }
}
