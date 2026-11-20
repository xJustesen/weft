//! UI state and key handling.

use crate::collector::Cmd;
use crate::history::{RecentProject, RecentSession};
use crate::jump::{self, Target};
use crate::model::{SessionView, Snapshot, Status};
use crate::worktrees::{self, Worktree};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Board,
    Monitor,
    History,
    Cleanup,
}

impl View {
    pub const ALL: [View; 4] = [View::Board, View::Monitor, View::History, View::Cleanup];
    pub fn name(self) -> &'static str {
        match self {
            View::Board => "board",
            View::Monitor => "monitor",
            View::History => "history",
            View::Cleanup => "cleanup",
        }
    }
    /// The pane's hotkey: its first letter.
    pub fn key(self) -> char {
        self.name().chars().next().unwrap_or(' ')
    }
    fn from_key(c: char) -> Option<View> {
        View::ALL.into_iter().find(|v| v.key() == c)
    }
    fn step(self, by: usize) -> View {
        let i = View::ALL.iter().position(|v| *v == self).unwrap_or(0);
        View::ALL[(i + by) % View::ALL.len()]
    }
}

/// :return: Option[int] the new selection for a movement key in a list of `n`, or None for other keys
fn nav(code: KeyCode, cur: usize, n: usize) -> Option<usize> {
    let last = n.saturating_sub(1);
    Some(match code {
        KeyCode::Down | KeyCode::Char('j') => (cur + 1).min(last),
        KeyCode::Up | KeyCode::Char('k') => cur.saturating_sub(1),
        KeyCode::PageDown => (cur + 10).min(last),
        KeyCode::PageUp => cur.saturating_sub(10),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => return None,
    })
}

/// One line of the history view: a project header or one of its sessions.
pub enum RecentRow<'a> {
    Project(&'a RecentProject, usize),
    Session(&'a RecentSession),
}

#[derive(Default)]
pub struct Cleanup {
    pub scanning: bool,
    pub scanned: bool,
    pub items: Vec<Worktree>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Activity,
    Pid,
    Name,
    Project,
    Cpu,
    Mem,
    Ctx,
    Tokens,
    Tools,
    Uptime,
}

impl SortKey {
    pub const ALL: [SortKey; 10] = [
        SortKey::Activity,
        SortKey::Pid,
        SortKey::Name,
        SortKey::Project,
        SortKey::Cpu,
        SortKey::Mem,
        SortKey::Ctx,
        SortKey::Tokens,
        SortKey::Tools,
        SortKey::Uptime,
    ];
    pub fn name(self) -> &'static str {
        match self {
            SortKey::Activity => "activity",
            SortKey::Pid => "pid",
            SortKey::Name => "name",
            SortKey::Project => "project",
            SortKey::Cpu => "cpu",
            SortKey::Mem => "mem",
            SortKey::Ctx => "ctx",
            SortKey::Tokens => "tokens",
            SortKey::Tools => "tools",
            SortKey::Uptime => "uptime",
        }
    }
    fn idx(self) -> usize {
        SortKey::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }
    pub fn next(self) -> SortKey {
        SortKey::ALL[(self.idx() + 1) % SortKey::ALL.len()]
    }
    pub fn prev(self) -> SortKey {
        SortKey::ALL[(self.idx() + SortKey::ALL.len() - 1) % SortKey::ALL.len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Kill(u32, String),
    Remove(PathBuf),
}

pub struct App {
    pub snap: Snapshot,
    pub selected_pid: Option<u32>,
    pub sort: SortKey,
    pub reverse: bool,
    pub filter: String,
    pub filtering: bool,
    pub show_detail: bool,
    pub show_graphs: bool,
    pub show_ended: bool,
    pub overlay: Overlay,
    pub interval: Duration,
    pub quit: bool,
    pub flash: Option<(String, Instant)>,
    pub table_offset: usize,
    pub ended_grace: Duration,
    pub view: View,
    pub recent_sel: usize,
    pub recent_offset: usize,
    pub show_archived: bool,
    pub cleanup: Arc<Mutex<Cleanup>>,
    pub cleanup_sel: usize,
    /// Messages from background jobs (jump, worktree removal), shown in the header.
    notes: Arc<Mutex<Vec<String>>>,
    cmd_tx: Sender<Cmd>,
}

impl App {
    pub fn new(cmd_tx: Sender<Cmd>, interval: Duration, view: View) -> Self {
        Self {
            snap: Snapshot::default(),
            selected_pid: None,
            sort: SortKey::Activity,
            reverse: false,
            filter: String::new(),
            filtering: false,
            show_detail: true,
            show_graphs: true,
            show_ended: false,
            overlay: Overlay::None,
            interval,
            quit: false,
            flash: None,
            table_offset: 0,
            ended_grace: Duration::from_secs(60),
            view,
            recent_sel: 0,
            recent_offset: 0,
            show_archived: false,
            cleanup: Arc::default(),
            cleanup_sel: 0,
            notes: Arc::default(),
            cmd_tx,
        }
    }

    /// Move finished background messages into the header flash.
    pub fn poll_background(&mut self) -> bool {
        let note = self.notes.lock().ok().and_then(|mut n| n.pop());
        let Some(note) = note else { return false };
        self.set_flash(note);
        true
    }

    fn matches(&self, fields: &[&str]) -> bool {
        let needle = self.filter.to_lowercase();
        needle.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(&needle))
    }

    /// Live sessions for the board: waiting on you, then working, then idle; newest activity first.
    pub fn board_rows(&self) -> Vec<&SessionView> {
        let mut rows: Vec<&SessionView> = self
            .snap
            .sessions
            .iter()
            .filter(|s| s.status != Status::Ended)
            .filter(|s| self.matches(&[&s.name, &s.project, &s.branch, &s.model]))
            .collect();
        rows.sort_by(|a, b| a.board().cmp(&b.board()).then(b.last_activity_ms.cmp(&a.last_activity_ms)).then(a.pid.cmp(&b.pid)));
        rows
    }

    pub fn recent_rows(&self) -> Vec<RecentRow<'_>> {
        let mut out = Vec::new();
        let Some(projects) = &self.snap.recent else { return out };
        for p in projects.iter() {
            let sessions: Vec<&RecentSession> = p
                .sessions
                .iter()
                .filter(|s| self.show_archived || !s.archived)
                .filter(|s| self.matches(&[&s.title, &p.name, s.branch.as_deref().unwrap_or("")]))
                .collect();
            if sessions.is_empty() {
                continue;
            }
            out.push(RecentRow::Project(p, sessions.len()));
            out.extend(sessions.into_iter().map(RecentRow::Session));
        }
        out
    }

    fn recent_sessions(&self) -> Vec<&RecentSession> {
        self.recent_rows()
            .into_iter()
            .filter_map(|r| match r {
                RecentRow::Session(s) => Some(s),
                RecentRow::Project(..) => None,
            })
            .collect()
    }

    fn background(&self, job: impl FnOnce() -> String + Send + 'static) {
        let notes = self.notes.clone();
        std::thread::spawn(move || {
            let msg = job();
            if let Ok(mut n) = notes.lock() {
                n.push(msg);
            }
        });
    }

    fn jump_to(&mut self, s: &SessionView) {
        let target = Target {
            pid: s.pid,
            cwd: s.cwd.clone(),
            desktop_id: s.desktop_id.clone(),
            entrypoint: s.entrypoint.clone(),
        };
        self.set_flash(format!("jumping to {}…", s.name));
        self.background(move || jump::jump(&target));
    }

    fn open_recent(&mut self, s: &RecentSession) {
        if let Some(id) = &s.desktop_id {
            let url = format!("claude://code/continue?session={id}");
            self.background(move || match std::process::Command::new("open").arg(&url).status() {
                Ok(st) if st.success() => "opened in Claude".into(),
                _ => "could not open the Claude app".into(),
            });
            return;
        }
        let cmd = format!("cd '{}' && claude --resume {}", s.cwd.replace('\'', "'\\''"), s.session_id);
        if jump::copy(&cmd) {
            self.set_flash("resume command copied");
        } else {
            self.set_flash("could not copy to the clipboard");
        }
    }

    /// Scan worktrees of every known project on a background thread.
    pub fn scan_cleanup(&mut self) {
        let mut roots: Vec<String> = self
            .snap
            .recent
            .iter()
            .flat_map(|r| r.iter().map(|p| p.root.clone()))
            .chain(self.snap.sessions.iter().map(|s| crate::history::project_root(&s.cwd)))
            .filter(|r| r.starts_with('/'))
            .collect();
        roots.sort();
        roots.dedup();
        // Desktop sessions keep their worktree until archived, even with no process running.
        let open_desktop = self
            .snap
            .recent
            .iter()
            .flat_map(|r| r.iter())
            .flat_map(|p| &p.sessions)
            .filter(|s| s.desktop_id.is_some() && !s.archived)
            .map(|s| s.work_cwd.clone());
        let live: Vec<String> = self
            .snap
            .sessions
            .iter()
            .filter(|s| s.status != Status::Ended)
            .map(|s| s.cwd.clone())
            .chain(open_desktop)
            .collect();
        let state = self.cleanup.clone();
        if let Ok(mut c) = state.lock() {
            if c.scanning {
                return;
            }
            c.scanning = true;
        }
        std::thread::spawn(move || {
            let items = worktrees::scan(&roots, &live);
            if let Ok(mut c) = state.lock() {
                *c = Cleanup { scanning: false, scanned: true, items };
            }
        });
    }

    fn selected_worktree(&self) -> Option<Worktree> {
        self.cleanup.lock().ok()?.items.get(self.cleanup_sel).cloned()
    }

    fn remove_worktree(&mut self, path: PathBuf) {
        let Some(wt) = self.cleanup.lock().ok().and_then(|c| c.items.iter().find(|w| w.path == path).cloned()) else { return };
        let state = self.cleanup.clone();
        self.background(move || match worktrees::remove(&wt) {
            Ok(()) => {
                if let Ok(mut c) = state.lock() {
                    c.items.retain(|w| w.path != wt.path);
                }
                format!("removed worktree {}", wt.name)
            }
            Err(e) => format!("git refused: {}", e.lines().next().unwrap_or("")),
        });
    }

    pub fn apply(&mut self, snap: Snapshot) {
        self.snap = snap;
        if self.selected_pid.is_none() {
            self.selected_pid = self.rows().first().map(|r| r.pid);
        }
    }

    pub fn has_data(&self) -> bool {
        self.snap.tick > 0
    }

    /// Filtered + sorted rows for the table.
    pub fn rows(&self) -> Vec<&SessionView> {
        let now = self.snap.at_ms;
        let grace = self.ended_grace.as_millis() as u64;
        let needle = self.filter.to_lowercase();
        let mut rows: Vec<&SessionView> = self
            .snap
            .sessions
            .iter()
            .filter(|s| match s.ended_at_ms {
                Some(e) => self.show_ended || now.saturating_sub(e) < grace,
                None => true,
            })
            .filter(|s| {
                needle.is_empty()
                    || s.name.to_lowercase().contains(&needle)
                    || s.project.to_lowercase().contains(&needle)
                    || s.branch.to_lowercase().contains(&needle)
                    || s.model.to_lowercase().contains(&needle)
                    || s.pid.to_string().contains(&needle)
            })
            .collect();
        let key = self.sort;
        rows.sort_by(|a, b| {
            use std::cmp::Ordering;
            let ord = match key {
                SortKey::Activity => a
                    .status
                    .rank()
                    .cmp(&b.status.rank())
                    .then(b.last_activity_ms.cmp(&a.last_activity_ms)),
                SortKey::Pid => a.pid.cmp(&b.pid),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Project => a.project.to_lowercase().cmp(&b.project.to_lowercase()),
                SortKey::Cpu => (b.cpu + b.child_cpu).partial_cmp(&(a.cpu + a.child_cpu)).unwrap_or(Ordering::Equal),
                SortKey::Mem => b.mem.cmp(&a.mem),
                SortKey::Ctx => b.ctx_tokens.cmp(&a.ctx_tokens),
                SortKey::Tokens => (b.input_total() + b.output_tokens).cmp(&(a.input_total() + a.output_tokens)),
                SortKey::Tools => b.tool_calls.cmp(&a.tool_calls),
                SortKey::Uptime => a.started_at_ms.cmp(&b.started_at_ms),
            };
            let ord = ord.then(a.pid.cmp(&b.pid));
            if self.reverse {
                ord.reverse()
            } else {
                ord
            }
        });
        rows
    }

    pub fn selected_index(&self) -> Option<usize> {
        let pid = self.selected_pid?;
        self.rows().iter().position(|r| r.pid == pid)
    }

    pub fn selected(&self) -> Option<&SessionView> {
        let pid = self.selected_pid?;
        self.snap.sessions.iter().find(|s| s.pid == pid)
    }

    fn move_sel(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            self.selected_pid = None;
            return;
        }
        let cur = self.selected_index().unwrap_or(0) as isize;
        let n = rows.len() as isize;
        let next = (cur + delta).clamp(0, n - 1) as usize;
        self.selected_pid = Some(rows[next].pid);
    }

    fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some((msg.into(), Instant::now()));
    }

    pub fn flash_text(&self) -> Option<&str> {
        match &self.flash {
            Some((m, t)) if t.elapsed() < Duration::from_secs(3) => Some(m.as_str()),
            _ => None,
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            self.quit = true;
            return;
        }
        match &self.overlay {
            Overlay::Help => {
                self.overlay = Overlay::None;
                return;
            }
            Overlay::Kill(pid, _) => {
                let pid = *pid;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    let _ = self.cmd_tx.send(Cmd::Kill(pid));
                    self.set_flash(format!("SIGTERM sent to {pid}"));
                }
                self.overlay = Overlay::None;
                return;
            }
            Overlay::Remove(path) => {
                let path = path.clone();
                self.overlay = Overlay::None;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    self.remove_worktree(path);
                }
                return;
            }
            Overlay::None => {}
        }
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) if !ctrl => self.filter.push(c),
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Down => self.move_sel(1),
                _ => {}
            }
            self.ensure_selection();
            return;
        }
        // Keys that mean the same thing on every pane.
        match key.code {
            KeyCode::Tab => return self.set_view(self.view.step(1)),
            KeyCode::BackTab => return self.set_view(self.view.step(View::ALL.len() - 1)),
            KeyCode::Char(c) if View::from_key(c).is_some() => return self.set_view(View::from_key(c).unwrap_or(self.view)),
            KeyCode::Char('?') | KeyCode::F(1) => return self.overlay = Overlay::Help,
            KeyCode::Char('q') => return self.quit = true,
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.quit = true;
                } else {
                    self.filter.clear();
                }
                return self.ensure_selection();
            }
            KeyCode::Char('f') | KeyCode::Char('/') if self.view != View::Cleanup => return self.filtering = true,
            _ => {}
        }
        match self.view {
            View::Board => self.on_board_key(key),
            View::Monitor => self.on_monitor_key(key),
            View::History => self.on_history_key(key),
            View::Cleanup => self.on_cleanup_key(key),
        }
        self.ensure_selection();
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        if v == View::Cleanup && !self.cleanup.lock().is_ok_and(|c| c.scanned || c.scanning) {
            self.scan_cleanup();
        }
    }

    /// Keys shared by the two live-session panes, board and monitor.
    fn on_session_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => {
                if let Some(s) = self.selected().filter(|s| s.status != Status::Ended).cloned() {
                    self.jump_to(&s);
                }
            }
            KeyCode::Char('d') => self.show_detail = !self.show_detail,
            KeyCode::Char('t') | KeyCode::Char('K') => {
                if let Some(s) = self.selected().filter(|s| s.status != Status::Ended) {
                    self.overlay = Overlay::Kill(s.pid, s.name.clone());
                }
            }
            _ => {}
        }
    }

    fn on_board_key(&mut self, key: KeyEvent) {
        if let KeyCode::Char(c @ '1'..='9') = key.code {
            let n = c as usize - '1' as usize;
            if let Some(s) = self.board_rows().get(n).map(|s| (*s).clone()) {
                self.selected_pid = Some(s.pid);
                self.jump_to(&s);
            }
            return;
        }
        let pids: Vec<u32> = self.board_rows().iter().map(|r| r.pid).collect();
        let cur = self.selected_pid.and_then(|p| pids.iter().position(|x| *x == p)).unwrap_or(0);
        match nav(key.code, cur, pids.len()) {
            Some(i) => self.selected_pid = pids.get(i).copied(),
            None => self.on_session_key(key),
        }
    }

    fn on_monitor_key(&mut self, key: KeyEvent) {
        let pids: Vec<u32> = self.rows().iter().map(|r| r.pid).collect();
        if let Some(i) = nav(key.code, self.selected_index().unwrap_or(0), pids.len()) {
            self.selected_pid = pids.get(i).copied();
            return;
        }
        match key.code {
            KeyCode::Char('g') => self.show_graphs = !self.show_graphs,
            KeyCode::Char('s') | KeyCode::Right | KeyCode::Char('>') => self.sort = self.sort.next(),
            KeyCode::Char('S') | KeyCode::Left | KeyCode::Char('<') => self.sort = self.sort.prev(),
            KeyCode::Char('r') => self.reverse = !self.reverse,
            KeyCode::Char('a') => self.show_ended = !self.show_ended,
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_interval(self.interval + Duration::from_millis(100)),
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.set_interval(self.interval.saturating_sub(Duration::from_millis(100)).max(Duration::from_millis(100)))
            }
            _ => self.on_session_key(key),
        }
    }

    fn on_history_key(&mut self, key: KeyEvent) {
        let n = self.recent_sessions().len();
        if let Some(i) = nav(key.code, self.recent_sel, n) {
            self.recent_sel = i;
            return;
        }
        match key.code {
            KeyCode::Char('a') => {
                self.show_archived = !self.show_archived;
                self.recent_sel = 0;
            }
            KeyCode::Enter => {
                if let Some(s) = self.recent_sessions().get(self.recent_sel).map(|s| (*s).clone()) {
                    self.open_recent(&s);
                }
            }
            _ => {}
        }
    }

    fn on_cleanup_key(&mut self, key: KeyEvent) {
        let n = self.cleanup.lock().map_or(0, |c| c.items.len());
        if let Some(i) = nav(key.code, self.cleanup_sel, n) {
            self.cleanup_sel = i;
            return;
        }
        match key.code {
            KeyCode::Char('R') => self.scan_cleanup(),
            KeyCode::Enter => {
                if let Some(w) = self.selected_worktree() {
                    let _ = std::process::Command::new("open").arg(&w.path).status();
                }
            }
            KeyCode::Char('x') | KeyCode::Delete => match self.selected_worktree() {
                Some(w) if w.removable() => self.overlay = Overlay::Remove(w.path),
                Some(_) => self.set_flash("has uncommitted changes; git would refuse"),
                None => {}
            },
            _ => {}
        }
    }

    fn set_interval(&mut self, d: Duration) {
        self.interval = d;
        let _ = self.cmd_tx.send(Cmd::Interval(d));
        self.set_flash(format!("refresh {} ms", d.as_millis()));
    }

    fn ensure_selection(&mut self) {
        let rows = self.rows();
        if rows.is_empty() {
            self.selected_pid = None;
        } else if self.selected_index().is_none() {
            self.selected_pid = rows.first().map(|r| r.pid);
        }
    }
}
