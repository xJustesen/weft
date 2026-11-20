//! The history view: past sessions grouped by project, tapestry's board as a list.

use super::theme::*;
use super::widgets::*;
use crate::app::{App, RecentRow};
use crate::history::RecentSession;
use crate::util::{fit, human_dur, shorten_path};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

fn status(s: &RecentSession) -> (String, Style) {
    if s.archived {
        return ("▫ archived".into(), fg(INACTIVE_FG));
    }
    match s.status.as_deref() {
        Some("blocked") => ("◐ blocked".into(), bold(WARN)),
        Some("review_ready") => ("◆ review".into(), bold(ACCENT)),
        Some("completed") => ("✓ done".into(), fg(BUSY)),
        Some(other) => (format!("· {other}"), dim()),
        None => ("·".into(), dim()),
    }
}

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let rows = app.recent_rows();
    let total: usize = rows.iter().filter(|r| matches!(r, RecentRow::Session(_))).count();
    let mut block = boxed("history", MEM_BOX).title_top(tab(format!("{total} sessions"), MEM_BOX, dim()).right_aligned());
    if app.filtering || !app.filter.is_empty() {
        let txt = format!("filter: {}{}", app.filter, if app.filtering { "_" } else { "" });
        block = block.title_top(tab(txt, MEM_BOX, bold(WARN)).right_aligned());
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if rows.is_empty() {
        let msg = if app.snap.recent.is_none() { "reading transcripts…" } else { "no sessions" };
        let r = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
        f.render_widget(Paragraph::new(msg).style(dim()).alignment(Alignment::Center), r);
        return;
    }
    let now = app.snap.at_ms;
    let w = inner.width as usize;
    let sel = app.recent_sel.min(total.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
    let mut sel_line = 0;
    let mut k = 0usize;
    for row in &rows {
        match row {
            RecentRow::Project(p, n) => {
                if !lines.is_empty() {
                    lines.push(Line::raw(""));
                }
                lines.push(Line::from(vec![
                    Span::styled(format!(" {} ", p.name), bold(TITLE)),
                    Span::styled(format!("{n} · {}", shorten_path(&p.root)), dim()),
                ]));
            }
            RecentRow::Session(s) => {
                let selected = k == sel;
                if selected {
                    sel_line = lines.len();
                }
                k += 1;
                let (st, st_style) = status(s);
                let ago = if s.last_ms == 0 { "-".into() } else { human_dur(now.saturating_sub(s.last_ms)) };
                let mut right = String::new();
                if let Some(b) = &s.branch {
                    right.push_str(&format!("⎇ {b}  "));
                }
                if let Some(pr) = s.prs.last() {
                    right.push_str(&format!("PR #{}  ", pr.number.unwrap_or(0)));
                }
                right.push_str(&format!("{:<8} {:>5}", s.source.replace("claude-", ""), ago));
                let right = fit(&right, w * 45 / 100, true);
                let title_w = w.saturating_sub(3 + 11 + right.chars().count() + 2);
                let title_style = if s.archived { fg(INACTIVE_FG) } else { main() };
                let mut line = Line::from(vec![
                    Span::raw("   "),
                    Span::styled(fit(&st, 11, false), st_style),
                    Span::styled(fit(&s.title, title_w, false), title_style),
                    Span::raw("  "),
                    Span::styled(right, dim()),
                ]);
                if selected {
                    line = line.style(Style::new().bg(SELECTED_BG));
                }
                lines.push(line);
            }
        }
    }
    let h = inner.height as usize;
    let mut offset = app.recent_offset;
    if sel_line < offset + 1 {
        offset = sel_line.saturating_sub(1);
    } else if sel_line >= offset + h {
        offset = sel_line + 1 - h;
    }
    app.recent_offset = offset;
    let lines: Vec<Line> = lines.into_iter().skip(offset).take(h).collect();
    f.render_widget(Paragraph::new(lines), inner);
}
