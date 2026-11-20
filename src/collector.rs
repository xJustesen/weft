//! Background thread: discovers sessions, tails transcripts, samples processes,
//! and publishes a `Snapshot` every tick.

use crate::desktop::{DesktopIndex, DesktopSession};
use crate::model::*;
use crate::procs::{ProcInfo, Procs};
use crate::registry::{self, RegEntry};
use crate::transcript::{encode_cwd, Tailer};
use crate::util::{now_ms, project_label, truncate};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

pub enum Cmd {
    Kill(u32),
    Interval(Duration),
    Quit,
}

#[derive(Clone)]
pub struct Config {
    pub claude_dir: PathBuf,
    pub interval: Duration,
    pub ctx_limit: Option<u64>,
    pub history: usize,
    pub keep_ended: Duration,
    /// Poll subscription usage limits for the header.
    pub usage: bool,
    /// The desktop app's session-metadata folder.
    pub desktop_dir: PathBuf,
}

struct Tracked {
    pid: u32,
    reg: Option<RegEntry>,
    session_id: Option<String>,
    cwd: String,
    start_time_s: u64,
    main: Option<Tailer>,
    subagents: HashMap<PathBuf, Tailer>,
    sub_dir: Option<PathBuf>,
    last_sub_scan: Option<Instant>,
    last_path_probe: Option<Instant>,
    hist_in: VecDeque<f32>,
    hist_out: VecDeque<f32>,
    raw_in: VecDeque<f32>,
    raw_out: VecDeque<f32>,
    prev_in: u64,
    prev_out: u64,
    prev_t: Instant,
    ended_at_ms: Option<u64>,
    proc: ProcInfo,
}

impl Tracked {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            reg: None,
            session_id: None,
            cwd: String::new(),
            start_time_s: 0,
            main: None,
            subagents: HashMap::new(),
            sub_dir: None,
            last_sub_scan: None,
            last_path_probe: None,
            hist_in: VecDeque::new(),
            hist_out: VecDeque::new(),
            raw_in: VecDeque::new(),
            raw_out: VecDeque::new(),
            prev_in: 0,
            prev_out: 0,
            prev_t: Instant::now(),
            ended_at_ms: None,
            proc: ProcInfo::default(),
        }
    }

    fn totals_in(&self) -> u64 {
        self.main.as_ref().map_or(0, |t| t.stats.input_total())
            + self.subagents.values().map(|t| t.stats.input_total()).sum::<u64>()
    }
    fn totals_out(&self) -> u64 {
        self.main.as_ref().map_or(0, |t| t.stats.output_tokens)
            + self.subagents.values().map(|t| t.stats.output_tokens).sum::<u64>()
    }
}

fn projects_dir(cfg: &Config) -> PathBuf {
    cfg.claude_dir.join("projects")
}

/// Locate `<projects>/<encoded cwd>/<session>.jsonl`, falling back to a scan of all project dirs.
fn find_transcript(cfg: &Config, cwd: &str, sid: &str) -> Option<PathBuf> {
    let projects = projects_dir(cfg);
    if !cwd.is_empty() {
        let p = projects.join(encode_cwd(cwd)).join(format!("{sid}.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    let rd = std::fs::read_dir(&projects).ok()?;
    for ent in rd.flatten() {
        let p = ent.path().join(format!("{sid}.jsonl"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// For an unregistered process: newest transcript in its project dir that was modified after it started.
fn guess_transcript(cfg: &Config, cwd: &str, start_time_s: u64) -> Option<(String, PathBuf)> {
    let dir = projects_dir(cfg).join(encode_cwd(cwd));
    let rd = std::fs::read_dir(&dir).ok()?;
    let mut best: Option<(u64, PathBuf)> = None;
    for ent in rd.flatten() {
        let p = ent.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = ent.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if mtime + 5 < start_time_s {
            continue;
        }
        if best.as_ref().map_or(true, |(t, _)| mtime > *t) {
            best = Some((mtime, p));
        }
    }
    best.map(|(_, p)| {
        let sid = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        (sid, p)
    })
}

fn scan_subagents(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out
}

fn push_hist(q: &mut VecDeque<f32>, v: f32, cap: usize) {
    q.push_back(v);
    while q.len() > cap {
        q.pop_front();
    }
}

/// Smoothing window for token rates: a turn's usage lands in one tick, so spread it over ~4s.
const SMOOTH: Duration = Duration::from_secs(4);

/// Push a raw per-tick token delta and return the smoothed tokens/s over the window.
fn smooth(raw: &mut VecDeque<f32>, delta: f32, interval: Duration) -> f32 {
    let k = ((SMOOTH.as_secs_f32() / interval.as_secs_f32()).ceil() as usize).max(1);
    push_hist(raw, delta, k);
    let secs = raw.len() as f32 * interval.as_secs_f32();
    raw.iter().sum::<f32>() / secs.max(0.001)
}

fn status_of(reg: Option<&RegEntry>, ended: bool) -> Status {
    if ended {
        return Status::Ended;
    }
    match reg.and_then(|r| r.status.as_deref()) {
        Some("busy") | Some("running") | Some("working") => Status::Busy,
        Some("idle") | None => Status::Idle,
        Some(other) => Status::Other(other.to_string()),
    }
}

fn subagent_active(t: &Tailer, now: u64) -> bool {
    let s = &t.stats;
    if s.turns == 0 && s.prompts == 0 {
        return false;
    }
    let fresh = now.saturating_sub(s.last_activity_ms) < 10 * 60 * 1000;
    fresh && (s.last_entry_was_user || !s.running.is_empty() || s.last_stop_reason.as_deref() == Some("tool_use"))
}

pub fn run(mut cfg: Config, tx: Sender<Snapshot>, rx: Receiver<Cmd>) {
    let mut procs = Procs::new();
    let mut tracked: BTreeMap<u32, Tracked> = BTreeMap::new();
    let mut hist_in: VecDeque<f32> = VecDeque::new();
    let mut hist_out: VecDeque<f32> = VecDeque::new();
    let mut tick: u64 = 0;
    let mut collect_ema: f64 = 0.0;
    let sessions_dir = cfg.claude_dir.join("sessions");
    let usage = if cfg.usage { Some(crate::usage::spawn(cfg.claude_dir.clone())) } else { None };
    let recent = crate::history::spawn(projects_dir(&cfg), cfg.desktop_dir.clone(), Duration::from_secs(15));
    let mut desktop = DesktopIndex::default();
    let mut desktop_by_id: HashMap<String, DesktopSession> = HashMap::new();
    let mut last_desktop: Option<Instant> = None;

    loop {
        let t0 = Instant::now();
        let now = now_ms();

        // ---- 0. desktop metadata (throttled) -------------------------------
        if last_desktop.map_or(true, |t| t.elapsed() >= Duration::from_secs(3)) {
            desktop.refresh(&cfg.desktop_dir);
            desktop_by_id = desktop.by_any_id();
            last_desktop = Some(Instant::now());
        }

        // ---- 1. registry -------------------------------------------------
        let regs = registry::read(&sessions_dir);
        let mut reg_pids: HashSet<u32> = HashSet::new();
        for r in &regs {
            reg_pids.insert(r.pid);
            let t = tracked.entry(r.pid).or_insert_with(|| Tracked::new(r.pid));
            if t.session_id.as_deref() != Some(r.session_id.as_str()) {
                // new or re-used pid: reset transcript state
                t.main = None;
                t.subagents.clear();
                t.sub_dir = None;
                t.last_path_probe = None;
                t.session_id = Some(r.session_id.clone());
            }
            t.cwd = r.cwd.clone();
            t.reg = Some(r.clone());
            t.ended_at_ms = None;
        }

        // ---- 2. processes ------------------------------------------------
        let pids: Vec<u32> = tracked.keys().copied().collect();
        procs.refresh(&pids);
        for t in tracked.values_mut() {
            t.proc = procs.info(t.pid);
            if t.start_time_s == 0 {
                t.start_time_s = t.proc.start_time_s;
            }
        }
        // unregistered claude binaries
        let known: HashSet<u32> = tracked.keys().copied().collect();
        for u in procs.unregistered_claude(&known) {
            let t = tracked.entry(u.pid).or_insert_with(|| Tracked::new(u.pid));
            t.cwd = u.cwd.clone().unwrap_or_default();
            t.start_time_s = u.start_time_s;
            t.proc = procs.info(u.pid);
        }
        // mark ended
        for t in tracked.values_mut() {
            let dead = !t.proc.alive;
            let unregistered_gone = t.reg.is_some() && !reg_pids.contains(&t.pid);
            if (dead || unregistered_gone) && t.ended_at_ms.is_none() {
                t.ended_at_ms = Some(now);
            } else if !dead && (t.reg.is_none() || reg_pids.contains(&t.pid)) {
                t.ended_at_ms = None;
            }
        }
        // drop long-ended
        let keep_ms = cfg.keep_ended.as_millis() as u64;
        tracked.retain(|_, t| t.ended_at_ms.map_or(true, |e| now.saturating_sub(e) < keep_ms));

        // ---- 3. transcripts ----------------------------------------------
        // resolve paths (throttled)
        for t in tracked.values_mut() {
            if t.main.is_some() || t.ended_at_ms.is_some() {
                continue;
            }
            if t.last_path_probe.map_or(false, |p| p.elapsed() < Duration::from_secs(2)) {
                continue;
            }
            t.last_path_probe = Some(Instant::now());
            let found = match (&t.session_id, t.cwd.is_empty()) {
                (Some(sid), _) => find_transcript(&cfg, &t.cwd, sid).map(|p| (sid.clone(), p)),
                (None, false) => guess_transcript(&cfg, &t.cwd, t.start_time_s),
                _ => None,
            };
            if let Some((sid, p)) = found {
                t.session_id = Some(sid.clone());
                t.sub_dir = Some(p.with_extension("").join("subagents"));
                t.main = Some(Tailer::new(p));
            }
        }
        // subagent discovery (throttled)
        for t in tracked.values_mut() {
            let Some(dir) = t.sub_dir.clone() else { continue };
            if t.last_sub_scan.map_or(false, |p| p.elapsed() < Duration::from_secs(3)) {
                continue;
            }
            t.last_sub_scan = Some(Instant::now());
            for p in scan_subagents(&dir) {
                t.subagents.entry(p.clone()).or_insert_with(|| Tailer::new(p));
            }
        }
        // poll: first loads in parallel, steady state inline
        {
            let mut jobs: Vec<&mut Tailer> = Vec::new();
            for t in tracked.values_mut() {
                if let Some(m) = t.main.as_mut() {
                    jobs.push(m);
                }
                for s in t.subagents.values_mut() {
                    jobs.push(s);
                }
            }
            let any_unloaded = jobs.iter().any(|j| !j.loaded);
            if any_unloaded && jobs.len() > 1 {
                std::thread::scope(|sc| {
                    for j in jobs {
                        sc.spawn(move || {
                            j.poll();
                        });
                    }
                });
            } else {
                for j in jobs {
                    j.poll();
                }
            }
        }

        // ---- 4. rates ----------------------------------------------------
        let mut g_in = 0.0f32;
        let mut g_out = 0.0f32;
        for t in tracked.values_mut() {
            let cur_in = t.totals_in();
            let cur_out = t.totals_out();
            let loaded = t.main.as_ref().map_or(false, |m| m.loaded);
            // first sample after a (re)load must not spike with the whole history
            let (di, do_) = if !loaded || t.hist_in.is_empty() || (t.prev_in == 0 && t.prev_out == 0) {
                (0.0, 0.0)
            } else {
                (
                    cur_in.saturating_sub(t.prev_in) as f32,
                    cur_out.saturating_sub(t.prev_out) as f32,
                )
            };
            t.prev_in = cur_in;
            t.prev_out = cur_out;
            t.prev_t = Instant::now();
            let ri = smooth(&mut t.raw_in, di, cfg.interval);
            let ro = smooth(&mut t.raw_out, do_, cfg.interval);
            push_hist(&mut t.hist_in, ri, cfg.history);
            push_hist(&mut t.hist_out, ro, cfg.history);
            g_in += ri;
            g_out += ro;
        }
        push_hist(&mut hist_in, g_in, cfg.history);
        push_hist(&mut hist_out, g_out, cfg.history);

        // ---- 5. snapshot -------------------------------------------------
        let mut views = Vec::with_capacity(tracked.len());
        let mut totals = Totals::default();
        for t in tracked.values() {
            let ended = t.ended_at_ms.is_some();
            let status = status_of(t.reg.as_ref(), ended);
            let st = t.main.as_ref().map(|m| &m.stats);
            let reg = t.reg.as_ref();
            let desk = reg
                .and_then(|r| r.host_session_id.as_ref())
                .and_then(|h| desktop_by_id.get(h))
                .or_else(|| t.session_id.as_ref().and_then(|sid| desktop_by_id.get(sid)));
            let name = reg
                .and_then(|r| r.name.clone())
                .filter(|n| !n.trim().is_empty())
                .or_else(|| desk.and_then(|d| d.title.clone()))
                .or_else(|| st.and_then(|s| s.title.clone()))
                .or_else(|| st.map(|s| truncate(&s.first_prompt, 60)).filter(|s| !s.is_empty()))
                .or_else(|| t.session_id.as_ref().map(|s| s[..8.min(s.len())].to_string()))
                .unwrap_or_else(|| format!("pid {}", t.pid));
            let started_at_ms = reg
                .map(|r| r.started_at)
                .filter(|v| *v > 0)
                .unwrap_or(t.start_time_s * 1000);
            let (sub_in, sub_out, sub_active) = {
                let mut i = 0;
                let mut o = 0;
                let mut a = 0;
                for s in t.subagents.values() {
                    i += s.stats.input_total();
                    o += s.stats.output_tokens;
                    if !ended && subagent_active(s, now) {
                        a += 1;
                    }
                }
                (i, o, a)
            };
            let running: Vec<RunningView> = st
                .map(|s| {
                    s.running
                        .iter()
                        .rev()
                        .take(4)
                        .map(|r| RunningView {
                            name: r.name.clone(),
                            summary: r.summary.clone(),
                            since_ms: r.since_ms,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let model = st.map(|s| s.model.clone()).unwrap_or_default();
            let ctx_limit = cfg.ctx_limit.unwrap_or_else(|| crate::util::context_limit(&model));
            let v = SessionView {
                pid: t.pid,
                session_id: t.session_id.clone().unwrap_or_default(),
                name,
                cwd: t.cwd.clone(),
                project: project_label(&t.cwd),
                branch: st.map(|s| s.git_branch.clone()).unwrap_or_default(),
                status: status.clone(),
                status_since_ms: reg.and_then(|r| r.status_updated_at).unwrap_or(started_at_ms),
                started_at_ms,
                version: reg.map(|r| r.version.clone()).unwrap_or_default(),
                entrypoint: reg.map(|r| r.entrypoint.clone()).unwrap_or_else(|| "cli".into()),
                registered: t.reg.is_some(),
                model,
                effort: st.and_then(|s| s.effort.clone()),
                ctx_tokens: st.map_or(0, |s| s.ctx_tokens),
                ctx_limit,
                input_tokens: st.map_or(0, |s| s.input_tokens),
                output_tokens: st.map_or(0, |s| s.output_tokens),
                cache_read: st.map_or(0, |s| s.cache_read),
                cache_write: st.map_or(0, |s| s.cache_write),
                sub_input: sub_in,
                sub_output: sub_out,
                turns: st.map_or(0, |s| s.turns),
                prompts: st.map_or(0, |s| s.prompts),
                tool_calls: st.map_or(0, |s| s.tool_calls),
                tool_errors: st.map_or(0, |s| s.tool_errors),
                thinking_ms: st.map_or(0, |s| s.thinking_ms),
                compactions: st.map_or(0, |s| s.compactions),
                tools_top: st.map(|s| s.top_tools(6)).unwrap_or_default(),
                running,
                last_prompt: st.map(|s| s.last_prompt.clone()).unwrap_or_default(),
                last_reply: st.map(|s| s.last_reply.clone()).unwrap_or_default(),
                last_activity_ms: st.map_or(0, |s| s.last_activity_ms),
                cpu: t.proc.cpu,
                mem: t.proc.mem,
                children: t.proc.children,
                child_cpu: t.proc.child_cpu,
                subagents_active: sub_active,
                subagents_total: t.subagents.len() as u32,
                hist_in: t.hist_in.iter().copied().collect(),
                hist_out: t.hist_out.iter().copied().collect(),
                transcript_bytes: t.main.as_ref().map_or(0, |m| m.bytes),
                transcript_found: t.main.is_some(),
                loading: t.main.as_ref().map_or(false, |m| !m.loaded),
                ended_at_ms: t.ended_at_ms,
                desktop_id: desk.map(|d| d.local_id.clone()).or_else(|| reg.and_then(|r| r.host_session_id.clone())),
                needs_you: desk.is_some_and(|d| d.needs_attention()) && status != Status::Busy,
                attention: desk.and_then(|d| d.needs_action.clone().or(d.status_detail.clone())),
                prs: desk.map(|d| d.prs.clone()).unwrap_or_default(),
            };
            totals.sessions += 1;
            match v.status {
                Status::Busy | Status::Other(_) => totals.busy += 1,
                Status::Idle => totals.idle += 1,
                Status::Ended => totals.ended += 1,
            }
            totals.input_total += v.input_total();
            totals.output_total += v.output_tokens;
            totals.cache_read += v.cache_read;
            totals.cache_write += v.cache_write;
            totals.sub_input += v.sub_input;
            totals.sub_output += v.sub_output;
            totals.turns += v.turns;
            totals.tool_calls += v.tool_calls;
            totals.tool_errors += v.tool_errors;
            totals.subagents_active += v.subagents_active;
            totals.running_tools += v.running.len() as u32;
            if v.status != Status::Ended {
                totals.cpu += v.cpu + v.child_cpu;
                totals.mem += v.mem;
                totals.ctx_tokens += v.ctx_tokens;
                totals.ctx_limit += v.ctx_limit;
            }
            views.push(v);
        }
        tick += 1;
        let us = t0.elapsed().as_micros() as f64;
        collect_ema = if tick == 1 { us } else { collect_ema * 0.8 + us * 0.2 };
        let snap = Snapshot {
            at_ms: now,
            sessions: views,
            totals,
            hist_in: hist_in.iter().copied().collect(),
            hist_out: hist_out.iter().copied().collect(),
            collect_us: collect_ema as u128,
            usage: usage.as_ref().and_then(|u| u.lock().ok()?.clone()),
            claude_dir: cfg.claude_dir.to_string_lossy().into_owned(),
            tick,
            recent: recent.lock().ok().and_then(|g| g.clone()),
        };
        if tx.send(snap).is_err() {
            return;
        }

        // ---- 6. sleep / commands -----------------------------------------
        let mut remaining = cfg.interval.saturating_sub(t0.elapsed());
        loop {
            match rx.recv_timeout(remaining) {
                Ok(Cmd::Quit) => return,
                Ok(Cmd::Kill(pid)) => {
                    procs.terminate(pid);
                }
                Ok(Cmd::Interval(d)) => {
                    cfg.interval = d;
                    remaining = d.saturating_sub(t0.elapsed());
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            remaining = cfg.interval.saturating_sub(t0.elapsed());
            if remaining.is_zero() {
                break;
            }
        }
    }
}
