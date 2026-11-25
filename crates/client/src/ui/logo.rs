//! Logo art (approved; copied verbatim from `docs/assets/logo.py`).
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::theme::{BG, ME, THEM};

/// Full logo, 6 rows.
pub const FULL: &str = r"          ▄███▄         ██████   ██████  ███    ███ ██████   ██████
          ██▄███▄      ██   ██ ██    ██ ████  ████ ██   ██ ██    ██          ▀▀
    ▄▄▄▄▄█████▀       ██████  ██    ██ ██ ████ ██ ██████  ██    ██          ██  ██▄▀▀▀  ▄█▀▀▀▀
▄▄████████████       ██      ██    ██ ██  ██  ██ ██   ██ ██    ██    ▀▀▀▀  ██  ██      ██
 ▀▀██████████▀      ██       ██████  ██      ██ ██████   ██████           ██  ██      ▀█▄▄▄▄
      █▄  █▄";

/// Compact logo, 4 rows.
pub const COMPACT: &str = r"     ▄██▄                                  ██                      ▀▀
  ▄▄▄█▄██▀    ██▀▀█▄  ▄█▀▀█▄  ██▀▀██▀▀█▄  ██▀▀█▄  ▄█▀▀█▄   ▄▄▄▄  ██  ██▄▀▀▀  ▄█▀▀▀
▀███████▀    ██▄▄█▀  ▀█▄▄█▀  ██  ██  ██  ██▄▄█▀  ▀█▄▄█▀         ██  ██      ▀█▄▄▄
   █▄ █▄    ██";

/// Dim tagline shown under the logo.
pub const TAGLINE: &str = "  group rooms  ·  end-to-end encrypted  ·  nothing stored";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Full,
    Compact,
    Chip,
}

/// Display width of the widest row of `art`.
fn art_width(art: &str) -> usize {
    art.lines().map(UnicodeWidthStr::width).max().unwrap_or(0)
}

/// Logo tier for a terminal `width` columns wide (same rule as the script: art width + 2).
pub fn pick(width: u16) -> Tier {
    let width = usize::from(width);
    if width >= art_width(FULL) + 2 {
        Tier::Full
    } else if width >= art_width(COMPACT) + 2 {
        Tier::Compact
    } else {
        Tier::Chip
    }
}

/// The logo for `tier`, ready to render. Art tiers get the per-column ME to THEM gradient.
pub fn lines(tier: Tier) -> Vec<Line<'static>> {
    match tier {
        Tier::Full => gradient(FULL),
        Tier::Compact => gradient(COMPACT),
        Tier::Chip => vec![Line::from(Span::styled(
            " pombo-irc ",
            Style::default().bg(ME).fg(BG).add_modifier(Modifier::BOLD),
        ))],
    }
}

/// Mirrors `grad()` in the logo script: column `i` of an art `w` columns wide gets
/// `round(a + (b - a) * i / w)` per channel (Python rounds half to even).
fn gradient(art: &'static str) -> Vec<Line<'static>> {
    let w = art_width(art).max(1) as f64;
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (ME, THEM) else {
        unreachable!("palette colors are RGB")
    };
    let mix = |x: u8, y: u8, i: usize| {
        let (x, y) = (f64::from(x), f64::from(y));
        (x + (y - x) * i as f64 / w).round_ties_even() as u8
    };
    art.lines()
        .map(|row| {
            let spans = row
                .char_indices()
                .enumerate()
                .map(|(col, (i, ch))| {
                    let color = Color::Rgb(mix(ar, br, col), mix(ag, bg, col), mix(ab, bb, col));
                    Span::styled(
                        &row[i..i + ch.len_utf8()],
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    )
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::theme::{BG, ME, THEM};
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn logo_tiers() {
        assert_eq!(pick(200), Tier::Full);
        assert_eq!(pick(90), Tier::Compact);
        assert_eq!(pick(60), Tier::Chip);
        assert_eq!(pick(96), Tier::Full);
        assert_eq!(pick(95), Tier::Compact);
        assert_eq!(pick(84), Tier::Compact);
        assert_eq!(pick(83), Tier::Chip);
        assert_eq!(pick(0), Tier::Chip);
    }

    #[test]
    fn logo_art_matches_script() {
        assert_eq!(FULL.lines().count(), 6);
        assert_eq!(COMPACT.lines().count(), 4);
        assert!(FULL.contains("▄███▄"));
        let script = include_str!("../../../../docs/assets/logo.py");
        assert!(script.contains(&format!("FULL = r\"\"\"{FULL}\"\"\"")));
        assert!(script.contains(&format!("COMPACT = r\"\"\"{COMPACT}\"\"\"")));
    }

    #[test]
    fn gradient_runs_me_to_them() {
        let ls = lines(Tier::Full);
        assert_eq!(ls.len(), 6);
        for (l, art) in ls.iter().zip(FULL.lines()) {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(text, art);
        }
        let first = &ls[0].spans[0];
        assert_eq!(first.style.fg, Some(ME));
        assert!(first.style.add_modifier.contains(Modifier::BOLD));
        // Widest row (94 cols, index 2): last cell at i = 93 of w = 94, Python round().
        let last = ls[2].spans.last().unwrap();
        assert_eq!(last.style.fg, Some(Color::Rgb(0xc3, 0x9d, 0xef)));
        assert_ne!(last.style.fg, Some(THEM));
        assert_eq!(lines(Tier::Compact).len(), 4);
    }

    #[test]
    fn chip_tier() {
        let ls = lines(Tier::Chip);
        assert_eq!(ls.len(), 1);
        let s = &ls[0].spans[0];
        assert_eq!(s.content, " pombo-irc ");
        assert_eq!(s.style.bg, Some(ME));
        assert_eq!(s.style.fg, Some(BG));
    }
}
