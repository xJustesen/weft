//! Bring a session's window to the front (macOS, best effort).
//!
//! - Claude desktop sessions open through the app's `claude://code/continue?session=local_…` link.
//! - Terminal sessions: find the process's tty, select the matching tmux pane, iTerm2 session or
//!   Terminal tab, then activate the app that hosts it (found by walking up the process tree).
//!   iTerm2 and Terminal ask once for macOS Automation permission.
//! - Anything else opens the project folder.

use std::process::Command;

pub struct Target {
    pub pid: u32,
    pub cwd: String,
    pub desktop_id: Option<String>,
    pub entrypoint: String,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn osascript(script: &str) -> Option<String> {
    run("osascript", &["-e", script])
}

/// `ps` gives `ttys012`; tmux and AppleScript use `/dev/ttys012`.
fn tty_of(pid: u32) -> Option<String> {
    let t = run("ps", &["-o", "tty=", "-p", &pid.to_string()])?;
    (!t.is_empty() && !t.starts_with('?')).then(|| format!("/dev/{t}"))
}

/// The `.app` bundle of the nearest ancestor process that lives in one.
fn host_app(pid: u32) -> Option<String> {
    let mut cur = pid;
    for _ in 0..16 {
        let line = run("ps", &["-o", "ppid=,comm=", "-p", &cur.to_string()])?;
        let (ppid, comm) = line.trim().split_once(' ')?;
        if cur != pid {
            if let Some(i) = comm.find(".app/") {
                return Some(comm[..i + 4].to_string());
            }
        }
        cur = ppid.trim().parse().ok()?;
        if cur <= 1 {
            return None;
        }
    }
    None
}

fn tmux_select(tty: &str) -> Option<String> {
    let panes = run("tmux", &["list-panes", "-a", "-F", "#{pane_tty} #{session_name}:#{window_index}.#{pane_index}"])?;
    let target = panes.lines().find_map(|l| l.strip_prefix(tty)?.strip_prefix(' '))?.to_string();
    run("tmux", &["switch-client", "-t", &target])?;
    run("tmux", &["select-window", "-t", &target]);
    run("tmux", &["select-pane", "-t", &target]);
    Some(target)
}

fn iterm_select(tty: &str) -> bool {
    let script = format!(
        r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s is "{tty}" then
          select w
          select t
          select s
          activate
          return "ok"
        end if
      end repeat
    end repeat
  end repeat
end tell"#
    );
    osascript(&script).as_deref() == Some("ok")
}

fn terminal_select(tty: &str) -> bool {
    let script = format!(
        r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" then
        set selected of t to true
        set index of w to 1
        activate
        return "ok"
      end if
    end repeat
  end repeat
end tell"#
    );
    osascript(&script).as_deref() == Some("ok")
}

/// :return: str what happened, for the status line
pub fn jump(t: &Target) -> String {
    if let Some(id) = &t.desktop_id {
        if t.entrypoint == "claude-desktop" {
            let url = format!("claude://code/continue?session={id}");
            return match run("open", &[&url]) {
                Some(_) => "opened in Claude".into(),
                None => "could not open the Claude app".into(),
            };
        }
    }
    let app = host_app(t.pid);
    if let Some(tty) = tty_of(t.pid) {
        let pane = tmux_select(&tty);
        let app_name = app.as_deref().unwrap_or("");
        if app_name.ends_with("iTerm.app") && iterm_select(&tty) {
            return "focused iTerm2 session".into();
        }
        if app_name.ends_with("Terminal.app") && terminal_select(&tty) {
            return "focused Terminal tab".into();
        }
        if let Some(app) = &app {
            run("open", &["-a", app]);
        }
        if let Some(p) = pane {
            return format!("selected tmux pane {p}");
        }
    }
    if let Some(app) = &app {
        if app.contains("Visual Studio Code") || app.contains("Cursor") || app.contains("Windsurf") || app.contains("Zed") {
            run("open", &["-a", app, &t.cwd]);
            return "opened project in editor".into();
        }
        run("open", &["-a", app]);
        let name = app.rsplit('/').next().unwrap_or(app).trim_end_matches(".app");
        return format!("activated {name}");
    }
    run("open", &[&t.cwd]);
    "opened project folder".into()
}

/// Copy text to the clipboard.
pub fn copy(text: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn() else { return false };
    let ok = child.stdin.take().is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
    child.wait().is_ok_and(|s| s.success()) && ok
}
