//! Command line entry.

use crate::app::{App, View};
use crate::collector::{Cmd, Config};
use crate::{collector, model, ui, usage, util};
use crossterm::event::{self, Event, KeyEventKind};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const USAGE: &str = "\
weft — live terminal views of your Claude Code sessions

USAGE:
    weft [OPTIONS]

OPTIONS:
    -i, --interval <MS>     refresh interval in milliseconds   [default: 500, env WEFT_INTERVAL_MS]
    -c, --context <TOKENS>  override context window for ctx %   [default: per model (1M, 200k for 4.5 and older), env WEFT_CONTEXT]
    -d, --dir <PATH>        Claude config dir                   [default: $CLAUDE_CONFIG_DIR or ~/.claude]
        --dump              collect once, print a plain-text table and exit (for scripts)
    -m, --metadata <PATH>   Claude desktop session metadata     [default: ~/Library/Application Support/Claude/claude-code-sessions]
        --no-usage          do not poll subscription usage limits (5h / weekly) [env WEFT_USAGE=0]
    -h, --help              print this help
    -V, --version           print version

PANES (b m h c, or tab / shift-tab):
    board     live sessions grouped by waiting / working / idle; 1–9 jumps to a card
    monitor   token graphs, totals and the full session table
    history   past sessions by project, from transcripts and Claude desktop metadata
    cleanup   agent worktrees with no live session, with git state; x removes a clean one

KEYS (the same on every pane):
    ↑↓/jk select   enter open / jump to session   f filter   d detail   t terminate
    ? help (lists the keys of the current pane)    q quit
";

/// :param prog: str program name for messages; :param default_view: View the screen to open on
pub fn main(prog: &str, default_view: View) -> std::io::Result<()> {
    let mut interval_ms: u64 = std::env::var("WEFT_INTERVAL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
    let mut ctx: Option<u64> = std::env::var("WEFT_CONTEXT").ok().and_then(|v| parse_tokens(&v));
    let mut dir: Option<PathBuf> = None;
    let mut desktop_dir = crate::desktop::default_dir();
    let mut dump = false;
    let mut usage = std::env::var("WEFT_USAGE").map_or(true, |v| !matches!(v.as_str(), "0" | "false" | "off"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("{prog} {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-i" | "--interval" => {
                interval_ms = args.next().and_then(|v| v.parse().ok()).unwrap_or_else(|| die("--interval needs a number"));
            }
            "-c" | "--context" => {
                ctx = Some(args.next().and_then(|v| parse_tokens(&v)).unwrap_or_else(|| die("--context needs a number like 200000 or 1m")));
            }
            "--dump" => dump = true,
            "--no-usage" => usage = false,
            "-d" | "--dir" => {
                dir = Some(PathBuf::from(args.next().unwrap_or_else(|| die("--dir needs a path"))));
            }
            "-m" | "--metadata" => {
                desktop_dir = PathBuf::from(args.next().unwrap_or_else(|| die("--metadata needs a path")));
            }
            other => die(&format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    let claude_dir = dir
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))
        .unwrap_or_else(|| die("cannot determine Claude config dir; pass --dir"));
    if !claude_dir.is_dir() {
        die(&format!("{} does not exist", claude_dir.display()));
    }
    let interval = Duration::from_millis(interval_ms.clamp(100, 60_000));
    let cfg = Config {
        claude_dir,
        interval,
        ctx_limit: ctx.map(|c| c.max(1)),
        history: 600,
        keep_ended: Duration::from_secs(600),
        usage,
        desktop_dir,
    };

    let (snap_tx, snap_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("collector".into())
        .spawn(move || collector::run(cfg, snap_tx, cmd_rx))
        .expect("spawn collector");

    if dump {
        return dump_once(&snap_rx, &cmd_tx, usage);
    }
    let mut terminal = ratatui::init();
    let mut app = App::new(cmd_tx.clone(), interval, default_view);
    let res = run_loop(&mut terminal, &mut app, &snap_rx);
    ratatui::restore();
    let _ = cmd_tx.send(Cmd::Quit);
    res
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    rx: &mpsc::Receiver<model::Snapshot>,
) -> std::io::Result<()> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;
    loop {
        while let Ok(s) = rx.try_recv() {
            app.apply(s);
            dirty = true;
        }
        dirty |= app.poll_background();
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            terminal.draw(|f| ui::draw(f, app))?;
            last_draw = Instant::now();
            dirty = false;
        }
        if event::poll(Duration::from_millis(60))? {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    app.on_key(k);
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
        }
        if app.quit {
            return Ok(());
        }
    }
}

/// Wait for the first fully-loaded snapshot and print it as text.
fn dump_once(rx: &mpsc::Receiver<model::Snapshot>, cmd_tx: &mpsc::Sender<Cmd>, want_usage: bool) -> std::io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = None;
    while Instant::now() < deadline {
        let Ok(s) = rx.recv_timeout(Duration::from_secs(5)) else { break };
        let settled = s.tick >= 2 && s.sessions.iter().all(|x| !x.loading) && (!want_usage || s.usage.is_some());
        last = Some(s);
        if settled {
            break;
        }
    }
    let _ = cmd_tx.send(Cmd::Quit);
    let Some(s) = last else {
        die("no snapshot received");
    };
    println!(
        "{:<6} {:<6} {:<28} {:<22} {:<12} {:>7} {:>8} {:>7} {:>5} {:>5} {:>4} {:>5} {:>6}  {}",
        "pid", "status", "name", "project", "model", "ctx", "in", "out", "turns", "tools", "err", "cpu%", "rss", "now"
    );
    for v in &s.sessions {
        let now = v.running.first().map(|r| format!("{} {}", r.name, r.summary)).unwrap_or_default();
        println!(
            "{:<6} {:<6} {:<28} {:<22} {:<12} {:>7} {:>8} {:>7} {:>5} {:>5} {:>4} {:>5.0} {:>6}  {}",
            v.pid,
            v.status.label(),
            util::truncate(&v.name, 28),
            util::truncate(&v.project, 22),
            util::short_model(&v.model),
            v.ctx_tokens,
            v.input_total(),
            v.output_tokens,
            v.turns,
            v.tool_calls,
            v.tool_errors,
            v.cpu + v.child_cpu,
            util::human_bytes(v.mem),
            util::truncate(&now, 60),
        );
    }
    println!(
        "-- {} sessions, {} busy · in {} out {} · collect {}µs",
        s.totals.sessions, s.totals.busy, s.totals.input_total, s.totals.output_total, s.collect_us
    );
    if let Some(u) = &s.usage {
        let win = |name: &str, w: &Option<usage::Window>| match w {
            Some(w) => {
                let reset = w
                    .resets_in_ms(s.at_ms)
                    .map(|ms| format!(" (resets in {})", util::human_dur(ms)))
                    .unwrap_or_default();
                format!("{name} {:.0}%{reset}", w.used_pct)
            }
            None => format!("{name} n/a"),
        };
        let mut line = format!("-- limits: {} · {}", win("5h", &u.five_hour), win("week", &u.seven_day));
        for (k, w) in &u.extra {
            line.push_str(&format!(" · {} {:.0}%", k, w.used_pct));
        }
        if u.fetched_at_ms > 0 {
            line.push_str(&format!(" · via {} {} ago", u.source, util::human_dur(s.at_ms.saturating_sub(u.fetched_at_ms))));
        }
        if let Some(e) = &u.error {
            line.push_str(&format!(" · error: {e}"));
        }
        println!("{line}");
    }
    Ok(())
}

fn parse_tokens(v: &str) -> Option<u64> {
    let v = v.trim().to_lowercase();
    if let Some(n) = v.strip_suffix('m') {
        return n.parse::<f64>().ok().map(|x| (x * 1_000_000.0) as u64);
    }
    if let Some(n) = v.strip_suffix('k') {
        return n.parse::<f64>().ok().map(|x| (x * 1_000.0) as u64);
    }
    v.replace('_', "").parse().ok()
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}
