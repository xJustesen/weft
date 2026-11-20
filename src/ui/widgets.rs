//! Custom drawing primitives: filled braille graphs and block meters, btop style.

use super::theme::{self, Stops};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

/// Braille dot bit for [column][row], rows top→bottom.
const DOTS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];

/// Draw a filled braille graph of `data` (oldest→newest, right aligned) into `area`.
/// `invert` grows the graph downward from the top edge (btop's "upload" half).
pub fn braille_graph(buf: &mut Buffer, area: Rect, data: &[f32], max: f32, invert: bool, stops: &Stops) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let w = area.width as usize;
    let h = area.height as usize;
    let npts = w * 2;
    let levels = (h * 4) as f32;
    let max = if max <= 0.0 { 1.0 } else { max };
    let start = data.len().saturating_sub(npts);
    let slice = &data[start..];
    let pad = npts - slice.len();
    let grad = theme::gradient(stops, h.max(2));
    let level_at = |i: usize| -> usize {
        if i < pad {
            0
        } else {
            let v = slice[i - pad];
            let l = ((v / max).clamp(0.0, 1.0) * levels).round() as usize;
            if v > 0.0 && l == 0 {
                1
            } else {
                l
            }
        }
    };
    for cx in 0..w {
        let l0 = level_at(cx * 2);
        let l1 = level_at(cx * 2 + 1);
        if l0 == 0 && l1 == 0 {
            continue;
        }
        for cy in 0..h {
            let rb = if invert { cy } else { h - 1 - cy };
            let base = rb * 4;
            let mut bits = 0u8;
            for (col, l) in [(0usize, l0), (1usize, l1)] {
                let lit = l.saturating_sub(base).min(4);
                for k in 0..lit {
                    let r = if invert { k } else { 3 - k };
                    bits |= DOTS[col][r];
                }
            }
            if bits == 0 {
                continue;
            }
            let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
            let gi = if h > 1 { rb * (grad.len() - 1) / (h - 1) } else { grad.len() - 1 };
            let color = grad[gi.min(grad.len() - 1)];
            if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + cy as u16)) {
                cell.set_char(ch).set_fg(color);
            }
        }
    }
}

/// Visible-window maximum for auto-scaling a graph.
pub fn window_max(data: &[f32], width: u16, floor: f32) -> f32 {
    let n = (width as usize) * 2;
    let start = data.len().saturating_sub(n);
    data[start..].iter().copied().fold(floor, f32::max)
}

/// A horizontal block meter coloured along a gradient.
pub fn meter(width: usize, frac: f32, stops: &Stops) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let grad = theme::gradient(stops, width.max(2));
    let filled = (frac.clamp(0.0, 1.0) * width as f32).round() as usize;
    (0..width)
        .map(|i| {
            if i < filled {
                let gi = if width > 1 { i * (grad.len() - 1) / (width - 1) } else { grad.len() - 1 };
                Span::styled("■", Style::new().fg(grad[gi.min(grad.len() - 1)]))
            } else {
                Span::styled("■", Style::new().fg(theme::METER_BG))
            }
        })
        .collect()
}

/// A rounded, coloured box with a `┤ title ├` tab, like btop.
pub fn boxed(title: &str, color: Color) -> Block<'static> {
    let border = Style::new().fg(color);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title_top(Line::from(vec![
            Span::styled("┤", border),
            Span::styled(format!(" {title} "), theme::bold(theme::TITLE)),
            Span::styled("├", border),
        ]))
}

/// A secondary `┤ text ├` tab for the right side of a box title.
pub fn tab(text: impl Into<String>, color: Color, text_style: Style) -> Line<'static> {
    let border = Style::new().fg(color);
    Line::from(vec![
        Span::styled("┤", border),
        Span::styled(format!(" {} ", text.into()), text_style),
        Span::styled("├", border),
    ])
}

/// Overlay a short label on top of a graph at its top-left.
pub fn label(buf: &mut Buffer, area: Rect, text: &str, style: Style) {
    if area.width < 2 || area.height == 0 {
        return;
    }
    let t = crate::util::truncate(text, area.width as usize - 1);
    buf.set_string(area.x + 1, area.y, t, style);
}

/// Key hint: highlighted key + description.
pub fn key(k: &str, desc: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(k.to_string(), theme::bold(theme::HI_FG)),
        Span::styled(format!(" {desc}  "), theme::fg(theme::MAIN_FG)),
    ]
}

/// Dimmed label + value pair.
pub fn kv(k: &str, v: impl Into<String>) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{k} "), theme::dim()),
        Span::styled(v.into(), theme::main()),
    ]
}

pub fn sep() -> Span<'static> {
    Span::styled(" · ", theme::fg(theme::INACTIVE_FG))
}
