//! Screen layout: header, token graphs, totals, session table, detail pane, footer, overlays.

mod board;
mod cleanup;
mod history;
pub mod theme;
pub mod widgets;

use crate::app::{App, Overlay, View};
use crate::model::{SessionView, Status};
use crate::util::{fit, human_bytes, human_dur, human_num, human_rate, pct, short_model, shorten_path, truncate};
use chrono::{DateTime, Local};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use theme::*;
use widgets::*;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if area.width < 40 || area.height < 8 {
        f.render_widget(Paragraph::new("terminal too small").style(fg(WARN)), area);
        return;
    }
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(4), Constraint::Length(1)]).areas(area);
    draw_header(f, head, app);
    match app.view {
        View::Board => board::draw(f, body, app),
        View::Monitor => draw_monitor(f, body, app),
        View::History => history::draw(f, body, app),
        View::Cleanup => cleanup::draw(f, body, app),
    }
    draw_footer(f, foot, app);
    match app.overlay.clone() {
        Overlay::Help => draw_help(f, area, app.view),
        Overlay::Kill(pid, name) => draw_confirm(f, area, pid, &name),
        Overlay::Remove(path) => cleanup::draw_confirm(f, area, &path),
        Overlay::None => {}
    }
}

fn draw_monitor(f: &mut Frame, area: Rect, app: &mut App) {
    let small = area.height < 28;
    let graphs_h: u16 = if app.show_graphs { if small { 7 } else { 9 } } else { 0 };
    let detail_h: u16 = if app.show_detail { if small { 7 } else { 10 } } else { 0 };
    let chunks = Layout::vertical([Constraint::Length(graphs_h), Constraint::Min(4), Constraint::Length(detail_h)]).split(area);
    if graphs_h > 0 {
        draw_graphs(f, chunks[0], app);
    }
    draw_sessions(f, chunks[1], app);
    if detail_h > 0 {
        draw_detail(f, chunks[2], app);
    }
}

fn status_span(s: &SessionView) -> Span<'static> {
    match &s.status {
        Status::Busy if !s.running.is_empty() => Span::styled("⚙ tool", bold(BUSY)),
        Status::Busy => Span::styled("● busy", bold(BUSY)),
        Status::Idle => Span::styled("○ idle", fg(IDLE)),
        Status::Ended => Span::styled("✕ ended", fg(INACTIVE_FG)),
        Status::Other(o) => Span::styled(format!("◐ {}", truncate(o, 5)), bold(WARN)),
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let s = &app.snap;
    let t = &s.totals;
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for v in View::ALL {
        let (k, rest) = v.name().split_at(1);
        let (ks, st) = if v == app.view {
            (bold(HI_FG).bg(SELECTED_BG).add_modifier(Modifier::UNDERLINED), bold(SELECTED_FG).bg(SELECTED_BG))
        } else {
            (bold(HI_FG).add_modifier(Modifier::UNDERLINED), dim())
        };
        spans.extend([Span::styled(" ", st), Span::styled(k.to_string(), ks), Span::styled(format!("{rest} "), st)]);
    }
    let waiting = s.sessions.iter().filter(|x| x.board() == crate::model::Board::Waiting && x.status != Status::Ended).count();
    spans.push(Span::styled(" ▏", fg(DIV_LINE)));
    if waiting > 0 {
        spans.push(Span::styled(format!(" {waiting} "), bold(WARN)));
        spans.push(Span::styled("waiting ", dim()));
    }
    spans.extend([
        Span::styled(format!(" {} ", t.sessions - t.ended), bold(MAIN_FG)),
        Span::styled("sessions ", dim()),
        Span::styled(format!("{} ", t.busy), bold(BUSY)),
        Span::styled("busy ", dim()),
        Span::styled(format!("{} ", t.idle), fg(IDLE)),
        Span::styled("idle ", dim()),
    ]);
    if t.ended > 0 {
        spans.push(Span::styled(format!("{} ", t.ended), fg(INACTIVE_FG)));
        spans.push(Span::styled("ended ", dim()));
    }
    if t.subagents_active > 0 {
        spans.push(Span::styled(format!("{} ", t.subagents_active), bold(ACCENT)));
        spans.push(Span::styled("agents ", dim()));
    }
    spans.push(Span::styled("▏", fg(DIV_LINE)));
    let collect = if s.collect_us >= 1000 {
        format!("{:.1}ms", s.collect_us as f64 / 1000.0)
    } else {
        format!("{}µs", s.collect_us)
    };
    spans.push(Span::styled(" ⟳ ", dim()));
    spans.push(Span::styled(collect, fg(PROC_MISC)));
    spans.push(Span::styled(format!("/{}ms ", app.interval.as_millis()), dim()));
    spans.push(Span::styled("▏", fg(DIV_LINE)));
    // subscription usage limits: rolling 5-hour window and 7-day week
    usage_spans(&mut spans, s);
    if let Some(msg) = app.flash_text() {
        spans.push(Span::styled("▏", fg(DIV_LINE)));
        spans.push(Span::styled(format!(" {msg} "), bold(WARN)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
    let clock = Local::now().format("%H:%M:%S ").to_string();
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(clock, bold(TITLE)))).alignment(Alignment::Right),
        area,
    );
}

/// `5h ■■■■■■□□□□ 62% ↻2h13m  week ■■■□□□□□□□ 31% ↻3d4h` — how close the
/// account is to its limits, green→red, with the time until each window resets.
fn usage_spans(spans: &mut Vec<Span<'static>>, s: &crate::model::Snapshot) {
    let Some(u) = &s.usage else {
        spans.push(Span::styled(" limits ", dim()));
        spans.push(Span::styled("… ", fg(GRAPH_TEXT)));
        return;
    };
    let now = s.at_ms;
    let stale = u.stale(now);
    for (name, w) in [("5h", &u.five_hour), ("week", &u.seven_day)] {
        spans.push(Span::styled(format!(" {name} "), dim()));
        let Some(w) = w else {
            spans.push(Span::styled("n/a ", fg(GRAPH_TEXT)));
            continue;
        };
        let frac = w.frac();
        let color = if stale { GRAPH_TEXT } else { grade(&GRAD_CPU, frac) };
        if stale {
            spans.extend(meter(10, frac, &[(0x50, 0x50, 0x50), (0x60, 0x60, 0x60), (0x70, 0x70, 0x70)]));
        } else {
            spans.extend(meter(10, frac, &GRAD_CPU));
        }
        spans.push(Span::styled(format!(" {:>3.0}% ", w.used_pct), bold(color)));
        if let Some(left) = w.resets_in_ms(now) {
            spans.push(Span::styled(format!("↻{} ", human_dur(left)), fg(GRAPH_TEXT)));
        }
    }
    if let Some(e) = &u.error {
        spans.push(Span::styled(format!("! {e} "), fg(WARN)));
    } else if stale {
        spans.push(Span::styled("stale ", fg(WARN)));
    }
}

/// Dual in/out braille graph with overlaid labels.
fn dual_graph(buf: &mut Buffer, area: Rect, hist_in: &[f32], hist_out: &[f32], compact: bool) {
    if area.height < 2 {
        return;
    }
    let top_h = (area.height + 1) / 2;
    let top = Rect::new(area.x, area.y, area.width, top_h);
    let bot = Rect::new(area.x, area.y + top_h, area.width, area.height - top_h);
    let max_in = window_max(hist_in, area.width, 50.0);
    let max_out = window_max(hist_out, area.width, 10.0);
    braille_graph(buf, top, hist_in, max_in, false, &GRAD_IN);
    braille_graph(buf, bot, hist_out, max_out, true, &GRAD_OUT);
    let cur_in = hist_in.last().copied().unwrap_or(0.0);
    let cur_out = hist_out.last().copied().unwrap_or(0.0);
    let (li, lo) = if compact {
        (
            format!("↓in {}/s", human_rate(cur_in)),
            format!("↑out {}/s", human_rate(cur_out)),
        )
    } else {
        (
            format!("↓ in  {}/s   top {}/s", human_rate(cur_in), human_rate(max_in)),
            format!("↑ out {}/s   top {}/s", human_rate(cur_out), human_rate(max_out)),
        )
    };
    label(buf, top, &li, bold(PURPLE));
    let bl = Rect::new(bot.x, bot.y + bot.height.saturating_sub(1), bot.width, 1);
    label(buf, bl, &lo, bold(PINK));
}

fn draw_graphs(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).split(area);
    // --- tokens/s graph ---
    let s = &app.snap;
    let total_rate: f32 = s.hist_in.last().copied().unwrap_or(0.0) + s.hist_out.last().copied().unwrap_or(0.0);
    let block = boxed("tokens/s", NET_BOX).title_top(
        tab(
            format!("{} samples · {}/s", s.hist_in.len(), human_rate(total_rate)),
            NET_BOX,
            dim(),
        )
        .right_aligned(),
    );
    let inner = block.inner(cols[0]);
    f.render_widget(block, cols[0]);
    dual_graph(f.buffer_mut(), inner, &s.hist_in, &s.hist_out, inner.width < 40);

    // --- totals ---
    let t = &s.totals;
    let block = boxed("totals", MEM_BOX);
    let inner = block.inner(cols[1]);
    f.render_widget(block, cols[1]);
    let mw = (inner.width as usize).saturating_sub(26).clamp(4, 24);
    let mut lines: Vec<Line> = Vec::new();
    let cache_frac = pct(t.cache_read, t.input_total);
    let mut l = kv("in     ", fit(&human_num(t.input_total), 6, true));
    l.push(Span::raw(" "));
    l.extend(meter(mw, cache_frac, &GRAD_CACHED));
    l.push(Span::styled(format!(" cache {:>3.0}%", cache_frac * 100.0), dim()));
    lines.push(Line::from(l));
    let mut l = kv("out    ", fit(&human_num(t.output_total), 6, true));
    l.push(sep());
    l.extend(kv("agents", format!("{} active", t.subagents_active)));
    if t.sub_output > 0 || t.sub_input > 0 {
        l.push(Span::styled(
            format!(" ({}/{})", human_num(t.sub_input), human_num(t.sub_output)),
            dim(),
        ));
    }
    lines.push(Line::from(l));
    let ctx_frac = pct(t.ctx_tokens, t.ctx_limit);
    let mut l = kv("ctx    ", fit(&human_num(t.ctx_tokens), 6, true));
    l.push(Span::raw(" "));
    l.extend(meter(mw, ctx_frac, &GRAD_USED));
    l.push(Span::styled(format!(" avg {:>3.0}%", ctx_frac * 100.0), dim()));
    lines.push(Line::from(l));
    let mut l = kv("turns  ", fit(&t.turns.to_string(), 6, true));
    l.push(sep());
    l.extend(kv("tools", t.tool_calls.to_string()));
    if t.tool_errors > 0 {
        l.push(sep());
        l.push(Span::styled(format!("{} err", t.tool_errors), fg(ERR)));
    }
    lines.push(Line::from(l));
    let mut l = kv("cpu    ", fit(&format!("{:.0}%", t.cpu), 6, true));
    l.push(sep());
    l.extend(kv("rss", human_bytes(t.mem)));
    l.push(sep());
    l.push(Span::styled("⚙ ", bold(BUSY)));
    l.extend(kv("", format!("{} running", t.running_tools)));
    lines.push(Line::from(l));
    let mut l = kv("cache  ", fit(&human_num(t.cache_read), 6, true));
    l.push(Span::styled(" read", dim()));
    l.push(sep());
    l.extend(kv("", human_num(t.cache_write)));
    l.push(Span::styled(" written", dim()));
    lines.push(Line::from(l));
    lines.push(Line::from(vec![
        Span::styled("watch  ", dim()),
        Span::styled(shorten_path(&s.claude_dir), fg(GRAPH_TEXT)),
    ]));
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), inner);
}

struct Col {
    name: &'static str,
    width: u16,
    right: bool,
    prio: u8,
}

const COLS: &[Col] = &[
    Col { name: "pid", width: 6, right: true, prio: 1 },
    Col { name: "st", width: 7, right: false, prio: 1 },
    Col { name: "name", width: 14, right: false, prio: 0 }, // flex
    Col { name: "project", width: 20, right: false, prio: 2 },
    Col { name: "branch", width: 14, right: false, prio: 5 },
    Col { name: "model", width: 11, right: false, prio: 3 },
    Col { name: "ctx", width: 12, right: false, prio: 2 },
    Col { name: "in", width: 6, right: true, prio: 3 },
    Col { name: "out", width: 6, right: true, prio: 3 },
    Col { name: "cache", width: 5, right: true, prio: 6 },
    Col { name: "tools", width: 5, right: true, prio: 4 },
    Col { name: "turns", width: 5, right: true, prio: 6 },
    Col { name: "cpu%", width: 5, right: true, prio: 2 },
    Col { name: "mem", width: 6, right: true, prio: 2 },
    Col { name: "last", width: 6, right: true, prio: 2 },
    Col { name: "up", width: 6, right: true, prio: 4 },
];

/// Choose which columns fit and how wide the flexible name column is.
fn layout_cols(width: u16) -> (Vec<usize>, u16) {
    let mut best: Option<(Vec<usize>, u16)> = None;
    for max_prio in (0..=6).rev() {
        let idx: Vec<usize> = COLS.iter().enumerate().filter(|(_, c)| c.prio <= max_prio).map(|(i, _)| i).collect();
        let fixed: u16 = idx.iter().map(|&i| COLS[i].width + 1).sum::<u16>() + 1;
        if fixed <= width {
            let name_extra = width - fixed;
            best = Some((idx, COLS[2].width + name_extra));
            break;
        }
    }
    best.unwrap_or_else(|| (vec![0, 1, 2], COLS[2].width))
}

fn cell(text: String, col: &Col, w: u16, style: Style) -> Span<'static> {
    Span::styled(fit(&text, w as usize, col.right), style)
}

fn draw_sessions(f: &mut Frame, area: Rect, app: &mut App) {
    let rows = app.rows();
    let sort_label = format!(
        "sort: {} {}",
        app.sort.name(),
        if app.reverse { "▲" } else { "▼" }
    );
    let mut block = boxed("sessions", PROC_BOX).title_top(tab(sort_label, PROC_BOX, dim()).right_aligned());
    if app.filtering || !app.filter.is_empty() {
        let txt = if app.filtering {
            format!("filter: {}_", app.filter)
        } else {
            format!("filter: {}", app.filter)
        };
        block = block.title_top(tab(txt, PROC_BOX, bold(WARN)).right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 || inner.width < 10 {
        return;
    }
    let (cols, name_w) = layout_cols(inner.width);
    let widths: Vec<u16> = cols.iter().map(|&i| if i == 2 { name_w } else { COLS[i].width }).collect();

    // header
    let mut hdr: Vec<Span> = vec![Span::raw(" ")];
    for (k, &ci) in cols.iter().enumerate() {
        let c = &COLS[ci];
        let is_sort = c.name == app.sort.name()
            || (c.name == "st" && app.sort.name() == "activity")
            || (c.name == "in" && app.sort.name() == "tokens")
            || (c.name == "up" && app.sort.name() == "uptime")
            || (c.name == "cpu%" && app.sort.name() == "cpu");
        let st = if is_sort { bold(HI_FG) } else { bold(TITLE) };
        hdr.push(cell(c.name.to_string(), c, widths[k], st));
        hdr.push(Span::raw(" "));
    }
    f.render_widget(Paragraph::new(Line::from(hdr)), Rect::new(inner.x, inner.y, inner.width, 1));

    let body = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    if rows.is_empty() {
        let msg = if !app.has_data() {
            "scanning…"
        } else if !app.filter.is_empty() {
            "no sessions match the filter"
        } else {
            "no Claude Code sessions running"
        };
        let r = Rect::new(body.x, body.y + body.height / 2, body.width, 1);
        f.render_widget(Paragraph::new(msg).style(dim()).alignment(Alignment::Center), r);
        return;
    }
    // keep selection in view
    let sel = app.selected_index().unwrap_or(0);
    let visible = body.height as usize;
    let mut offset = app.table_offset;
    if sel < offset {
        offset = sel;
    } else if sel >= offset + visible {
        offset = sel + 1 - visible;
    }
    if offset > rows.len().saturating_sub(1) {
        offset = rows.len().saturating_sub(1);
    }
    let now = app.snap.at_ms;
    let buf = f.buffer_mut();
    for (ri, s) in rows.iter().enumerate().skip(offset).take(visible) {
        let y = body.y + (ri - offset) as u16;
        let selected = ri == sel;
        let base = if selected {
            Style::new().fg(SELECTED_FG).bg(SELECTED_BG)
        } else if s.status == Status::Ended {
            fg(INACTIVE_FG)
        } else if s.status == Status::Idle {
            fg(DIM)
        } else {
            main()
        };
        let mut spans: Vec<Span> = vec![Span::styled(" ", base)];
        for (k, &ci) in cols.iter().enumerate() {
            let c = &COLS[ci];
            let w = widths[k];
            let sp = match c.name {
                "pid" => cell(s.pid.to_string(), c, w, base),
                "st" => {
                    let mut sp = status_span(s);
                    if selected {
                        sp.style = sp.style.bg(SELECTED_BG);
                    } else if s.status == Status::Ended {
                        sp.style = fg(INACTIVE_FG);
                    }
                    Span::styled(fit(&sp.content, w as usize, false), sp.style)
                }
                "name" => {
                    let st = if selected { bold(SELECTED_FG).bg(SELECTED_BG) } else if s.status == Status::Busy { bold(TITLE) } else { base };
                    cell(s.name.clone(), c, w, st)
                }
                "project" => cell(s.project.clone(), c, w, base),
                "branch" => cell(s.branch.clone(), c, w, base),
                "model" => cell(short_model(&s.model), c, w, base),
                "ctx" => {
                    let frac = pct(s.ctx_tokens, s.ctx_limit);
                    let mut v = meter(7, frac, &GRAD_USED);
                    for sp in v.iter_mut() {
                        if selected {
                            sp.style = sp.style.bg(SELECTED_BG);
                        }
                    }
                    v.push(Span::styled(format!(" {:>3.0}%", frac * 100.0), base));
                    spans.extend(v);
                    spans.push(Span::styled(" ", base));
                    continue;
                }
                "in" => cell(human_num(s.input_total()), c, w, base),
                "out" => cell(human_num(s.output_tokens), c, w, base),
                "cache" => cell(format!("{:.0}%", s.cache_hit() * 100.0), c, w, base),
                "tools" => cell(s.tool_calls.to_string(), c, w, base),
                "turns" => cell(s.turns.to_string(), c, w, base),
                "cpu%" => {
                    let cpu = s.cpu + s.child_cpu;
                    let st = if selected || s.status == Status::Ended { base } else { fg(grade(&GRAD_CPU, cpu / 100.0)) };
                    cell(format!("{cpu:.0}"), c, w, st)
                }
                "mem" => cell(human_bytes(s.mem), c, w, base),
                "last" => {
                    let t = if s.last_activity_ms == 0 { "-".into() } else { human_dur(now.saturating_sub(s.last_activity_ms)) };
                    cell(t, c, w, base)
                }
                "up" => {
                    let t = if s.started_at_ms == 0 { "-".into() } else { human_dur(now.saturating_sub(s.started_at_ms)) };
                    cell(t, c, w, base)
                }
                _ => cell(String::new(), c, w, base),
            };
            spans.push(sp);
            spans.push(Span::styled(" ", base));
        }
        let line = Line::from(spans);
        if selected {
            buf.set_style(Rect::new(body.x, y, body.width, 1), Style::new().bg(SELECTED_BG));
        }
        buf.set_line(body.x, y, &line, body.width);
    }
    drop(rows);
    app.table_offset = offset;
}

fn fmt_clock(ms: u64) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map(|d| d.with_timezone(&Local).format("%H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(s) = app.selected().cloned() else {
        let block = boxed("detail", CPU_BOX);
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new("select a session").style(dim()).alignment(Alignment::Center), inner);
        return;
    };
    let now = app.snap.at_ms;
    let title = truncate(&s.name, (area.width as usize).saturating_sub(30).max(10));
    let mut block = boxed(&title, CPU_BOX).title_top(
        tab(format!("pid {}", s.pid), CPU_BOX, dim()).right_aligned(),
    );
    if !s.session_id.is_empty() {
        block = block.title_bottom(tab(s.session_id.clone(), CPU_BOX, fg(GRAPH_TEXT)).right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 20 || inner.height == 0 {
        return;
    }
    let graph_w: u16 = if inner.width >= 90 { 32 } else if inner.width >= 70 { 24 } else { 0 };
    let text_w = inner.width - graph_w;
    let text_area = Rect::new(inner.x, inner.y, text_w, inner.height);

    let mut lines: Vec<Line> = Vec::new();
    // 1: where
    let mut l = kv("cwd   ", shorten_path(&s.cwd));
    if !s.branch.is_empty() {
        l.push(Span::styled("  ⎇ ", dim()));
        l.push(Span::styled(s.branch.clone(), fg(ACCENT)));
    }
    l.push(sep());
    l.push(Span::styled(
        format!(
            "v{} · {} · up {} · since {}",
            if s.version.is_empty() { "?".into() } else { s.version.clone() },
            s.entrypoint.replace("claude-", ""),
            human_dur(now.saturating_sub(s.started_at_ms)),
            fmt_clock(s.started_at_ms)
        ),
        dim(),
    ));
    lines.push(Line::from(l));
    // 2: model + ctx
    let frac = pct(s.ctx_tokens, s.ctx_limit);
    let mut l = kv("model ", if s.model.is_empty() { "-".into() } else { short_model(&s.model) });
    if let Some(e) = &s.effort {
        l.push(sep());
        l.extend(kv("effort", e.clone()));
    }
    l.push(sep());
    l.push(Span::styled("ctx ", dim()));
    l.extend(meter(20, frac, &GRAD_USED));
    l.push(Span::styled(
        format!(" {} / {}  ", human_num(s.ctx_tokens), human_num(s.ctx_limit)),
        main(),
    ));
    l.push(Span::styled(format!("{:.0}%", frac * 100.0), bold(grade(&GRAD_CPU, frac))));
    if s.compactions > 0 {
        l.push(Span::styled(format!("  {} compactions", s.compactions), dim()));
    }
    lines.push(Line::from(l));
    // 3: tokens
    let mut l = kv("tokens", format!("in {}", human_num(s.input_total())));
    l.push(Span::styled(format!(" (cache {:.0}%)", s.cache_hit() * 100.0), dim()));
    l.push(sep());
    l.extend(kv("out", human_num(s.output_tokens)));
    if s.sub_input > 0 || s.sub_output > 0 {
        l.push(sep());
        l.extend(kv("agents in", human_num(s.sub_input)));
        l.extend(kv(" out", human_num(s.sub_output)));
    }
    if s.thinking_ms > 0 {
        l.push(sep());
        l.extend(kv("thinking", human_dur(s.thinking_ms)));
    }
    lines.push(Line::from(l));
    // 4: work
    let tools: Vec<String> = s.tools_top.iter().take(5).map(|(n, c)| format!("{n} {c}")).collect();
    let mut l = kv("work  ", format!("turns {}", s.turns));
    l.push(sep());
    l.extend(kv("prompts", s.prompts.to_string()));
    l.push(sep());
    l.extend(kv("tools", s.tool_calls.to_string()));
    if !tools.is_empty() {
        l.push(Span::styled(format!(" ({})", tools.join(", ")), dim()));
    }
    if s.tool_errors > 0 {
        l.push(sep());
        l.push(Span::styled(format!("{} errors", s.tool_errors), fg(ERR)));
    }
    if s.subagents_total > 0 {
        l.push(sep());
        l.extend(kv("agents", format!("{}/{} active", s.subagents_active, s.subagents_total)));
    }
    lines.push(Line::from(l));
    // 5: proc
    let mut l = kv("proc  ", format!("cpu {:.0}%", s.cpu));
    if s.children > 0 {
        l.push(Span::styled(
            format!(" (+{:.0}% in {} child{})", s.child_cpu, s.children, if s.children == 1 { "" } else { "ren" }),
            dim(),
        ));
    }
    l.push(sep());
    l.extend(kv("rss", human_bytes(s.mem)));
    l.push(sep());
    if s.transcript_found {
        l.extend(kv("transcript", human_bytes(s.transcript_bytes)));
        if s.loading {
            l.push(Span::styled(" loading…", fg(WARN)));
        }
    } else {
        l.push(Span::styled("transcript not found yet", fg(WARN)));
    }
    if !s.registered {
        l.push(sep());
        l.push(Span::styled("unregistered process", fg(WARN)));
    }
    lines.push(Line::from(l));
    // 6: now
    let mut l = vec![Span::styled("now    ", dim())];
    match &s.status {
        Status::Ended => {
            l.push(Span::styled(
                format!("✕ ended {} ago", human_dur(now.saturating_sub(s.ended_at_ms.unwrap_or(now)))),
                fg(INACTIVE_FG),
            ));
        }
        _ if !s.running.is_empty() => {
            let r = &s.running[0];
            l.push(Span::styled("⚙ ", bold(BUSY)));
            l.push(Span::styled(r.name.clone(), bold(TITLE)));
            l.push(Span::styled(format!("  {}", r.summary), main()));
            if r.since_ms > 0 {
                l.push(Span::styled(format!("  · {}", human_dur(now.saturating_sub(r.since_ms))), dim()));
            }
            if s.running.len() > 1 {
                l.push(Span::styled(format!("  · +{} more", s.running.len() - 1), dim()));
            }
        }
        Status::Busy => {
            l.push(Span::styled("● generating", bold(BUSY)));
            if s.last_activity_ms > 0 {
                l.push(Span::styled(format!("  · {}", human_dur(now.saturating_sub(s.last_activity_ms))), dim()));
            }
        }
        Status::Idle => {
            let since = if s.status_since_ms > 0 { s.status_since_ms } else { s.last_activity_ms };
            l.push(Span::styled("○ idle", fg(IDLE)));
            if since > 0 {
                l.push(Span::styled(format!("  · {} · waiting for input", human_dur(now.saturating_sub(since))), dim()));
            }
        }
        Status::Other(o) => {
            l.push(Span::styled(format!("◐ {o}"), bold(WARN)));
        }
    }
    lines.push(Line::from(l));
    // 7/8: prompt + reply
    lines.push(Line::from(kv("prompt", s.last_prompt.clone())));
    lines.push(Line::from(kv("reply ", s.last_reply.clone())));
    lines.truncate(inner.height as usize);
    f.render_widget(Paragraph::new(lines), text_area);

    if graph_w > 0 && inner.height >= 2 {
        let g = Rect::new(inner.x + text_w, inner.y, graph_w, inner.height);
        // divider
        let buf = f.buffer_mut();
        for y in g.y..g.y + g.height {
            if let Some(c) = buf.cell_mut((g.x, y)) {
                c.set_char('│').set_fg(DIV_LINE);
            }
        }
        let ga = Rect::new(g.x + 1, g.y, g.width - 1, g.height);
        dual_graph(buf, ga, &s.hist_in, &s.hist_out, true);
    }
}

/// What enter does on each pane.
fn enter_action(view: View) -> &'static str {
    match view {
        View::Board | View::Monitor => "jump to session",
        View::History => "open in Claude / copy resume",
        View::Cleanup => "open folder",
    }
}

/// Pane-specific keys; every pane also has the keys in `COMMON_KEYS`.
fn pane_keys(view: View) -> &'static [(&'static str, &'static str)] {
    match view {
        View::Board => &[
            ("1 … 9", "jump to the numbered card"),
            ("d", "toggle detail pane"),
            ("t", "terminate selected session (SIGTERM, asks first)"),
        ],
        View::Monitor => &[
            ("d", "toggle detail pane"),
            ("t", "terminate selected session (SIGTERM, asks first)"),
            ("g", "toggle token graphs"),
            ("s S ← →", "cycle sort column"),
            ("r", "reverse sort"),
            ("a", "show / hide ended sessions"),
            ("+ -", "refresh interval ±100 ms"),
        ],
        View::History => &[("a", "show / hide archived sessions")],
        View::Cleanup => &[("x del", "git worktree remove (asks first; branch stays)"), ("R", "rescan")],
    }
}

const COMMON_KEYS: &[(&str, &str)] = &[
    ("↑ ↓ j k pgup pgdn", "move selection (home end: first, last)"),
    ("f /", "filter, esc clears (not on cleanup)"),
    ("b m h c", "go to board · monitor · history · cleanup"),
    ("tab shift-tab", "next / previous pane"),
    ("?", "this help"),
    ("q esc ctrl-c", "quit"),
];

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    if app.filtering {
        spans.extend(key("esc", "clear"));
        spans.extend(key("enter", "apply"));
        spans.push(Span::styled("type to filter by name / project / branch / model", dim()));
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    spans.extend(key("enter", enter_action(app.view)));
    match app.view {
        View::Board => {
            spans.extend(key("1-9", "jump"));
            spans.extend(key("d", "detail"));
            spans.extend(key("t", "term"));
        }
        View::Monitor => {
            spans.extend(key("d", "detail"));
            spans.extend(key("t", "term"));
            spans.extend(key("g", "graphs"));
            spans.extend(key("s", &format!("sort:{}", app.sort.name())));
            spans.extend(key("r", "reverse"));
            spans.extend(key("a", if app.show_ended { "hide ended" } else { "ended" }));
            spans.extend(key("+-", &format!("{}ms", app.interval.as_millis())));
        }
        View::History => spans.extend(key("a", if app.show_archived { "hide archived" } else { "archived" })),
        View::Cleanup => {
            spans.extend(key("x", "remove"));
            spans.extend(key("R", "rescan"));
        }
    }
    if app.view != View::Cleanup {
        spans.extend(key("f", "filter"));
    }
    spans.extend(key("bmhc", "pane"));
    spans.extend(key("?", "help"));
    spans.extend(key("q", "quit"));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_help(f: &mut Frame, area: Rect, view: View) {
    let enter = [("enter", enter_action(view))];
    let rows: Vec<(&str, &str)> = enter.iter().chain(pane_keys(view)).chain(COMMON_KEYS).copied().collect();
    let w = 78;
    let h = rows.len() as u16 + 4;
    let r = popup(area, w, h);
    f.render_widget(Clear, r);
    let block = boxed("help", NET_BOX);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!(" {} · live views of your Claude Code sessions", view.name()),
        bold(TITLE),
    ))];
    for (k, d) in rows {
        lines.push(Line::from(vec![
            Span::styled(format!(" {:<22}", k), bold(HI_FG)),
            Span::styled(d.to_string(), main()),
        ]));
    }
    lines.push(Line::from(Span::styled(" any key to close", dim())));
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_confirm(f: &mut Frame, area: Rect, pid: u32, name: &str) {
    let r = popup(area, 60, 5);
    f.render_widget(Clear, r);
    let block = boxed("terminate", PROC_BOX);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let lines = vec![
        Line::from(vec![
            Span::styled(" send SIGTERM to pid ", main()),
            Span::styled(pid.to_string(), bold(TITLE)),
            Span::styled(format!("  ({})", truncate(name, 30)), dim()),
        ]),
        Line::from(vec![
            Span::styled(" ", main()),
            Span::styled("y", bold(HI_FG)),
            Span::styled(" yes   ", main()),
            Span::styled("n", bold(HI_FG)),
            Span::styled(" no", main()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}
