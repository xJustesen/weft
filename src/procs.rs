//! Per-process CPU / memory sampling via sysinfo, plus descendant aggregation.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcInfo {
    pub alive: bool,
    pub cpu: f32,
    pub mem: u64,
    pub children: u32,
    pub child_cpu: f32,
    pub child_mem: u64,
    pub start_time_s: u64,
}

#[derive(Debug, Clone)]
pub struct Unregistered {
    pub pid: u32,
    pub cwd: Option<String>,
    pub start_time_s: u64,
}

pub struct Procs {
    sys: System,
    last_full: Option<Instant>,
    children: HashMap<Pid, Vec<Pid>>,
}

const FULL_EVERY: Duration = Duration::from_secs(3);

impl Procs {
    pub fn new() -> Self {
        Self {
            sys: System::new(),
            last_full: None,
            children: HashMap::new(),
        }
    }

    fn kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing().with_cpu().with_memory()
    }

    fn descendants(&self, root: Pid) -> Vec<Pid> {
        let mut out = Vec::new();
        let mut q: VecDeque<Pid> = VecDeque::new();
        let mut seen: HashSet<Pid> = HashSet::new();
        q.push_back(root);
        while let Some(cur) = q.pop_front() {
            if let Some(kids) = self.children.get(&cur) {
                for k in kids {
                    if seen.insert(*k) {
                        out.push(*k);
                        q.push_back(*k);
                    }
                }
            }
        }
        out
    }

    /// Cheap every tick: cpu/mem for the session pids and their descendants only.
    /// Every couple of seconds: a bare process-table listing (no per-process stats)
    /// to discover new children and unregistered `claude` binaries.
    pub fn refresh(&mut self, pids: &[u32]) {
        let full = self.last_full.is_none_or(|t| t.elapsed() >= FULL_EVERY);
        if full {
            self.sys
                .refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
            self.last_full = Some(Instant::now());
            let mut map: HashMap<Pid, Vec<Pid>> = HashMap::new();
            for (pid, p) in self.sys.processes() {
                if let Some(pp) = p.parent() {
                    map.entry(pp).or_default().push(*pid);
                }
            }
            self.children = map;
        }
        let mut set: Vec<Pid> = Vec::with_capacity(pids.len() * 4);
        for p in pids {
            let pid = Pid::from_u32(*p);
            set.push(pid);
            set.extend(self.descendants(pid));
        }
        if !set.is_empty() {
            self.sys.refresh_processes_specifics(ProcessesToUpdate::Some(&set), true, Self::kind());
        }
    }

    pub fn info(&self, pid: u32) -> ProcInfo {
        let p = Pid::from_u32(pid);
        let Some(proc_) = self.sys.process(p) else {
            return ProcInfo::default();
        };
        let mut info = ProcInfo {
            alive: true,
            cpu: proc_.cpu_usage(),
            mem: proc_.memory(),
            start_time_s: proc_.start_time(),
            ..Default::default()
        };
        for k in self.descendants(p) {
            if let Some(kp) = self.sys.process(k) {
                info.children += 1;
                info.child_cpu += kp.cpu_usage();
                info.child_mem += kp.memory();
            }
        }
        info
    }

    /// `claude` binaries running outside the registry (e.g. older CLI builds).
    pub fn unregistered_claude(&mut self, known: &HashSet<u32>) -> Vec<Unregistered> {
        let mut out = Vec::new();
        let mut want_cwd: Vec<Pid> = Vec::new();
        for (pid, p) in self.sys.processes() {
            let pid_u = pid.as_u32();
            if known.contains(&pid_u) {
                continue;
            }
            if p.name().to_str() != Some("claude") {
                continue;
            }
            // skip children of known sessions (e.g. nested helpers)
            let mut parent = p.parent();
            let mut nested = false;
            let mut hops = 0;
            while let Some(pp) = parent {
                if known.contains(&pp.as_u32()) {
                    nested = true;
                    break;
                }
                hops += 1;
                if hops > 8 {
                    break;
                }
                parent = self.sys.process(pp).and_then(|x| x.parent());
            }
            if nested {
                continue;
            }
            if p.cwd().is_none() {
                want_cwd.push(*pid);
            }
            out.push(Unregistered {
                pid: pid_u,
                cwd: p.cwd().map(|c| c.to_string_lossy().into_owned()),
                start_time_s: p.start_time(),
            });
        }
        if !want_cwd.is_empty() {
            self.sys.refresh_processes_specifics(
                ProcessesToUpdate::Some(&want_cwd),
                false,
                ProcessRefreshKind::nothing().with_cwd(UpdateKind::OnlyIfNotSet),
            );
            for u in out.iter_mut() {
                if u.cwd.is_none() {
                    u.cwd = self
                        .sys
                        .process(Pid::from_u32(u.pid))
                        .and_then(|p| p.cwd())
                        .map(|c| c.to_string_lossy().into_owned());
                }
            }
        }
        out
    }

    pub fn terminate(&self, pid: u32) -> bool {
        self.sys
            .process(Pid::from_u32(pid))
            .and_then(|p| p.kill_with(Signal::Term))
            .unwrap_or(false)
    }
}

impl Default for Procs {
    fn default() -> Self {
        Self::new()
    }
}
