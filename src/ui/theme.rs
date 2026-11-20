//! btop "Default" theme palette.

use ratatui::style::{Color, Modifier, Style};

pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

pub const MAIN_FG: Color = rgb(0xcc, 0xcc, 0xcc);
pub const TITLE: Color = rgb(0xee, 0xee, 0xee);
pub const HI_FG: Color = rgb(0xd0, 0x60, 0x60);
pub const SELECTED_BG: Color = rgb(0x6a, 0x2f, 0x2f);
pub const SELECTED_FG: Color = rgb(0xee, 0xee, 0xee);
pub const INACTIVE_FG: Color = rgb(0x50, 0x50, 0x50);
pub const GRAPH_TEXT: Color = rgb(0x70, 0x70, 0x70);
pub const METER_BG: Color = rgb(0x40, 0x40, 0x40);
pub const DIM: Color = rgb(0x80, 0x80, 0x80);
pub const DIV_LINE: Color = rgb(0x30, 0x30, 0x30);
pub const PROC_MISC: Color = rgb(0x0d, 0xe7, 0x56);

pub const CPU_BOX: Color = rgb(0x3d, 0x7b, 0x46);
pub const MEM_BOX: Color = rgb(0x8a, 0x88, 0x2e);
pub const NET_BOX: Color = rgb(0x42, 0x3b, 0xa5);
pub const PROC_BOX: Color = rgb(0x92, 0x35, 0x35);

pub const BUSY: Color = rgb(0x50, 0xf0, 0x75);
pub const IDLE: Color = rgb(0x8a, 0x8a, 0x8a);
pub const WARN: Color = rgb(0xf2, 0xe2, 0x66);
pub const ERR: Color = rgb(0xfa, 0x1e, 0x1e);
pub const ACCENT: Color = rgb(0x74, 0xe6, 0xfc);
pub const PURPLE: Color = rgb(0xb0, 0xa9, 0xde);
pub const PINK: Color = rgb(0xdc, 0xaf, 0xde);

pub type Stops = [(u8, u8, u8); 3];

pub const GRAD_CPU: Stops = [(0x50, 0xf0, 0x75), (0xf2, 0xe2, 0x66), (0xfa, 0x1e, 0x1e)];
pub const GRAD_IN: Stops = [(0x4f, 0x43, 0xa3), (0x80, 0x76, 0xc4), (0xb0, 0xa9, 0xde)];
pub const GRAD_OUT: Stops = [(0x7d, 0x41, 0x80), (0xb0, 0x78, 0xb3), (0xdc, 0xaf, 0xde)];
pub const GRAD_USED: Stops = [(0x8a, 0x4a, 0x50), (0xd9, 0x62, 0x6d), (0xff, 0x47, 0x69)];
pub const GRAD_CACHED: Stops = [(0x2a, 0x6a, 0x8a), (0x74, 0xe6, 0xfc), (0x26, 0xc5, 0xff)];

/// Interpolate `n` colours across three stops.
pub fn gradient(stops: &Stops, n: usize) -> Vec<Color> {
    let n = n.max(1);
    (0..n)
        .map(|i| {
            let t = if n == 1 { 1.0 } else { i as f32 / (n - 1) as f32 };
            let (a, b, lt) = if t < 0.5 {
                (stops[0], stops[1], t * 2.0)
            } else {
                (stops[1], stops[2], (t - 0.5) * 2.0)
            };
            let l = |x: u8, y: u8| -> u8 { (x as f32 + (y as f32 - x as f32) * lt).round() as u8 };
            Color::Rgb(l(a.0, b.0), l(a.1, b.1), l(a.2, b.2))
        })
        .collect()
}

/// Pick a colour for a 0..1 fraction on a gradient (green→red style).
pub fn grade(stops: &Stops, frac: f32) -> Color {
    let g = gradient(stops, 16);
    let i = ((frac.clamp(0.0, 1.0)) * 15.0).round() as usize;
    g[i.min(15)]
}

pub fn fg(c: Color) -> Style {
    Style::new().fg(c)
}
pub fn bold(c: Color) -> Style {
    Style::new().fg(c).add_modifier(Modifier::BOLD)
}
pub fn dim() -> Style {
    Style::new().fg(DIM)
}
pub fn main() -> Style {
    Style::new().fg(MAIN_FG)
}
