# weft

A live terminal board for every Claude Code session on your machine: what waits for you, what works, what is idle,
and a jump to any of them.

weft has four panes; `b`, `m`, `h`, `c` go straight to one, `tab` and `shift-tab` step through them. Movement, `enter` (open or
jump), `f` filter, `d` detail, `t` terminate, `?` help and `q` quit mean the same on every pane where they apply.

| view | shows |
|---|---|
| **board** | live sessions as numbered cards in three sections: *waiting for you* (the desktop app's turn summary says blocked or ready for review), *working*, *idle*. Each card shows what the session does now, what it needs, or its last reply. `enter` or `1`–`9` jumps to the session. |
| **monitor** | token graphs, totals and the full session table (below). |
| **history** | past sessions grouped by project, worktrees folded into their repo (tapestry's board as a list). Status, branch, PR and source come from the desktop app's metadata where it exists. `enter` opens a desktop session in Claude, or copies `cd … && claude --resume <id>` for a terminal session. `a` shows archived sessions. |
| **cleanup** | agent worktrees (`<repo>/.claude/worktrees/*`) that no running session and no unarchived desktop session uses, with branch, changed files and commits no remote has. `x` runs `git worktree remove` after a confirmation; git refuses a worktree with changes, and the branch stays. |

Jump (macOS, best effort): desktop sessions open through `claude://code/continue?session=local_…`. Terminal
sessions select the tmux pane, iTerm2 session or Terminal tab on the process's tty (iTerm2 and Terminal ask once
for Automation permission), then activate the app that hosts the process; editors open the project; anything else
opens the folder.

The desktop metadata is read from `~/Library/Application Support/Claude/claude-code-sessions` (`--metadata` to
change it). Live sessions join it on the registry's `hostSessionId`, past ones on `cliSessionId`. weft never writes
to it.

```
 board  monitor  history  cleanup ▏ 5 sessions 2 busy 3 idle ▏ ⟳ 0.9ms/500ms ▏ 5h ■■■■■□□□□□ 48% ↻2h13m week ■■■■■■■□□□ 71% ↻3d4h  16:02:20
╭┤ tokens/s ├─────────────────────────────────┤ 240 samples · 54.1k/s ├╮╭┤ totals ├──────────────────────────────╮
│ ↓ in  54.1k/s   top 426.8k/s         ⣀⣀⣠⣤⣶⣿⣿⣿⣿⣷⣤⣀                  ││in      32.6M ■■■■■■■■■■■■■■■ cache  97% │
│                                   ⣀⣠⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣄                 ││out    245.4k · agents 0 active          │
│ ↑ out 120/s     top 1.2k/s           ⠉⠛⠿⠿⠿⠛⠉                        ││ctx    880.1k ■■■■■■■■■■□□□□□  88% of 1M │
╰──────────────────────────────────────────────────────────────────────╯╰────────────────────────────────────────╯
╭┤ sessions ├───────────────────────────────────────────────────────────────────────────┤ sort: activity ▼ ├╮
│    pid st      name                    project               model      ctx            in    out  cpu%   mem   last │
│  62610 ⚙ tool  Claude Code live monitor weft                 fable-5.1  ■■■■□□□  22%  1.5M    93k   393  362M    6s │
│  58301 ● busy  Retry backoff for API…  acme-api              fable-5.1  ■■□□□□□  30%   22M  96.5k     0  304M    2s │
│  57733 ○ idle  Search index rebuild …  docs-site             fable-5.1  ■□□□□□□  18%  6.6M    41k     0  196M  1h31m │
╰──────────────────────────────────────────────────────────────────────────────────────────────────────────────────╯
╭┤ Claude Code live monitor ├──────────────────────────────────────────────────────────────────────┤ pid 62610 ├╮
│cwd    ~/src/weft · v2.1.293 · desktop · up 21m · since 15:43                       │ ↓in 12.3k/s       ⢸⣿⣷⣄     │
│model  fable-5.1 · effort xhigh · ctx ■■■■□□□□□□□□□□□□□□□□ 223.7k / 1M  22%          │                   ⢸⣿⣿⣿⣷⣄   │
│tokens in 2M (cache 91%) · out 109.3k · thinking 10m                                │                            │
│work   turns 15 · prompts 2 · tools 33 (Bash 28, Skill 1, ToolSearch 1) · 1 errors  │                            │
│proc   cpu 1% (+393% in 6 children) · rss 347M · transcript 1M                      │                            │
│now    ⚙ Bash  cargo build --release  · 6s  · +1 more                               │ ↑out 40/s         ⠈⠛⠿⠟⠁    │
│prompt Build btop but for claude code; a btop-style live monitor for all your concu… │                            │
│reply  Build succeeded. Next I'll add a `--dump` one-shot mode to verify parsed num… │                            │
╰───────────────────────────────────────────────────────────────┤ 0c1d2e3f-4a5b-4c6d-8e7f-9a0b1c2d3e4f ├╯
 enter jump to session  d detail  t term  g graphs  s sort:activity  r reverse  a ended  +- 500ms  f filter  bmhc pane  ? help  q quit
```

## The monitor pane

- **sessions** — every live Claude Code process: pid, status (`● busy`, `⚙ tool` while a tool runs, `○ idle`, `✕ ended`), name, project (worktrees shown as `repo:worktree`), git branch, model, context-window meter, input/output tokens, cache hit %, tool calls, turns, CPU (process + its tool subprocesses), RSS, time since last activity, uptime. Columns drop out gracefully on narrow terminals.
- **tokens/s** — btop-style dual braille graph: input tokens processed per second (incl. cache reads) growing up, output tokens per second growing down. Auto-scaled, smoothed over 4 s.
- **totals** — tokens, cache ratio, aggregate context pressure, turns, tool calls, errors, running tools, active subagents, CPU and RSS across all sessions.
- **detail** — the selected session: cwd, branch, version, entrypoint, uptime, model, effort, context meter, token breakdown (incl. subagent tokens), thinking time, tool histogram, errors, subagents, child-process CPU, what it is doing *right now* (running tool with its command/path, generating, or idle), the last prompt and the last reply, plus a per-session throughput graph.
- **header** — session counts, collector time per tick (typically under 2 ms), how close your subscription is to its usage limits (the rolling 5-hour window and the 7-day week, each with the time until it resets; greyed out when the last poll is older than 5 minutes, `!` with a reason when polling fails), clock.

Sessions that exit stay visible, greyed out, for 60 s (press `a` to keep them).

## Install

Requires a Rust toolchain (`brew install rust` or [rustup](https://rustup.rs)).

```bash
cd weft
cargo install --path .     # -> ~/.cargo/bin/weft
```

or just run the release binary: `cargo build --release && ./target/release/weft`.

## Usage

```
weft [OPTIONS]

  -i, --interval <MS>     refresh interval in ms            [default: 500, env WEFT_INTERVAL_MS]
  -c, --context <TOKENS>  override context window (e.g. 1m) [default: per model, env WEFT_CONTEXT]
  -d, --dir <PATH>        Claude config dir                 [default: $CLAUDE_CONFIG_DIR or ~/.claude]
  -m, --metadata <PATH>   Claude desktop session metadata   [default: ~/Library/Application Support/Claude/claude-code-sessions]
      --dump              collect once, print a plain table and exit
      --no-usage          do not poll subscription usage limits    [env WEFT_USAGE=0]
```

Monitor keys (`?` in weft lists the keys of the current pane):

| key | action |
|---|---|
| `↑ ↓` `j k` `pgup/pgdn` `home/end` | move selection |
| `enter` | jump to the selected session |
| `d` | toggle detail pane |
| `g` | toggle token graphs |
| `s` `S` `← →` | cycle sort column (activity, pid, name, project, cpu, mem, ctx, tokens, tools, uptime) |
| `r` | reverse sort |
| `f` / `/` | filter by name, project, branch, model or pid (`esc` clears) |
| `a` | show / hide ended sessions |
| `t` | terminate the selected session (SIGTERM, asks first) |
| `+` `-` | refresh interval ± 100 ms |
| `?` | help |
| `q` `esc` `ctrl-c` | quit |

## How it works

No daemon, no hooks. Everything except the usage limits is read from what Claude Code already writes to disk:

| source | gives |
|---|---|
| `~/.claude/sessions/<pid>.json` | the live registry: pid, session id, cwd, name, busy/idle status, version, entrypoint |
| `~/.claude/projects/<cwd>/<session>.jsonl` | the transcript: per-message token usage, model, effort, tool calls and results, prompts, replies, git branch |
| `~/.claude/projects/<cwd>/<session>/subagents/*.jsonl` | subagent transcripts: their tokens and whether they are still running |
| the process table (sysinfo) | CPU/RSS of each session and its descendants; also finds `claude` binaries that are not in the registry |
| `~/.claude/weft-usage.json` | the 5-hour and weekly limit utilisation, mirrored by a statusline tap (below) |
| `api.anthropic.com/api/oauth/usage` | fallback for the same figures when the tap is older than 10 minutes — the call `/usage` makes, with the OAuth token Claude Code keeps in the macOS keychain (`Claude Code-credentials`) or `~/.claude/.credentials.json`; the token is passed to `curl` on stdin, never on the command line. Polled every 5 minutes at most and backs off for as long as a 429 asks (that endpoint has a small quota of its own). `--no-usage` turns both off |

### Usage limits

Claude Code hands every statusline command a `rate_limits` object on each render, with the same 5-hour and weekly percentages `/usage` shows. Mirroring it to a file is free, so weft prefers that over polling the API. Add this near the top of your statusline script (right after it reads stdin into `$input`):

```bash
{
  cc_dir=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
  cc_tmp="$cc_dir/weft-usage.json.$$"
  printf '%s' "$input" | jq -c 'select(.rate_limits != null) | {at: now, rate_limits}' > "$cc_tmp" 2>/dev/null
  if [[ -s $cc_tmp ]]; then mv -f "$cc_tmp" "$cc_dir/weft-usage.json"; else rm -f "$cc_tmp"; fi
} 2>/dev/null
```

Without a statusline (or when no session has rendered one for 10 minutes) weft falls back to the usage endpoint. `weft --dump` prints which source the numbers came from and how old they are; `WEFT_USAGE_DEBUG=/path/file` saves the last raw endpoint response (headers and body, no token) for troubleshooting.

Transcripts are tailed incrementally: only bytes appended since the last tick are read, and each line is pre-filtered with a SIMD substring search before any JSON is parsed, so multi-megabyte transcripts full of file snapshots cost microseconds. Split assistant entries (one per content block, sharing a message id) are de-duplicated so usage is counted once per API response. The collector runs on its own thread and hands an immutable snapshot to the renderer; the UI never blocks on I/O.

Context window percentages use 1M for current models and 200k for the 4.5 family and older; override with `--context`.

## License

MIT
