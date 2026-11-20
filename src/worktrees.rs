//! Agent worktrees (`<repo>/.claude/worktrees/<name>`) that no live session uses,
//! with enough git state to tell whether removing one loses work.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Worktree {
    pub path: PathBuf,
    pub repo: PathBuf,
    pub name: String,
    pub branch: String,
    /// Changed or untracked files.
    pub dirty: u32,
    /// Commits on HEAD that no remote branch has.
    pub unpushed: u32,
    pub modified_ms: u64,
    pub error: Option<String>,
}

impl Worktree {
    /// :return: bool whether `git worktree remove` (without --force) will accept it; the branch and its commits stay
    pub fn removable(&self) -> bool {
        self.error.is_none() && self.dirty == 0
    }
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn inspect(repo: &Path, path: PathBuf) -> Worktree {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let modified_ms = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    let mut wt = Worktree {
        path,
        repo: repo.to_path_buf(),
        name,
        branch: String::new(),
        dirty: 0,
        unpushed: 0,
        modified_ms,
        error: None,
    };
    let state = (|| -> Result<(String, u32, u32), String> {
        let branch = match git(&wt.path, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim() {
            "HEAD" => "(detached)".to_string(),
            b => b.to_string(),
        };
        let dirty = git(&wt.path, &["status", "--porcelain"])?.lines().count() as u32;
        let unpushed = git(&wt.path, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?.trim().parse().unwrap_or(0);
        Ok((branch, dirty, unpushed))
    })();
    match state {
        Ok((b, d, u)) => (wt.branch, wt.dirty, wt.unpushed) = (b, d, u),
        Err(e) => wt.error = Some(e.lines().next().unwrap_or("git failed").to_string()),
    }
    wt
}

/// :param roots: list of repo roots; :param in_use: cwds of running or unarchived sessions; :return: list[Worktree] newest first
pub fn scan(roots: &[String], in_use: &[String]) -> Vec<Worktree> {
    let mut found = Vec::new();
    for root in roots {
        let repo = Path::new(root);
        let Ok(rd) = std::fs::read_dir(repo.join(".claude/worktrees")) else { continue };
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_dir() {
                continue;
            }
            let prefix = p.to_string_lossy().into_owned();
            let used = in_use.iter().any(|c| c == &prefix || c.starts_with(&format!("{prefix}/")));
            if !used {
                found.push((repo.to_path_buf(), p));
            }
        }
    }
    let mut out: Vec<Worktree> = std::thread::scope(|sc| {
        let jobs: Vec<_> = found.into_iter().map(|(repo, p)| sc.spawn(move || inspect(&repo, p))).collect();
        jobs.into_iter().filter_map(|j| j.join().ok()).collect()
    });
    out.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    out
}

/// Run `git worktree remove` without --force, so git refuses a worktree with changes.
pub fn remove(wt: &Worktree) -> Result<(), String> {
    let path = wt.path.to_string_lossy();
    git(&wt.repo, &["worktree", "remove", &path]).map(|_| ())
}
