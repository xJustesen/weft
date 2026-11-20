//! The cleanup view: agent worktrees that no live session uses, with their git state.

use super::theme::*;
use super::widgets::*;
use super::popup;
use crate::app::App;
use crate::util::{fit, human_dur, shorten_path, truncate};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use std::path::Path;

pub fn draw(f: &mut Frame, area: Rect, app: &mut App) {
    let Ok(c) = app.cleanup.lock() else { return };
    let tag = if c.scanning { "scanning…".to_string() } else { format!("{} worktrees", c.items.len()) };
    let block = boxed("worktrees without a live session", PROC_BOX).title_top(tab(tag, PROC_BOX, dim()).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
    if c.items.is_empty() {
        let msg = if c.scanned { "nothing to clean up" } else { "scanning…" };
        let r = Rect::new(inner.x, inner.y + inner.height / 2, inner.width, 1);
        f.render_widget(Paragraph::new(msg).style(dim()).alignment(Alignment::Center), r);
        return;
    }
    let now = app.snap.at_ms;
    let w = inner.width as usize;
    let name_w = 36.min(w / 3);
    let branch_w = w.saturating_sub(1 + name_w + 22 + 16 + 6 + 4).max(10);
    let mut lines = vec![Line::from(Span::styled(
        format!(" {} {} {} {} {}", fit("worktree", name_w, false), fit("repo", 22, false), fit("branch", branch_w, false), fit("state", 16, false), fit("age", 6, true)),
        bold(TITLE),
    ))];
    let sel = app.cleanup_sel.min(c.items.len() - 1);
    let h = inner.height as usize - 1;
    let offset = (sel + 1).saturating_sub(h);
    for (i, wt) in c.items.iter().enumerate().skip(offset).take(h) {
        let (state, st) = match (&wt.error, wt.dirty, wt.unpushed) {
            (Some(e), _, _) => (truncate(e, 16), fg(ERR)),
            (None, d, _) if d > 0 => (format!("{d} changed"), bold(WARN)),
            (None, 0, u) if u > 0 => (format!("clean · {u} local"), fg(ACCENT)),
            _ => ("clean".into(), fg(BUSY)),
        };
        let repo = wt.repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut line = Line::from(vec![
            Span::raw(" "),
            Span::styled(fit(&wt.name, name_w, false), main()),
            Span::raw(" "),
            Span::styled(fit(&repo, 22, false), dim()),
            Span::raw(" "),
            Span::styled(fit(&wt.branch, branch_w, false), fg(ACCENT)),
            Span::raw(" "),
            Span::styled(fit(&state, 16, false), st),
            Span::raw(" "),
            Span::styled(fit(&human_dur(now.saturating_sub(wt.modified_ms)), 6, true), dim()),
        ]);
        if i == sel {
            line = line.style(Style::new().bg(SELECTED_BG));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn draw_confirm(f: &mut Frame, area: Rect, path: &Path) {
    let r = popup(area, 72, 6);
    f.render_widget(Clear, r);
    let block = boxed("remove worktree", PROC_BOX);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let lines = vec![
        Line::from(vec![Span::styled(" git worktree remove ", main()), Span::styled(shorten_path(&path.to_string_lossy()), bold(TITLE))]),
        Line::from(Span::styled(" the folder is deleted; the branch and its commits stay", dim())),
        Line::from(vec![Span::styled(" y", bold(HI_FG)), Span::styled(" yes   ", main()), Span::styled("n", bold(HI_FG)), Span::styled(" no", main())]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}
