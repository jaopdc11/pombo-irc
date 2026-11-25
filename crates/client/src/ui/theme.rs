//! Palette (Global Constraints of the plan, plus the mockups' fg / faint / bar shades).
use ratatui::style::{Color, Modifier, Style};

/// Own side: cyan.
pub const ME: Color = Color::Rgb(0x7d, 0xd3, 0xc0);
/// The peer: lilac.
pub const THEM: Color = Color::Rgb(0xc4, 0x9c, 0xf0);
/// Attention: yellow.
pub const WARN: Color = Color::Rgb(0xf0, 0xc6, 0x74);
/// Success: green.
pub const OK: Color = Color::Rgb(0x8b, 0xd4, 0x9c);
/// Closed / ended: red.
pub const BAD: Color = Color::Rgb(0xf2, 0x8b, 0x82);
/// Background.
pub const BG: Color = Color::Rgb(0x10, 0x12, 0x1a);
/// Panels and input line.
pub const SURFACE: Color = Color::Rgb(0x18, 0x1b, 0x26);
/// Selected rows and options.
pub const RAISE: Color = Color::Rgb(0x20, 0x25, 0x3a);
/// Dim text.
pub const DIM: Color = Color::Rgb(0x67, 0x70, 0x87);
/// Regular text (mockups `--t-fg`).
pub const FG: Color = Color::Rgb(0xcf, 0xd5, 0xe2);
/// Faint text: spent part of the drain bar, closed input (mockups `--t-faint`).
pub const FAINT: Color = Color::Rgb(0x3a, 0x41, 0x57);
/// Status bar background (mockups `--t-bar`).
pub const BAR: Color = Color::Rgb(0x17, 0x1a, 0x24);

/// Foreground `color`.
pub fn fg(color: Color) -> Style {
    Style::default().fg(color)
}

/// Bold foreground `color`.
pub fn bold(color: Color) -> Style {
    fg(color).add_modifier(Modifier::BOLD)
}

/// Chip style: bold `BG` text on a `color` background.
pub fn chip(color: Color) -> Style {
    Style::default()
        .bg(color)
        .fg(BG)
        .add_modifier(Modifier::BOLD)
}

/// Extra nick hue: blue.
pub const BLUE: Color = Color::Rgb(0x82, 0xaa, 0xff);
/// Extra nick hue: orange.
pub const ORANGE: Color = Color::Rgb(0xf5, 0xa9, 0x7f);
/// Extra nick hue: pink.
pub const PINK: Color = Color::Rgb(0xf5, 0xa3, 0xc7);
/// Extra nick hue: lime.
pub const LIME: Color = Color::Rgb(0xc3, 0xe8, 0x8d);
/// Extra nick hue: sand.
pub const SAND: Color = Color::Rgb(0xd8, 0xc4, 0x8a);

/// Nick colors for the other members of a room, assigned in join order (own nick is `ME`).
pub const NICK_HUES: [Color; 7] = [THEM, BLUE, ORANGE, PINK, LIME, SAND, OK];

/// The hue for the `n`-th other member (zero-based), cycling through [`NICK_HUES`].
pub fn nick_hue(n: usize) -> Color {
    NICK_HUES[n % NICK_HUES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_distinct_hues_none_is_me() {
        for (i, a) in NICK_HUES.iter().enumerate() {
            assert_ne!(*a, ME);
            for b in &NICK_HUES[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(nick_hue(7), nick_hue(0));
    }
}
