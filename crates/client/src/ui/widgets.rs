//! Small line-level widgets: status bar, drain bar, room rows and message blocks.
//! Every string that may come from the network is passed through [`sanitize`] here too.
use std::time::Duration;

use proto::{RoomInfo, MAX_MEMBERS, MAX_NICK, MAX_ROOM_NAME};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::text::{sanitize, wrap, Group};
use super::theme::{bold, fg, BAR, DIM, FAINT, FG, ME, OK, RAISE, THEM};

/// Rule on the left of the peer's messages.
pub const PEER_RULE: &str = "▏";
/// Rule on the right of own messages.
pub const OWN_RULE: &str = "▕";

/// One status bar segment. `text` is shown as is (include any padding spaces).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: Style,
}

/// Cuts `s` to at most `max` display columns without splitting a char.
fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

/// Bottom bar exactly `width` columns wide on the bar background: `left` segments, filler,
/// `right` segments. When it does not fit, `right` segments are dropped from the end, then
/// `left` is cut at the width.
pub fn status_bar(left: &[Seg], right: &[Seg], width: u16) -> Line<'static> {
    let width = usize::from(width);
    let clean = |segs: &[Seg]| -> Vec<(String, Style)> {
        segs.iter().map(|s| (sanitize(&s.text), s.style)).collect()
    };
    let total = |segs: &[(String, Style)]| -> usize { segs.iter().map(|(t, _)| t.width()).sum() };
    let left = clean(left);
    let mut right = clean(right);
    while !right.is_empty() && total(&left) + total(&right) > width {
        right.pop();
    }
    let mut spans = Vec::new();
    let mut used = 0;
    for (text, style) in left {
        let text = truncate(&text, width - used);
        if text.is_empty() {
            break;
        }
        used += text.width();
        spans.push(Span::styled(text, style));
    }
    let right_w = total(&right);
    spans.push(Span::raw(" ".repeat(width - used - right_w)));
    spans.extend(right.into_iter().map(|(t, s)| Span::styled(t, s)));
    Line::from(spans).style(Style::default().bg(BAR).fg(FG))
}

/// Countdown bar of `cells` cells: `━` in ME for the remaining share, faint for the spent one.
pub fn drain_bar(remaining: Duration, total: Duration, cells: u16) -> Line<'static> {
    let cells = usize::from(cells);
    let left = if total.is_zero() {
        0
    } else {
        let share = (remaining.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0);
        ((share * cells as f64).round() as usize).min(cells)
    };
    Line::from(vec![
        Span::styled("━".repeat(left), fg(ME)),
        Span::styled("━".repeat(cells - left), fg(FAINT)),
    ])
}

/// `41s`, `3m`, `2h`.
fn age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h", secs / 3600),
    }
}

/// One row of the room list. `index` is the zero-based position; the row shows `index + 1`.
/// Layout: mark (2) · number (3) · green dot (2) · name (24 + 2) · `owner <nick>` (6 + 16 + 2)
/// · `n/8` (3 + 2) · age (`3m`).
pub fn room_row(index: usize, room: &RoomInfo, selected: bool) -> Line<'static> {
    let name = truncate(&sanitize(&room.name), MAX_ROOM_NAME);
    let name_pad = " ".repeat(MAX_ROOM_NAME + 2 - name.width());
    let owner = truncate(&sanitize(&room.owner), MAX_NICK);
    let owner_pad = " ".repeat(MAX_NICK + 2 - owner.width());
    let (mark, num_style, name_style) = if selected {
        ("▌ ", bold(ME), bold(THEM))
    } else {
        ("  ", fg(DIM), fg(FG))
    };
    let line = Line::from(vec![
        Span::styled(mark, fg(ME)),
        Span::styled(format!("{:<3}", index + 1), num_style),
        Span::styled("● ", fg(OK)),
        Span::styled(name, name_style),
        Span::raw(name_pad),
        Span::styled("owner ", fg(DIM)),
        Span::styled(owner, fg(FG)),
        Span::raw(owner_pad),
        Span::styled(format!("{}/{MAX_MEMBERS}  ", room.members), fg(DIM)),
        Span::styled(age(room.age_secs), fg(DIM)),
    ]);
    if selected {
        line.style(Style::default().bg(RAISE))
    } else {
        line
    }
}

/// A group of messages under one header, nick and rule in `color`. Peer: header `nick  HH:MM`,
/// lines on the left after a rule. Own (`group.mine`): header `HH:MM  nick` and lines right-aligned to `width`,
/// each ending in a rule. Lines wrap by display width.
pub fn message_block(group: &Group, nick: &str, color: Color, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let inner = width.saturating_sub(2).max(1);
    let time = format!("{:02}:{:02}", group.at.0, group.at.1);
    let nick = sanitize(nick);
    let pieces = group
        .lines
        .iter()
        .flat_map(|l| wrap(&sanitize(l), inner))
        .collect::<Vec<_>>();
    let mut out = Vec::with_capacity(pieces.len() + 1);
    if group.mine {
        let head_w = time.width() + 2 + nick.width();
        out.push(Line::from(vec![
            Span::raw(" ".repeat(width.saturating_sub(head_w))),
            Span::styled(time, fg(DIM)),
            Span::raw("  "),
            Span::styled(nick, bold(color)),
        ]));
        for p in pieces {
            let pad = width.saturating_sub(p.width() + 2);
            out.push(Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(p, fg(FG)),
                Span::raw(" "),
                Span::styled(OWN_RULE, fg(color)),
            ]));
        }
    } else {
        out.push(Line::from(vec![
            Span::styled(nick, bold(color)),
            Span::raw("  "),
            Span::styled(time, fg(DIM)),
        ]));
        for p in pieces {
            out.push(Line::from(vec![
                Span::styled(PEER_RULE, fg(color)),
                Span::raw(" "),
                Span::styled(p, fg(FG)),
            ]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::theme::{FAINT, ME, THEM};
    use super::*;

    fn cells(line: &Line, color: ratatui::style::Color) -> usize {
        line.spans
            .iter()
            .filter(|s| s.style.fg == Some(color))
            .map(|s| s.width())
            .sum()
    }

    #[test]
    fn drain_bar_halfway() {
        let l = drain_bar(Duration::from_secs(30), Duration::from_secs(60), 40);
        assert_eq!(cells(&l, ME), 20);
        assert_eq!(cells(&l, FAINT), 20);
        assert_eq!(l.width(), 40);
        let full = drain_bar(Duration::from_secs(90), Duration::from_secs(60), 40);
        assert_eq!(cells(&full, ME), 40);
        let none = drain_bar(Duration::from_secs(5), Duration::ZERO, 40);
        assert_eq!(cells(&none, FAINT), 40);
    }

    fn group_of(mine: bool, lines: &[&str]) -> Group {
        Group {
            mine,
            at: (14, 3),
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn own_message_is_right_aligned() {
        let ls = message_block(&group_of(true, &["hi"]), "user2", ME, 72);
        assert_eq!(ls.len(), 2);
        assert!(text(&ls[0]).ends_with("14:03  user2"));
        for l in &ls {
            assert_eq!(l.width(), 72);
        }
        let last = ls[1].spans.last().unwrap();
        assert_eq!(last.style.fg, Some(ME));
        assert_eq!(last.content, "▕");
        assert!(text(&ls[1]).ends_with("hi ▕"));
    }

    #[test]
    fn own_wide_chars_stay_aligned() {
        let ls = message_block(&group_of(true, &["🐦🐦🐦 日本"]), "me", ME, 9);
        assert!(ls.len() > 2);
        for l in &ls {
            assert_eq!(l.width(), 9, "{:?}", text(l));
        }
    }

    #[test]
    fn peer_message_left_with_rule_and_wraps() {
        let ls = message_block(&group_of(false, &["hi", "🐦🐦🐦🐦"]), "user1", THEM, 6);
        assert_eq!(text(&ls[0]), "user1  14:03");
        assert_eq!(ls.len(), 1 + 1 + 2);
        for l in &ls[1..] {
            assert_eq!(l.spans[0].content, "▏");
            assert_eq!(l.spans[0].style.fg, Some(THEM));
            assert!(l.width() <= 6);
        }
    }

    #[test]
    fn message_block_sanitizes() {
        let ls = message_block(
            &group_of(false, &["a\x1b[2Jb"]),
            "ev\x1b]0;x\x07il",
            THEM,
            40,
        );
        let all: String = ls.iter().map(text).collect();
        assert!(!all.contains('\x1b'));
        assert!(all.contains("evil") && all.contains("ab"));
    }

    #[test]
    fn block_uses_nick_color() {
        use super::super::theme::BLUE;
        let ls = message_block(&group_of(false, &["yo"]), "ana", BLUE, 40);
        assert_eq!(ls[0].spans[0].style.fg, Some(BLUE));
        assert_eq!(ls[1].spans[0].style.fg, Some(BLUE));
        let ls = message_block(&group_of(true, &["yo"]), "me", BLUE, 40);
        assert_eq!(ls[1].spans.last().unwrap().style.fg, Some(BLUE));
    }

    #[test]
    fn status_bar_fills_width() {
        let seg = |t: &str| Seg {
            text: t.into(),
            style: Style::default(),
        };
        let l = status_bar(&[seg(" pombo "), seg(" ● e2e ")], &[seg(" \\q leave ")], 60);
        assert_eq!(l.width(), 60);
        assert!(text(&l).starts_with(" pombo  ● e2e "));
        assert!(text(&l).ends_with(" \\q leave "));
        let narrow = status_bar(&[seg(" pombo "), seg(" ● e2e ")], &[seg(" \\q leave ")], 10);
        assert_eq!(narrow.width(), 10);
        assert!(text(&narrow).starts_with(" pombo "));
    }

    #[test]
    fn room_row_shows_name_owner_count_and_age() {
        let r = RoomInfo {
            name: "sala".into(),
            owner: "user1".into(),
            members: 3,
            age_secs: 185,
        };
        let t = text(&room_row(3, &r, true));
        assert!(t.starts_with("▌ 4  ● sala "), "{t:?}");
        assert!(t.contains("owner user1 ") && t.contains(" 3/8 "), "{t:?}");
        assert!(t.ends_with("3m"), "{t:?}");
        let t = text(&room_row(
            0,
            &RoomInfo {
                name: "a".into(),
                owner: "b".into(),
                members: 1,
                age_secs: 41,
            },
            false,
        ));
        assert!(
            t.starts_with("  1  ● a ") && t.ends_with("1/8  41s"),
            "{t:?}"
        );
        // Columns line up whatever the lengths.
        let long = RoomInfo {
            name: "x".repeat(24),
            owner: "y".repeat(16),
            members: 7,
            age_secs: 0,
        };
        let short = text(&room_row(0, &r, false));
        let long = text(&room_row(0, &long, false));
        assert_eq!(
            short.find("owner"),
            long.find("owner"),
            "{short:?} {long:?}"
        );
        assert_eq!(short.find("/8"), long.find("/8"), "{short:?} {long:?}");
        let evil = RoomInfo {
            name: "\x1b[2Jx".into(),
            owner: "\x07o".into(),
            members: 2,
            age_secs: 7200,
        };
        let t = text(&room_row(0, &evil, false));
        assert!(!t.contains('\x1b') && !t.contains('\x07'), "{t:?}");
        assert!(t.ends_with("2h"), "{t:?}");
    }
}
