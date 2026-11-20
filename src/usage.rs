//! Subscription usage limits: the rolling 5-hour window and the 7-day week.
//!
//! Two sources, freshest wins:
//!
//! 1. **statusline tap** — Claude Code hands every statusline command a
//!    `rate_limits` object on each render. A one-liner in the statusline script
//!    mirrors it to `<claude_dir>/weft-usage.json`; we just read that file.
//!    Zero API calls, updates whenever any session renders.
//! 2. **usage endpoint** — the call `/usage` makes, with the OAuth token Claude
//!    Code keeps in `~/.claude/.credentials.json` or the macOS keychain item
//!    "Claude Code-credentials". That endpoint has a small request quota of its
//!    own, so we poll it only every few minutes, only while the tap is stale,
//!    and back off for as long as a 429 asks. The token goes to `curl` via a
//!    config on stdin so it never appears in a process argument list.
//!
//! The poller runs on its own thread; the collector only clones the latest result.

use crate::util::{now_ms, parse_rfc3339_ms};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
/// File the statusline tap writes, relative to the Claude config dir.
pub const TAP_FILE: &str = "weft-usage.json";
/// A tap newer than this is used instead of hitting the endpoint.
const TAP_FRESH_MS: u64 = 10 * 60 * 1000;
/// How often the thread re-reads the tap file.
const TICK: Duration = Duration::from_secs(5);
/// Endpoint poll interval while the tap is stale or absent.
pub const POLL: Duration = Duration::from_secs(5 * 60);
/// Longest we will wait between endpoint polls while backing off after errors.
const MAX_BACKOFF: Duration = Duration::from_secs(60 * 60);
/// After this long without fresh numbers the header marks them stale.
pub const STALE_AFTER_MS: u64 = 12 * 60 * 1000;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    /// 0..=100
    pub used_pct: f32,
    pub resets_at_ms: Option<u64>,
}

impl Window {
    pub fn frac(&self) -> f32 {
        (self.used_pct / 100.0).clamp(0.0, 1.0)
    }
    pub fn resets_in_ms(&self, now: u64) -> Option<u64> {
        self.resets_at_ms.filter(|r| *r > now).map(|r| r - now)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    /// Any further windows the API reports (per-model weekly caps etc.), by key.
    pub extra: Vec<(String, Window)>,
    /// When the windows above were last fetched successfully (0 = never).
    pub fetched_at_ms: u64,
    /// "statusline" or "api".
    pub source: &'static str,
    /// Why the most recent poll failed, if it did. Windows keep their last good values.
    pub error: Option<String>,
}

impl Usage {
    pub fn stale(&self, now: u64) -> bool {
        self.fetched_at_ms == 0 || now.saturating_sub(self.fetched_at_ms) > STALE_AFTER_MS
    }
}

pub type Shared = Arc<Mutex<Option<Usage>>>;

/// Start the poller. The first result lands within a second or two of start.
pub fn spawn(claude_dir: PathBuf) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(None));
    let out = Arc::clone(&shared);
    let _ = std::thread::Builder::new().name("usage".into()).spawn(move || {
        let tap_path = claude_dir.join(TAP_FILE);
        let mut failures: u32 = 0;
        let mut next_poll = Instant::now();
        loop {
            let now = now_ms();
            let tap = read_tap(&tap_path).filter(|u| now.saturating_sub(u.fetched_at_ms) <= TAP_FRESH_MS);
            let mut fetched: Option<Result<Usage, Fail>> = None;
            if tap.is_none() && Instant::now() >= next_poll {
                let r = token(&claude_dir).map_err(Fail::other).and_then(|t| fetch(&t));
                let wait = match &r {
                    Ok(_) => {
                        failures = 0;
                        POLL
                    }
                    Err(f) => {
                        failures += 1;
                        let backoff = POLL.saturating_mul(1 << failures.min(4));
                        f.retry_after.unwrap_or(backoff).clamp(POLL, MAX_BACKOFF)
                    }
                };
                next_poll = Instant::now() + wait;
                fetched = Some(r.map_err(|f| Fail {
                    msg: if f.retry_after.is_some() {
                        format!("{} · retry in {}", f.msg, crate::util::human_dur(wait.as_millis() as u64))
                    } else {
                        f.msg
                    },
                    retry_after: f.retry_after,
                }));
            }
            if let Ok(mut slot) = out.lock() {
                let mut cur = slot.take().unwrap_or_default();
                if let Some(t) = tap {
                    if t.fetched_at_ms >= cur.fetched_at_ms {
                        cur = t;
                    }
                }
                match fetched {
                    Some(Ok(u)) => {
                        if u.fetched_at_ms >= cur.fetched_at_ms {
                            cur = u;
                        }
                    }
                    Some(Err(f)) => cur.error = Some(f.msg),
                    None => {}
                }
                *slot = Some(cur);
            }
            std::thread::sleep(TICK);
        }
    });
    shared
}

/// `{"at": <unix seconds>, "rate_limits": {...}}` as written by the statusline tap.
fn read_tap(path: &Path) -> Option<Usage> {
    let raw = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let at = v.get("at")?.as_f64()?;
    let at_ms = if at > 1e12 { at as u64 } else { (at * 1000.0) as u64 };
    let mut u = parse_windows(v.get("rate_limits")?)?;
    u.fetched_at_ms = at_ms;
    u.source = "statusline";
    Some(u)
}

/// Locate Claude Code's OAuth access token without ever logging it.
fn token(claude_dir: &Path) -> Result<String, String> {
    let file = claude_dir.join(".credentials.json");
    if let Ok(raw) = std::fs::read_to_string(&file) {
        if let Some(t) = token_from_json(&raw) {
            return Ok(t);
        }
    }
    if cfg!(target_os = "macos") {
        let out = Command::new("security")
            .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("keychain: {e}"))?;
        if out.status.success() {
            if let Some(t) = token_from_json(&String::from_utf8_lossy(&out.stdout)) {
                return Ok(t);
            }
            return Err("keychain item has no OAuth token".into());
        }
    }
    Err("no Claude login found (run `claude` and sign in)".into())
}

fn token_from_json(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw.trim()).ok()?;
    let oauth = v.get("claudeAiOauth")?;
    let t = oauth.get("accessToken")?.as_str()?.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.to_string())
}

/// A failed poll: what to show, and how long the server asked us to wait.
#[derive(Debug)]
struct Fail {
    msg: String,
    retry_after: Option<Duration>,
}

impl Fail {
    fn other(msg: impl Into<String>) -> Self {
        Self { msg: msg.into(), retry_after: None }
    }
}

fn fetch(token: &str) -> Result<Usage, Fail> {
    let mut child = Command::new("curl")
        .args(["-sS", "-m", "15", "--config", "-", "-D", "-", "-o", "-", "-w", "\n%{http_code}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Fail::other(format!("curl: {e}")))?;
    {
        let mut stdin = child.stdin.take().ok_or(Fail::other("curl stdin"))?;
        let cfg = format!(
            "url = \"{ENDPOINT}\"\nheader = \"Authorization: Bearer {token}\"\n\
             header = \"anthropic-beta: oauth-2025-04-20\"\nheader = \"Accept: application/json\"\n\
             header = \"User-Agent: weft/{}\"\n",
            env!("CARGO_PKG_VERSION")
        );
        stdin.write_all(cfg.as_bytes()).map_err(|e| Fail::other(format!("curl stdin: {e}")))?;
    }
    let out = child.wait_with_output().map_err(|e| Fail::other(format!("curl: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (raw, code) = match stdout.rsplit_once('\n') {
        Some((b, c)) => (b, c.trim()),
        None => ("", ""),
    };
    // `-D -` puts the header block(s) before the body, separated by a blank line.
    let (headers, body) = split_response(raw);
    if let Some(path) = std::env::var_os("WEFT_USAGE_DEBUG") {
        let _ = std::fs::write(path, format!("{}\n--- http {code}\n{headers}\n{body}\n", crate::util::now_ms()));
    }
    if !out.status.success() && body.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim().trim_start_matches("curl: ");
        return Err(Fail::other(if err.is_empty() { "network error".into() } else { crate::util::truncate(err, 48) }));
    }
    match code {
        "200" => parse(body).map_err(Fail::other),
        "401" | "403" => Err(Fail::other("auth expired · open a claude session to refresh")),
        "429" => Err(Fail {
            msg: match error_message(body) {
                Some(m) => format!("rate limited: {}", crate::util::truncate(&m, 40)),
                None => "rate limited".into(),
            },
            retry_after: retry_after(headers),
        }),
        c => Err(Fail::other(match error_message(body) {
            Some(m) => format!("http {c}: {}", crate::util::truncate(&m, 40)),
            None => format!("http {c}"),
        })),
    }
}

fn split_response(raw: &str) -> (&str, &str) {
    let mut rest = raw;
    let mut headers = "";
    // skip any number of header blocks (e.g. `100 Continue`), keep the last as `headers`
    while rest.starts_with("HTTP/") {
        let Some(end) = rest.find("\r\n\r\n").map(|i| (i, 4)).or_else(|| rest.find("\n\n").map(|i| (i, 2))) else {
            return (rest, "");
        };
        headers = &rest[..end.0];
        rest = &rest[end.0 + end.1..];
    }
    (headers, rest.trim())
}

fn retry_after(headers: &str) -> Option<Duration> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if !k.trim().eq_ignore_ascii_case("retry-after") {
            return None;
        }
        v.trim().parse::<u64>().ok().map(Duration::from_secs)
    })
}

fn error_message(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    v.get("error")?.get("message")?.as_str().map(str::to_string)
}

/// Parse the `/api/oauth/usage` body. Every top-level object carrying a numeric
/// `utilization` is a window; `five_hour` and `seven_day` are the headline ones.
pub fn parse(body: &str) -> Result<Usage, String> {
    let v: Value = serde_json::from_str(body).map_err(|_| "unexpected response".to_string())?;
    let mut u = parse_windows(&v).ok_or("no usage limits on this account")?;
    u.fetched_at_ms = now_ms();
    u.source = "api";
    Ok(u)
}

/// Every member object carrying a numeric `utilization` (endpoint) or
/// `used_percentage` (statusline) is a window. `None` when there are none.
fn parse_windows(v: &Value) -> Option<Usage> {
    let obj = v.as_object()?;
    let mut u = Usage::default();
    for (k, val) in obj {
        let Some(w) = window(val) else { continue };
        match k.as_str() {
            "five_hour" => u.five_hour = Some(w),
            "seven_day" => u.seven_day = Some(w),
            _ => u.extra.push((k.clone(), w)),
        }
    }
    if u.five_hour.is_none() && u.seven_day.is_none() {
        return None;
    }
    Some(u)
}

fn window(v: &Value) -> Option<Window> {
    let used = v.get("utilization").or_else(|| v.get("used_percentage"))?.as_f64()? as f32;
    let resets_at_ms = v.get("resets_at").and_then(|r| match r {
        Value::String(s) => parse_rfc3339_ms(s),
        Value::Number(n) => n.as_f64().map(|x| if x > 1e12 { x as u64 } else { (x * 1000.0) as u64 }),
        _ => None,
    });
    Some(Window { used_pct: used, resets_at_ms })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headline_and_extra_windows() {
        let body = r#"{
            "five_hour": {"utilization": 42.5, "resets_at": "2026-10-27T18:00:00Z"},
            "seven_day": {"utilization": 71, "resets_at": "2026-10-30T09:00:00+00:00"},
            "seven_day_opus": null,
            "seven_day_sonnet": {"utilization": 3, "resets_at": null},
            "extra_usage": {"is_enabled": false}
        }"#;
        let u = parse(body).unwrap();
        let fh = u.five_hour.unwrap();
        assert_eq!(fh.used_pct, 42.5);
        assert_eq!(fh.resets_at_ms, parse_rfc3339_ms("2026-10-27T18:00:00Z"));
        assert_eq!(u.seven_day.unwrap().used_pct, 71.0);
        assert_eq!(u.extra.len(), 1);
        assert_eq!(u.extra[0].0, "seven_day_sonnet");
        assert_eq!(u.extra[0].1.resets_at_ms, None);
    }

    #[test]
    fn reads_statusline_tap() {
        let dir = std::env::temp_dir().join(format!("weft-tap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TAP_FILE);
        std::fs::write(
            &path,
            r#"{"at":1761745000.5,"rate_limits":{"five_hour":{"used_percentage":70,"resets_at":1761750000},"seven_day":{"used_percentage":33.3,"resets_at":1761930000}}}"#,
        )
        .unwrap();
        let u = read_tap(&path).unwrap();
        assert_eq!(u.source, "statusline");
        assert_eq!(u.fetched_at_ms, 1761745000500);
        assert_eq!(u.five_hour.as_ref().unwrap().used_pct, 70.0);
        assert_eq!(u.five_hour.unwrap().resets_at_ms, Some(1761750000000));
        assert_eq!(u.seven_day.unwrap().used_pct, 33.3);
        std::fs::write(&path, r#"{"at":1,"rate_limits":null}"#).unwrap();
        assert!(read_tap(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_bodies_without_windows() {
        assert!(parse(r#"{"error":{"type":"authentication_error"}}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn token_lookup_ignores_blank_and_missing() {
        assert_eq!(token_from_json(r#"{"claudeAiOauth":{"accessToken":"  "}}"#), None);
        assert_eq!(token_from_json(r#"{"other":1}"#), None);
        assert_eq!(token_from_json(r#"{"claudeAiOauth":{"accessToken":"x"}}"#).as_deref(), Some("x"));
    }
}
