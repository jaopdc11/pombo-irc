//! Text handling: making remote text inert, input cleanup, wrapping and grouping.
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    pub mine: bool,
    pub at: (u8, u8),
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub mine: bool,
    pub at: (u8, u8),
    pub lines: Vec<String>,
}

const ESC: char = '\x1b';
const BEL: char = '\x07';
const C1_ST: char = '\u{9c}';

/// What the scanner is inside of.
enum State {
    Text,
    /// Right after ESC.
    Esc,
    /// ESC followed by intermediates (0x20..=0x2F), waiting for the final byte.
    EscIntermediate,
    /// CSI (ESC `[` or C1 0x9B), waiting for the final byte 0x40..=0x7E.
    Csi,
    /// OSC / DCS / SOS / PM / APC string, until BEL or ST.
    Str,
    /// ESC seen inside a string: `\` ends it (ST), anything else is still string.
    StrEsc,
}

/// Bidirectional overrides/isolates: inert for the terminal but can reorder what is shown.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Makes text from the network inert for the terminal. Drops every ESC-introduced sequence
/// (CSI, OSC/DCS/SOS/PM/APC strings up to BEL or ST, two- and three-byte escapes), their C1
/// equivalents, all other C0 and C1 controls, DEL and bidi overrides. Tab becomes one space.
/// An unterminated sequence swallows the rest of the input. The result has no control chars.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut state = State::Text;
    for c in s.chars() {
        state = match state {
            State::Text => feed_text(&mut out, c),
            State::Esc => match c {
                '[' => State::Csi,
                ']' | 'P' | 'X' | '^' | '_' => State::Str,
                ESC => State::Esc,
                '\x20'..='\x2f' => State::EscIntermediate,
                '\x30'..='\x7e' => State::Text,
                // Not a valid escape: drop the ESC alone and treat `c` as text.
                c => feed_text(&mut out, c),
            },
            State::EscIntermediate => match c {
                '\x20'..='\x2f' => State::EscIntermediate,
                '\x30'..='\x7e' => State::Text,
                c => feed_text(&mut out, c),
            },
            State::Csi => match c {
                '\x20'..='\x3f' => State::Csi,
                '\x40'..='\x7e' => State::Text,
                // Malformed CSI: abandon it and treat `c` as text.
                c => feed_text(&mut out, c),
            },
            State::Str => match c {
                BEL | C1_ST => State::Text,
                ESC => State::StrEsc,
                _ => State::Str,
            },
            State::StrEsc => match c {
                '\\' | BEL | C1_ST => State::Text,
                ESC => State::StrEsc,
                _ => State::Str,
            },
        };
    }
    out
}

/// Handles one char in text state (also used for a char that ended a malformed sequence).
fn feed_text(out: &mut String, c: char) -> State {
    match c {
        ESC => State::Esc,
        '\u{9b}' => State::Csi,
        '\u{90}' | '\u{98}' | '\u{9d}' | '\u{9e}' | '\u{9f}' => State::Str,
        '\t' => {
            out.push(' ');
            State::Text
        }
        c if c.is_control() || is_bidi_control(c) => State::Text,
        c => {
            out.push(c);
            State::Text
        }
    }
}

/// Turns typed or pasted input into one chat line: `\r\n`, `\n` and lone `\r` become a
/// space, then trailing whitespace is trimmed.
pub fn input_to_line(s: &str) -> String {
    s.replace("\r\n", " ")
        .replace(['\n', '\r'], " ")
        .trim_end()
        .to_string()
}

/// Word-wraps `s` to `width` display columns. Words longer than a line are broken between
/// chars; a char wider than `width` sits alone on its line. Never splits a char. Always
/// returns at least one line. A `width` of 0 is treated as 1.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0;
    for word in s.split(' ') {
        let word_w: usize = word.chars().map(|c| c.width().unwrap_or(0)).sum();
        if !cur.is_empty() && cur_w + 1 + word_w <= width {
            cur.push(' ');
            cur.push_str(word);
            cur_w += 1 + word_w;
            continue;
        }
        if word.is_empty() {
            // A space that falls on the wrap point is dropped.
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        for c in word.chars() {
            let w = c.width().unwrap_or(0);
            if cur_w + w > width && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            cur.push(c);
            cur_w += w;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Merges consecutive messages from the same side; a group takes its first message's time.
pub fn group(msgs: &[Msg]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for m in msgs {
        match groups.last_mut() {
            Some(g) if g.mine == m.mine => g.lines.push(m.text.clone()),
            _ => groups.push(Group {
                mine: m.mine,
                at: m.at,
                lines: vec![m.text.clone()],
            }),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn sanitize_strips_escapes() {
        assert_eq!(sanitize("\x1b[2J\x1b[31mhi\x07"), "hi");
        assert_eq!(sanitize("a\x1b]0;pwned\x07b"), "ab"); // OSC title set
        assert_eq!(sanitize("x\u{9b}31my"), "xy"); // C1 CSI
        assert_eq!(sanitize("tab\there"), "tab here");
        assert_eq!(sanitize("tá 🐦"), "tá 🐦");
    }

    #[test]
    fn sanitize_osc_terminated_by_st() {
        assert_eq!(
            sanitize("a\x1b]8;;http://evil\x1b\\link\x1b]8;;\x1b\\b"),
            "alinkb"
        );
        assert_eq!(sanitize("a\x1b]0;t\u{9c}b"), "ab"); // C1 ST
        assert_eq!(sanitize("a\u{9d}0;t\x07b"), "ab"); // C1 OSC
    }

    #[test]
    fn sanitize_unterminated_strings_drop_the_rest() {
        assert_eq!(sanitize("a\x1b]0;never ends"), "a");
        assert_eq!(sanitize("a\x1bP+q544e\x1b\\b"), "ab"); // DCS
        assert_eq!(sanitize("a\x1b_apc\x07b"), "ab"); // APC
        assert_eq!(sanitize("a\x1b[1;3"), "a"); // CSI cut short
        assert_eq!(sanitize("a\x1b"), "a");
    }

    #[test]
    fn sanitize_other_escapes() {
        assert_eq!(sanitize("a\x1bcb"), "ab"); // RIS (full reset)
        assert_eq!(sanitize("a\x1b(Bb"), "ab"); // charset
        assert_eq!(sanitize("a\x1b#8b"), "ab"); // DECALN
        assert_eq!(sanitize("a\x1b[?1049hb"), "ab"); // private CSI
        assert_eq!(sanitize("a\x1b\x1b[31mb"), "ab"); // ESC ESC
        assert_eq!(sanitize("a\x1b\u{e9}b"), "aéb"); // ESC + non-ASCII: ESC alone dropped
    }

    #[test]
    fn sanitize_drops_controls() {
        assert_eq!(sanitize("a\x00b\x08c\rd\ne\x7ff"), "abcdef");
        assert_eq!(sanitize("a\u{80}b\u{85}c\u{9a}d"), "abcd");
        assert_eq!(sanitize("a\u{9f}apc\u{9c}b"), "ab"); // C1 APC string
        assert_eq!(sanitize("a\u{202e}b\u{2066}c\u{2069}"), "abc"); // bidi overrides
    }

    #[test]
    fn sanitize_is_idempotent_and_clean() {
        let nasty = "\x1b]0;x\x07\x1b[31m\u{9b}2J\tok\u{90}q\x1b\\\x1bZ";
        let once = sanitize(nasty);
        assert_eq!(once, " ok");
        assert_eq!(sanitize(&once), once);
        for b in 0u32..=0xff {
            let c = char::from_u32(b).unwrap();
            let s = sanitize(&format!("a{c}b"));
            assert!(!s.chars().any(|c| c.is_control()), "{b:#x} -> {s:?}");
        }
    }

    #[test]
    fn pasted_newlines_become_spaces() {
        assert_eq!(input_to_line("a\nb\r\nc\n"), "a b c");
        assert_eq!(input_to_line("a\rb"), "a b");
        assert_eq!(input_to_line("  keep lead  \n"), "  keep lead");
    }

    #[test]
    fn wrap_by_display_width() {
        assert_eq!(wrap("🐦🐦🐦", 4), vec!["🐦🐦", "🐦"]);
        assert_eq!(wrap("abc def", 3), vec!["abc", "def"]);
    }

    #[test]
    fn wrap_edges() {
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("short", 10), vec!["short"]);
        assert_eq!(wrap("ab cd ef", 5), vec!["ab cd", "ef"]);
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
        assert_eq!(wrap("🐦🐦", 1), vec!["🐦", "🐦"]); // wider than width: alone, never split
        assert_eq!(wrap("ab", 0), vec!["a", "b"]);
        assert_eq!(wrap("abc  def", 3), vec!["abc", "def"]);
        assert_eq!(wrap("abc ", 3), vec!["abc"]);
        assert_eq!(wrap("a  b", 10), vec!["a  b"]);
        for l in wrap("tá na rede do cliente? 日本語のテキスト 🐦🐦 ok", 7) {
            assert!(l.width() <= 7, "{l:?}");
        }
    }

    #[test]
    fn grouping() {
        let m = |mine, at, t: &str| Msg {
            mine,
            at,
            text: t.into(),
        };
        let g = group(&[
            m(false, (14, 2), "hi"),
            m(false, (14, 3), "tá?"),
            m(true, (14, 3), "hi"),
            m(false, (14, 3), "x"),
        ]);
        assert_eq!(g.len(), 3);
        assert_eq!(
            g[0],
            Group {
                mine: false,
                at: (14, 2),
                lines: vec!["hi".into(), "tá?".into()]
            }
        );
        assert!(g[1].mine);
        assert_eq!(g[2].lines, vec!["x"]);
        assert!(group(&[]).is_empty());
    }
}
