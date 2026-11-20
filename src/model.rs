//! Immutable snapshot handed from the collector thread to the UI every tick.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    Ended,
    Other(String),
}

impl Status {
    pub fn label(&self) -> &str {
        match self {
            Status::Busy => "busy",
            Status::Idle => "idle",
            Status::Ended => "ended",
            Status::Other(s) => s.as_str(),
        }
    }
    pub fn rank(&self) -> u8 {
        match self {
            Status::Busy => 0,
            Status::Other(_) => 1,
            Status::Idle => 2,
            Status::Ended => 3,
        }
    }
}

/// Board sections, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Board {
    Waiting,
    Working,
    Idle,
}

#[derive(Debug, Clone, Default)]
pub struct RunningView {
    pub name: String,
    pub summary: String,
    pub since_ms: u64,
}

#[derive(Debug, Clone)]
pub struct SessionView {
    pub pid: u32,
    pub session_id: String,
    pub name: String,
    pub cwd: String,
    pub project: String,
    pub branch: String,
    pub status: Status,
    pub status_since_ms: u64,
    pub started_at_ms: u64,
    pub version: String,
    pub entrypoint: String,
    pub registered: bool,
    pub model: String,
    pub effort: Option<String>,
    pub ctx_tokens: u64,
    pub ctx_limit: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub sub_input: u64,
    pub sub_output: u64,
    pub turns: u32,
    pub prompts: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub thinking_ms: u64,
    pub compactions: u32,
    pub tools_top: Vec<(String, u32)>,
    pub running: Vec<RunningView>,
    pub last_prompt: String,
    pub last_reply: String,
    pub last_activity_ms: u64,
    pub cpu: f32,
    pub mem: u64,
    pub children: u32,
    pub child_cpu: f32,
    pub subagents_active: u32,
    pub subagents_total: u32,
    pub hist_in: Vec<f32>,
    pub hist_out: Vec<f32>,
    pub transcript_bytes: u64,
    pub transcript_found: bool,
    pub loading: bool,
    pub ended_at_ms: Option<u64>,
    /// The Claude desktop app's `local_…` id, when the desktop app hosts this session.
    pub desktop_id: Option<String>,
    /// The desktop app's turn summary says the session waits on you (blocked or ready for review).
    pub needs_you: bool,
    /// What the desktop app says the session needs, or its status detail.
    pub attention: Option<String>,
    pub prs: Vec<crate::desktop::Pr>,
}

impl SessionView {
    /// :return: Board the board section: waiting on you, working, or idle
    pub fn board(&self) -> Board {
        match (&self.status, self.needs_you) {
            (Status::Ended, _) => Board::Idle,
            (Status::Other(_), _) | (_, true) => Board::Waiting,
            (Status::Busy, _) => Board::Working,
            (Status::Idle, _) => Board::Idle,
        }
    }
    pub fn input_total(&self) -> u64 {
        self.input_tokens + self.cache_read + self.cache_write
    }
    pub fn cache_hit(&self) -> f32 {
        let t = self.input_total();
        if t == 0 {
            0.0
        } else {
            self.cache_read as f32 / t as f32
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Totals {
    pub sessions: usize,
    pub busy: usize,
    pub idle: usize,
    pub ended: usize,
    pub input_total: u64,
    pub output_total: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub sub_input: u64,
    pub sub_output: u64,
    pub turns: u32,
    pub tool_calls: u32,
    pub tool_errors: u32,
    pub subagents_active: u32,
    pub running_tools: u32,
    pub cpu: f32,
    pub mem: u64,
    pub ctx_tokens: u64,
    pub ctx_limit: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub at_ms: u64,
    pub sessions: Vec<SessionView>,
    pub totals: Totals,
    pub hist_in: Vec<f32>,
    pub hist_out: Vec<f32>,
    pub collect_us: u128,
    /// Subscription usage limits; `None` until the first poll lands (or when disabled).
    pub usage: Option<crate::usage::Usage>,
    pub claude_dir: String,
    pub tick: u64,
    /// Past sessions by project; `None` until the first history scan finishes.
    pub recent: Option<std::sync::Arc<Vec<crate::history::RecentProject>>>,
}
