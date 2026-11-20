//! The board: live sessions as two-line cards in waiting / working / idle sections, numbered for jumping.

use super::theme::*;
use super::widgets::*;
use super::{draw_detail, status_span};
use crate::app::App;
use crate::model::{Board, SessionView};
use crate::util::{fit, human_dur, pct, short_model, truncate};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

fn section(b: Board) -> (&'static str, Color) {
    match b {
        Board::Waiting => ("waiting for you", WARN),
        Board::Working => ("working", BUSY),
        Board::Idle => ("idle", IDLE),
    }
}

/// Second card line: what the session is doing now, what it needs, or its last reply.
fn activity(s: &SessionView, now: u64) -> Vec<Span<'static>> {
    if let Some(r) = s.running.first() {
        let mut v = vec![Span::styled("⚙ ", bold(BUSY)), Span::styled(r.name.clone(), bold(TITLE)), Span::styled(format!("  {}", r.summary), main())];
        if r.since_ms > 0 {
            v.push(Span::styled(format!("  · {}", human_dur(now.saturating_sub(r.since_ms))), dim()));
        }
        return v;
    }
    if s.board() == Board::Waiting {
        let need = s.attention.clone().unwrap_or_else(|| s.status.label().to_string());
        return vec![Span::styled("◐ ", bold(WARN)), Span::styled(need, fg(WARN))];
    }
    if !s.last_reply.is_empty() {
        return vec![Span::styled("↳ ", dim()), Span::styled(s.last_reply.clone(), dim())];
    }
    vec![Span::styled(format!("↳ {}", s.last_prompt), dim())]
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let detail_h: u16 = if app.show_detail && area.height >= 24 { 10 } else { 0 };
    let [cards, detail] = Layout::vertical([Constraint::Min(4), Constraint::Length(detail_h)]).areas(area);
    if detail_h > 0 {
        draw_detail(f, detail, app);
    }
    let rows = app.board_rows();
    let mut block = boxed("sessions", NET_BOX);
    if app.filtering || !app.filter.is_empty() {
        let txt = format!("filter: {}{}", app.filter, if app.filtering { "_" } else { "" });
        block = block.title_top(tab(txt, NET_BOX, bold(WARN)).right_aligned());
    }
    let inner = block.inner(cards);
    f.render_widget(block, cards);
    if rows.is_empty() {
        let msg = if !app.has_data() { "scanning…" } else if !app.filter.is_empty() { "no sessions match the filter" } else { "no Claude Code sessions running" };
        let r = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
        f.render_widget(Paragraph::new(msg).style(dim()).alignment(Alignment::Center), r);
        return;
    }
    let now = app.snap.at_ms;
    let w = inner.width as usize;
    let sel_pid = app.selected_pid;
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_line = 0usize;
    let mut cur: Option<Board> = None;
    for (i, s) in rows.iter().enumerate() {
        let b = s.board();
        if cur != Some(b) {
            let (name, color) = section(b);
            let count = rows.iter().filter(|r| r.board() == b).count();
            if cur.is_some() {
                lines.push(Line::raw(""));
            }
            let label = format!("─ {name} ({count}) ");
            lines.push(Line::from(vec![
                Span::styled(label.clone(), bold(color)),
                Span::styled("─".repeat(w.saturating_sub(label.chars().count())), fg(DIV_LINE)),
            ]));
            cur = Some(b);
        }
        let selected = Some(s.pid) == sel_pid;
        if selected {
            sel_line = lines.len();
        }
        let badge = if i < 9 { format!(" {} ", i + 1) } else { "   ".into() };
        let place = format!("{}{}", s.project, if s.branch.is_empty() { String::new() } else { format!(" ⎇ {}", s.branch) });
        let stats = format!(
            "  {:<9} ctx {:>3.0}%  {:>5}",
            short_model(&s.model),
            pct(s.ctx_tokens, s.ctx_limit) * 100.0,
            if s.last_activity_ms == 0 { "-".into() } else { human_dur(now.saturating_sub(s.last_activity_ms)) },
        );
        let right = format!("{}{stats}", truncate(&place, (w / 2).saturating_sub(stats.chars().count())));
        let st = if b == Board::Waiting && s.running.is_empty() { Span::styled("◐ wait", bold(WARN)) } else { status_span(s) };
        let title_w = w.saturating_sub(3 + 8 + right.chars().count() + 2);
        let title_style = if b == Board::Idle { main() } else { bold(TITLE) };
        lines.push(Line::from(vec![
            Span::styled(badge, bold(HI_FG)),
            Span::styled(fit(&st.content, 8, false), st.style),
            Span::styled(fit(&s.name, title_w, false), title_style),
            Span::raw("  "),
            Span::styled(right, dim()),
        ]));
        let mut l2 = vec![Span::raw("           ")];
        l2.extend(activity(s, now));
        if let Some(pr) = s.prs.last() {
            l2.push(Span::styled(format!("  PR #{}", pr.number.unwrap_or(0)), fg(ACCENT)));
        }
        lines.push(Line::from(l2));
        if selected {
            let n = lines.len();
            for l in &mut lines[n - 2..] {
                *l = std::mem::take(l).style(Style::new().bg(SELECTED_BG));
            }
        }
    }
    let h = inner.height as usize;
    let scroll = (sel_line + 3).saturating_sub(h);
    let lines: Vec<Line> = lines.into_iter().skip(scroll).take(h).map(|l| pad_line(l, w)).collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// Pad a highlighted line so its background spans the full width.
fn pad_line(l: Line<'static>, w: usize) -> Line<'static> {
    let used: usize = l.spans.iter().map(|s| s.content.chars().count()).sum();
    let style = l.style;
    let mut l = l;
    if used < w && style.bg.is_some() {
        l.spans.push(Span::raw(" ".repeat(w - used)));
    }
    l.style(style)
}
