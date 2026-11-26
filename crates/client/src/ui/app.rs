//! Screens and the event loop. One inline ratatui viewport (never the alternate screen) holds
//! the live part of each screen; room text scrolls above it through `insert_before`.
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use crossterm::cursor::{MoveTo, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};
use proto::{RoomInfo, MAX_LINE, MAX_MEMBERS, REQUEST_TIMEOUT_SECS};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use unicode_width::UnicodeWidthStr;
use zeroize::{Zeroize, Zeroizing};

use super::logo;
use super::text::{input_to_line, sanitize, Group};
use super::theme::{bold, chip, fg, nick_hue, BAD, DIM, FAINT, FG, ME, OK, RAISE, SURFACE, WARN};
use super::tty::{self, Tty};
use super::widgets::{drain_bar, message_block, room_row, status_bar, Seg};
use crate::core::{Conn, ConnectError, CoreError, CreateError, LobbyEvent, Room, RoomEvent};
use crate::harden;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(90);
const TYPING_EVERY: Duration = Duration::from_secs(2);
const TYPING_HIDE: Duration = Duration::from_secs(4);
const REQUEST_TTL: Duration = Duration::from_secs(REQUEST_TIMEOUT_SECS);
/// Input buffer size: room for an over-limit line so the "too long" notice can show.
const INPUT_CAP: usize = MAX_LINE * 2;
/// Most room rows shown at once.
const ROOM_ROWS: usize = 10;

type Term = Terminal<Tty>;

/// Restores the terminal however `run` ends.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, Show);
    }
}

enum Screen {
    Nick {
        error: Option<String>,
    },
    Menu {
        lit: Option<u8>,
    },
    Name {
        error: Option<String>,
    },
    Rooms {
        list: Vec<RoomInfo>,
        sel: usize,
        notice: Option<(String, Color)>,
    },
    Knock {
        room: String,
        since: Instant,
    },
    Room(RoomView),
}

#[derive(Default)]
struct RoomView {
    /// Owner: pending requests, newest last.
    requests: Vec<(String, Instant)>,
    typing: Vec<(String, Instant)>,
    colors: HashMap<String, Color>,
    next_hue: usize,
    /// Sender of the last message group (`None` after a system line).
    last_sender: Option<String>,
    notice: Option<(String, Color)>,
    closed: bool,
    typing_sent: Option<Instant>,
}

impl RoomView {
    fn color(&mut self, nick: &str, me: &str) -> Color {
        if nick == me {
            return ME;
        }
        if let Some(c) = self.colors.get(nick) {
            return *c;
        }
        let c = nick_hue(self.next_hue);
        self.next_hue += 1;
        self.colors.insert(nick.to_owned(), c);
        c
    }
}

enum Input {
    Term(Option<io::Result<Event>>),
    Lobby(Result<LobbyEvent, CoreError>),
    Room(Result<RoomEvent, CoreError>),
    Tick,
}

/// Shown when the relay runs a different commit; the client refuses to run.
const UPDATE_REQUIRED: &str =
    "this version is out of date. Update it from the folder you cloned: git pull && ./install.sh";

struct App {
    url: String,
    term: Term,
    height: u16,
    screen: Screen,
    nick: String,
    conn: Option<Conn>,
    room: Option<Room>,
    input: Zeroizing<String>,
    started: Instant,
    quit: bool,
}

/// Runs the client against `url` until the user quits.
pub async fn run(url: String) -> io::Result<()> {
    enable_raw_mode()?;
    let _restore = Restore;
    // Consoles without VT support (old conhost) refuse this; typed input still works there.
    let _ = execute!(io::stdout(), EnableBracketedPaste);
    let mut input = Zeroizing::new(String::with_capacity(INPUT_CAP));
    input.extend(std::iter::repeat_n('\0', INPUT_CAP));
    harden::lock(input.as_bytes());
    input.clear();
    let mut app = App {
        url,
        term: new_term(1)?,
        height: 1,
        screen: Screen::Nick { error: None },
        nick: String::new(),
        conn: None,
        room: None,
        input,
        started: Instant::now(),
        quit: false,
    };
    let result = app.event_loop().await;
    let _ = app.term.clear();
    if let Some(room) = app.room.take() {
        room.leave().await;
    }
    result
}

fn new_term(height: u16) -> io::Result<Term> {
    Terminal::with_options(
        Tty::stdout(),
        TerminalOptions {
            viewport: Viewport::Inline(height.max(1)),
        },
    )
}

/// Local wall-clock time as (hour, minute).
#[cfg(windows)]
fn now_hm() -> (u8, u8) {
    // SAFETY: GetLocalTime fills the local struct it is given.
    let t = unsafe {
        let mut t: windows_sys::Win32::Foundation::SYSTEMTIME = std::mem::zeroed();
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut t);
        t
    };
    (t.wHour as u8, t.wMinute as u8)
}

/// Local wall-clock time as (hour, minute).
#[cfg(unix)]
fn now_hm() -> (u8, u8) {
    // SAFETY: `time` accepts a null pointer; `localtime_r` writes into a local, zeroed `tm`.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return (0, 0);
        }
        (tm.tm_hour as u8, tm.tm_min as u8)
    }
}

async fn lobby_next(conn: &mut Option<Conn>, active: bool) -> Result<LobbyEvent, CoreError> {
    match conn {
        Some(c) if active => c.next_event().await,
        _ => std::future::pending().await,
    }
}

async fn room_next(room: &mut Option<Room>, active: bool) -> Result<RoomEvent, CoreError> {
    match room {
        Some(r) if active => r.next().await,
        _ => std::future::pending().await,
    }
}

fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg {
        text: text.into(),
        style,
    }
}

fn brand(closed: bool) -> Seg {
    seg(" pombo-irc ", chip(if closed { BAD } else { ME }))
}

/// `› text▏`, showing the tail of the input when it is wider than the screen.
fn input_line(prompt: &str, text: &str, width: u16, enabled: bool) -> Line<'static> {
    let room = usize::from(width).saturating_sub(prompt.width() + 2).max(1);
    let mut shown = String::new();
    let mut w = 0;
    for c in text.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > room {
            break;
        }
        shown.insert(0, c);
        w += cw;
    }
    let (p, t) = if enabled {
        (bold(ME), fg(FG))
    } else {
        (fg(FAINT), fg(FAINT))
    };
    let mut spans = vec![Span::styled(prompt.to_owned(), p), Span::styled(shown, t)];
    if enabled {
        spans.push(Span::styled(" ", Style::default().bg(ME)));
    }
    Line::from(spans).style(Style::default().bg(SURFACE))
}

fn centered(text: &str, color: Color, width: u16) -> Line<'static> {
    let text = format!(" {text} ");
    let side = usize::from(width).saturating_sub(text.width()) / 2;
    Line::from(vec![
        Span::styled("─".repeat(side), fg(FAINT)),
        Span::styled(text, fg(color)),
        Span::styled("─".repeat(side), fg(FAINT)),
    ])
}

impl App {
    fn width(&self) -> u16 {
        self.term.size().map(|s| s.width).unwrap_or(80).max(10)
    }

    fn spinner(&self) -> &'static str {
        let n = self.started.elapsed().as_millis() / TICK.as_millis();
        SPINNER[(n as usize) % SPINNER.len()]
    }

    async fn event_loop(&mut self) -> io::Result<()> {
        let mut events = tty::input();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while !self.quit {
            self.draw()?;
            let knocking = matches!(self.screen, Screen::Knock { .. });
            let in_room = matches!(&self.screen, Screen::Room(v) if !v.closed);
            let input = tokio::select! {
                e = events.recv() => Input::Term(e),
                e = lobby_next(&mut self.conn, knocking) => Input::Lobby(e),
                e = room_next(&mut self.room, in_room) => Input::Room(e),
                _ = tick.tick() => Input::Tick,
            };
            match input {
                Input::Term(None) => self.quit = true,
                Input::Term(Some(Err(e))) => return Err(e),
                Input::Term(Some(Ok(ev))) => self.on_term(ev).await?,
                Input::Lobby(ev) => self.on_lobby(ev).await?,
                Input::Room(ev) => self.on_room(ev)?,
                Input::Tick => self.on_tick(),
            }
        }
        Ok(())
    }

    // ---------- drawing ----------

    fn view(&self, width: u16) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        let host_seg = seg(" ● server ".to_owned(), fg(OK));
        match &self.screen {
            Screen::Nick { error } => {
                out.extend(logo::lines(logo::pick(width)));
                out.push(Line::styled(logo::TAGLINE, fg(DIM)));
                out.push(Line::default());
                out.push(input_line("nick › ", &self.input, width, true));
                out.push(match error {
                    Some(e) => Line::styled(format!("  {e}"), fg(BAD)),
                    None => Line::default(),
                });
                out.push(status_bar(
                    &[brand(false), host_seg],
                    &[seg(" ^C quit ", fg(DIM))],
                    width,
                ));
            }
            Screen::Menu { lit } => {
                out.push(self.you_are());
                out.push(Line::default());
                let w = (usize::from(width).saturating_sub(2) / 2).clamp(10, 34);
                let block = |n: u8, title: &str, sub: &str| -> [Span<'static>; 3] {
                    let on = *lit == Some(n);
                    let bg = if on { ME } else { RAISE };
                    let (k, t, s) = if on {
                        (bold(Color::Reset), bold(Color::Reset), fg(Color::Reset))
                    } else {
                        (bold(ME), bold(FG), fg(DIM))
                    };
                    let pad = |s: String| format!("{s:<w$}");
                    [
                        Span::styled(pad(String::new()), Style::default().bg(bg)),
                        Span::styled(pad(format!("  {n}  {title}")), k.patch(t).bg(bg)),
                        Span::styled(pad(format!("     {sub}")), s.bg(bg)),
                    ]
                };
                let a = block(1, "create room", "open a room, let people knock");
                let b = block(2, "enter room", "knock on an open room");
                for (x, y) in a.into_iter().zip(b) {
                    out.push(Line::from(vec![x, Span::raw("  "), y]));
                }
                out.push(Line::default());
                out.push(status_bar(
                    &[brand(false), host_seg],
                    &[seg(" 1 / 2 choose ", fg(DIM)), seg(" ^C quit ", fg(DIM))],
                    width,
                ));
            }
            Screen::Name { error } => {
                out.push(self.you_are());
                out.push(Line::default());
                out.push(input_line("room name › ", &self.input, width, true));
                out.push(match error {
                    Some(e) => Line::styled(format!("  {e}"), fg(BAD)),
                    None => Line::default(),
                });
                out.push(status_bar(
                    &[brand(false), host_seg],
                    &[seg(" \\q back ", fg(DIM))],
                    width,
                ));
            }
            Screen::Rooms { list, sel, notice } => {
                out.push(Line::from(vec![
                    Span::styled("open rooms", bold(FG)),
                    Span::styled(format!(" · {}", list.len()), fg(DIM)),
                ]));
                out.push(Line::default());
                if list.is_empty() {
                    out.push(Line::styled("  no rooms open", fg(DIM)));
                } else {
                    let start = sel.saturating_sub(ROOM_ROWS - 1);
                    for (i, r) in list.iter().enumerate().skip(start).take(ROOM_ROWS) {
                        out.push(room_row(i, r, i == *sel));
                    }
                }
                out.push(Line::default());
                out.push(match notice {
                    Some((t, c)) => Line::styled(format!("  {t}"), fg(*c)),
                    None => Line::default(),
                });
                out.push(input_line("› ", &self.input, width, true));
                out.push(status_bar(
                    &[brand(false), host_seg],
                    &[
                        seg(" ↑↓ / number + enter ", fg(DIM)),
                        seg(" r refresh ", fg(DIM)),
                        seg(" \\q leave ", fg(DIM)),
                    ],
                    width,
                ));
            }
            Screen::Knock { room, since } => {
                out.push(Line::from(vec![
                    Span::styled(format!("{} ", self.spinner()), fg(ME)),
                    Span::styled("knocking on ", fg(FG)),
                    Span::styled(sanitize(room), bold(FG)),
                    Span::styled("...", fg(FG)),
                ]));
                let left = REQUEST_TTL.saturating_sub(since.elapsed());
                out.push(drain_bar(
                    left,
                    REQUEST_TTL,
                    width.saturating_sub(2).min(40),
                ));
                out.push(Line::styled(
                    format!("waiting for the owner to answer · {}s", left.as_secs()),
                    fg(DIM),
                ));
                out.push(input_line("› ", &self.input, width, true));
                out.push(status_bar(
                    &[brand(false), host_seg],
                    &[seg(" \\q leave ", fg(DIM))],
                    width,
                ));
            }
            Screen::Room(v) => self.room_view(v, width, &mut out),
        }
        out
    }

    fn you_are(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("✓ ", fg(OK)),
            Span::styled("you are ", fg(FG)),
            Span::styled(sanitize(&self.nick), bold(ME)),
            Span::styled(" while you stay online", fg(FG)),
        ])
    }

    fn room_view(&self, v: &RoomView, width: u16, out: &mut Vec<Line<'static>>) {
        let Some(room) = &self.room else { return };
        if let Some((from, at)) = v.requests.last().filter(|_| !v.closed) {
            let edge = || Span::styled("▌ ", fg(WARN));
            let bg = Style::default().bg(SURFACE);
            out.push(
                Line::from(vec![
                    edge(),
                    Span::styled("request  ", bold(WARN)),
                    Span::styled(
                        sanitize(from),
                        bold(v.colors.get(from).copied().unwrap_or(FG)),
                    ),
                    Span::styled(" wants to join", fg(FG)),
                ])
                .style(bg),
            );
            let mut keys = vec![
                edge(),
                Span::styled(" \\y ", chip(OK)),
                Span::styled(" accept   ", fg(FG)),
                Span::styled(" \\n ", chip(BAD)),
                Span::styled(" refuse", fg(FG)),
            ];
            if v.requests.len() > 1 {
                keys.push(Span::styled(
                    format!("   +{} more waiting", v.requests.len() - 1),
                    fg(DIM),
                ));
            }
            out.push(Line::from(keys).style(bg));
            let left = REQUEST_TTL.saturating_sub(at.elapsed());
            let mut bar = drain_bar(left, REQUEST_TTL, width.saturating_sub(4).min(40));
            bar.spans.insert(0, edge());
            out.push(bar.style(bg));
        }
        // One line above the input: notice, typing, or the empty-room hint.
        let typing: Vec<&str> = v.typing.iter().map(|(n, _)| n.as_str()).collect();
        out.push(if v.closed {
            Line::default()
        } else if let Some((t, c)) = &v.notice {
            Line::styled(format!("  {t}"), fg(*c))
        } else if !typing.is_empty() {
            let names = typing
                .iter()
                .map(|n| sanitize(n))
                .collect::<Vec<_>>()
                .join(", ");
            let verb = if typing.len() == 1 { "is" } else { "are" };
            Line::styled(format!("  {names} {verb} typing..."), fg(DIM))
        } else if room.members().len() <= 1 {
            Line::from(vec![
                Span::styled(format!("  {} ", self.spinner()), fg(ME)),
                Span::styled("waiting for people to knock", fg(DIM)),
            ])
        } else {
            Line::default()
        });
        if v.closed {
            let shown = if self.input.is_empty() {
                "input closed"
            } else {
                self.input.as_str()
            };
            out.push(input_line("› ", shown, width, false));
        } else {
            out.push(input_line("› ", &self.input, width, true));
        }
        let mut left = vec![brand(v.closed)];
        if v.closed {
            left.push(seg(" ○ closed ", fg(BAD)));
        } else {
            left.push(seg(" ● e2e ", fg(OK)));
        }
        left.push(seg(format!(" {} ", room.name()), bold(FG)));
        left.push(seg(
            format!(" {}/{MAX_MEMBERS} ", room.members().len()),
            fg(DIM),
        ));
        let mut right = Vec::new();
        if !v.closed {
            if room.is_owner() {
                right.push(seg(" you own this room ", fg(ME)));
            } else if let Some(fp) = room.fingerprint() {
                right.push(seg(format!(" fingerprint {fp} "), fg(WARN)));
            } else {
                right.push(seg(" securing room... ", fg(WARN)));
            }
            right.push(seg(" \\q leave ", fg(DIM)));
        } else {
            right.push(seg(" \\q clear and return ", fg(DIM)));
        }
        out.push(status_bar(&left, &right, width));
    }

    fn draw(&mut self) -> io::Result<()> {
        let width = self.width();
        let lines = self.view(width);
        let want = lines.len() as u16;
        if want != self.height {
            self.term.clear()?;
            self.term = new_term(want)?;
            self.height = want;
        }
        self.term.draw(|f| {
            let area = f.area();
            let skip = lines.len().saturating_sub(usize::from(area.height));
            Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>())
                .render(area, f.buffer_mut());
        })?;
        Ok(())
    }

    /// Prints `lines` above the live region.
    fn print(&mut self, lines: Vec<Line<'static>>) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let n = lines.len() as u16;
        self.term.insert_before(n, |buf| {
            let area = Rect::new(0, 0, buf.area.width, buf.area.height);
            Paragraph::new(lines).render(area, buf);
        })
    }

    /// Wipes the screen and its scrollback and starts a fresh live region at the top.
    fn wipe(&mut self) -> io::Result<()> {
        self.term.clear()?;
        execute!(
            io::stdout(),
            Clear(ClearType::All),
            Clear(ClearType::Purge),
            MoveTo(0, 0)
        )?;
        self.term = new_term(self.height)?;
        Ok(())
    }

    fn clear_input(&mut self) {
        self.input.zeroize();
        self.input.clear();
    }

    /// Ends whatever connection there is, wipes the screen and goes back to the nick prompt.
    async fn back_to_nick(&mut self, error: Option<String>) -> io::Result<()> {
        if let Some(room) = self.room.take() {
            room.leave().await;
        }
        self.conn = None;
        self.clear_input();
        self.screen = Screen::Nick { error };
        self.wipe()
    }

    // ---------- input ----------

    async fn on_term(&mut self, ev: Event) -> io::Result<()> {
        match ev {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k).await,
            Event::Paste(p) => {
                let p = Zeroizing::new(p);
                let line = Zeroizing::new(input_to_line(&p));
                for c in line.chars() {
                    self.push_char(c);
                }
                self.typed().await;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn push_char(&mut self, c: char) {
        if c.is_control() || self.input.len() + c.len_utf8() > INPUT_CAP {
            return;
        }
        self.input.push(c);
    }

    async fn on_key(&mut self, k: KeyEvent) -> io::Result<()> {
        let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return match self.screen {
                Screen::Nick { .. } | Screen::Menu { .. } => {
                    self.quit = true;
                    Ok(())
                }
                _ => self.back_to_nick(None).await,
            };
        }
        if let Screen::Menu { .. } = self.screen {
            return self.menu_key(k).await;
        }
        if let Screen::Rooms { list, sel, .. } = &mut self.screen {
            match k.code {
                KeyCode::Up => {
                    *sel = sel.saturating_sub(1);
                    return Ok(());
                }
                KeyCode::Down => {
                    if *sel + 1 < list.len() {
                        *sel += 1;
                    }
                    return Ok(());
                }
                KeyCode::Char('r') if self.input.is_empty() => return self.refresh(None).await,
                _ => {}
            }
        }
        match k.code {
            KeyCode::Enter => {
                let line = Zeroizing::new(input_to_line(&self.input));
                self.clear_input();
                self.submit(line).await
            }
            KeyCode::Backspace => {
                self.input.pop();
                Ok(())
            }
            KeyCode::Esc => {
                self.clear_input();
                Ok(())
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.push_char(c);
                self.typed().await;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// After the input changed: clears a stale notice and sends a throttled typing event.
    async fn typed(&mut self) {
        let Screen::Room(v) = &mut self.screen else {
            return;
        };
        if v.closed {
            return;
        }
        if matches!(&v.notice, Some((_, c)) if *c == BAD) {
            v.notice = None;
        }
        if self.input.is_empty() || self.input.starts_with('\\') {
            return;
        }
        if v.typing_sent.is_some_and(|t| t.elapsed() < TYPING_EVERY) {
            return;
        }
        v.typing_sent = Some(Instant::now());
        if let Some(room) = &mut self.room {
            let _ = room.send_typing().await;
        }
    }

    async fn menu_key(&mut self, k: KeyEvent) -> io::Result<()> {
        let n = match k.code {
            KeyCode::Char('1') => 1,
            KeyCode::Char('2') => 2,
            _ => return Ok(()),
        };
        self.screen = Screen::Menu { lit: Some(n) };
        self.draw()?;
        tokio::time::sleep(Duration::from_millis(120)).await;
        if n == 1 {
            self.screen = Screen::Name { error: None };
            Ok(())
        } else {
            self.refresh(None).await
        }
    }

    /// Loads the room list and shows it.
    async fn refresh(&mut self, notice: Option<(String, Color)>) -> io::Result<()> {
        let Some(conn) = &mut self.conn else {
            return self.back_to_nick(None).await;
        };
        match conn.list_rooms().await {
            Ok(list) => {
                let sel = match &self.screen {
                    Screen::Rooms { sel, .. } => (*sel).min(list.len().saturating_sub(1)),
                    _ => 0,
                };
                self.screen = Screen::Rooms { list, sel, notice };
                Ok(())
            }
            Err(CoreError::Timeout) => {
                let notice = Some(("server did not answer · press r to retry".into(), BAD));
                if let Screen::Rooms { notice: n, .. } = &mut self.screen {
                    *n = notice;
                } else {
                    self.screen = Screen::Rooms {
                        list: Vec::new(),
                        sel: 0,
                        notice,
                    };
                }
                Ok(())
            }
            Err(_) => {
                let e = "connection to the server lost".to_owned();
                self.back_to_nick(Some(e)).await
            }
        }
    }

    async fn submit(&mut self, line: Zeroizing<String>) -> io::Result<()> {
        let quit = line.as_str() == "\\q";
        match &mut self.screen {
            Screen::Nick { .. } => self.submit_nick(line.trim()).await,
            Screen::Menu { .. } => Ok(()),
            Screen::Name { .. } if quit => {
                self.screen = Screen::Menu { lit: None };
                Ok(())
            }
            Screen::Name { .. } => self.submit_name(line.trim()).await,
            Screen::Rooms { .. } | Screen::Knock { .. } if quit => self.back_to_nick(None).await,
            Screen::Knock { .. } => Ok(()),
            Screen::Rooms { list, sel, .. } => {
                let t = line.trim();
                let pick = if t.is_empty() {
                    list.get(*sel)
                } else {
                    t.parse::<usize>()
                        .ok()
                        .and_then(|n| n.checked_sub(1))
                        .and_then(|i| list.get(i))
                };
                let Some(room) = pick.map(|r| r.name.clone()) else {
                    if let Screen::Rooms { notice, .. } = &mut self.screen {
                        *notice = Some(("pick a room by its number".into(), WARN));
                    }
                    return Ok(());
                };
                let Some(conn) = &mut self.conn else {
                    return self.back_to_nick(None).await;
                };
                match conn.join(&room).await {
                    Ok(()) => {
                        self.screen = Screen::Knock {
                            room,
                            since: Instant::now(),
                        };
                        Ok(())
                    }
                    Err(_) => {
                        let e = "connection to the server lost".to_owned();
                        self.back_to_nick(Some(e)).await
                    }
                }
            }
            Screen::Room(v) if v.closed => {
                if quit {
                    self.back_to_nick(None).await
                } else {
                    Ok(())
                }
            }
            Screen::Room(_) => self.submit_room(line).await,
        }
    }

    async fn submit_nick(&mut self, nick: &str) -> io::Result<()> {
        if nick.is_empty() {
            return Ok(());
        }
        self.screen = Screen::Nick {
            error: Some("connecting to the server...".to_owned()),
        };
        self.draw()?;
        match Conn::connect(&self.url, nick).await {
            Ok(conn) => {
                self.nick = nick.to_owned();
                self.conn = Some(conn);
                self.screen = Screen::Menu { lit: None };
            }
            Err(e) => {
                let msg = match e {
                    ConnectError::NickTaken => "nick taken".to_owned(),
                    ConnectError::NickInvalid => {
                        "nick must be 1-16 letters, digits, _ or -".to_owned()
                    }
                    ConnectError::Network(_) => "cannot reach the server".to_owned(),
                    ConnectError::UpdateRequired => {
                        return Err(io::Error::other(UPDATE_REQUIRED));
                    }
                };
                self.screen = Screen::Nick { error: Some(msg) };
            }
        }
        Ok(())
    }

    async fn submit_name(&mut self, name: &str) -> io::Result<()> {
        const INVALID: &str = "name must be 1-24 letters, digits, _ or -";
        if !proto::valid_room_name(name) {
            self.screen = Screen::Name {
                error: Some(INVALID.into()),
            };
            return Ok(());
        }
        let Some(conn) = self.conn.take() else {
            return self.back_to_nick(None).await;
        };
        match conn.create_room(name).await {
            Ok(room) => self.enter_room(room).await,
            Err((conn, e)) => {
                self.conn = Some(conn);
                let msg = match e {
                    CreateError::NameTaken => "name taken",
                    CreateError::NameInvalid => INVALID,
                    CreateError::Core(CoreError::Timeout) => "server did not answer",
                    CreateError::Core(_) => {
                        let e = "connection to the server lost".to_owned();
                        return self.back_to_nick(Some(e)).await;
                    }
                };
                self.screen = Screen::Name {
                    error: Some(msg.into()),
                };
                Ok(())
            }
        }
    }

    async fn submit_room(&mut self, line: Zeroizing<String>) -> io::Result<()> {
        let (Screen::Room(v), Some(room)) = (&mut self.screen, &mut self.room) else {
            return Ok(());
        };
        match line.as_str() {
            "" => Ok(()),
            "\\q" => self.back_to_nick(None).await,
            "\\y" | "\\n" => {
                let Some((from, _)) = v.requests.pop() else {
                    v.notice = Some(("no request waiting".into(), DIM));
                    return Ok(());
                };
                let r = if line.as_str() == "\\y" {
                    room.accept(&from).await
                } else {
                    room.refuse(&from).await
                };
                if r.is_err() {
                    return self.lost();
                }
                Ok(())
            }
            text => match room.send_line(text).await {
                Ok(()) => {
                    v.notice = None;
                    v.typing_sent = None;
                    let me = room.me().to_owned();
                    let width = self.width();
                    self.print_message(&me, text, true, width)
                }
                Err(CoreError::TooLong) => {
                    v.notice = Some((format!("message too long (max {MAX_LINE} bytes)"), BAD));
                    self.input.push_str(text);
                    Ok(())
                }
                Err(CoreError::NotReady) => {
                    v.notice = Some(("securing room...".into(), WARN));
                    self.input.push_str(text);
                    Ok(())
                }
                Err(_) => self.lost(),
            },
        }
    }

    // ---------- room ----------

    async fn enter_room(&mut self, room: Room) -> io::Result<()> {
        let me = room.me().to_owned();
        let mut v = RoomView::default();
        for m in room.members() {
            v.color(m, &me);
        }
        let mut head = Vec::new();
        if room.is_owner() && room.members().len() <= 1 {
            head.push(Line::from(vec![
                Span::styled("✓ ", fg(OK)),
                Span::styled("room ", fg(FG)),
                Span::styled(sanitize(room.name()), bold(FG)),
                Span::styled(" open as ", fg(FG)),
                Span::styled(sanitize(&me), bold(ME)),
                Span::styled("  ·  you own this room", fg(DIM)),
            ]));
        } else {
            head.push(Line::from(vec![
                Span::styled("✓ ", fg(OK)),
                Span::styled("joined ", fg(FG)),
                Span::styled(sanitize(room.name()), bold(FG)),
                Span::styled("  ·  Noise NN  ·  XChaCha20-Poly1305", fg(DIM)),
            ]));
            let mut who = vec![Span::styled("  in the room: ", fg(DIM))];
            for (i, m) in room.members().iter().enumerate() {
                if i > 0 {
                    who.push(Span::styled(", ", fg(DIM)));
                }
                who.push(Span::styled(sanitize(m), bold(v.color(m, &me))));
            }
            head.push(Line::from(who));
            head.push(Line::styled(
                format!(
                    "  compare the fingerprint in the status bar with {} by another channel",
                    sanitize(room.owner())
                ),
                fg(DIM),
            ));
        }
        head.push(Line::default());
        self.room = Some(room);
        self.conn = None;
        self.clear_input();
        self.screen = Screen::Room(v);
        self.draw()?;
        self.wipe()?;
        self.print(head)
    }

    /// Prints one chat line, under the sender's header unless it continues their group.
    fn print_message(&mut self, nick: &str, text: &str, mine: bool, width: u16) -> io::Result<()> {
        let Screen::Room(v) = &mut self.screen else {
            return Ok(());
        };
        let me = self
            .room
            .as_ref()
            .map(|r| r.me().to_owned())
            .unwrap_or_default();
        let color = v.color(nick, &me);
        let continues = v.last_sender.as_deref() == Some(nick);
        let mut group = Group {
            mine,
            at: now_hm(),
            lines: vec![text.to_owned()],
        };
        let mut lines = message_block(&group, nick, color, width);
        group.lines.iter_mut().for_each(Zeroize::zeroize);
        if continues {
            lines.remove(0);
        } else if v.last_sender.is_some() {
            lines.insert(0, Line::default());
        }
        v.last_sender = Some(nick.to_owned());
        self.print(lines)
    }

    /// Prints a system line (joined, left, ...) and breaks the current message group.
    fn print_system(&mut self, lines: Vec<Line<'static>>) -> io::Result<()> {
        if let Screen::Room(v) = &mut self.screen {
            v.last_sender = None;
        }
        self.print(lines)
    }

    fn lost(&mut self) -> io::Result<()> {
        let width = self.width();
        if let Screen::Room(v) = &mut self.screen {
            if v.closed {
                return Ok(());
            }
            v.closed = true;
            v.requests.clear();
            v.typing.clear();
            v.notice = None;
        }
        self.clear_input();
        self.print_system(vec![
            Line::default(),
            centered("connection lost · keys discarded", BAD, width),
        ])
    }

    fn on_room(&mut self, ev: Result<RoomEvent, CoreError>) -> io::Result<()> {
        let width = self.width();
        let (Screen::Room(v), Some(room)) = (&mut self.screen, &self.room) else {
            return Ok(());
        };
        let me = room.me().to_owned();
        let ev = match ev {
            Ok(RoomEvent::ConnectionLost) | Err(_) => return self.lost(),
            Ok(ev) => ev,
        };
        match ev {
            RoomEvent::Line { from, text } => {
                v.typing.retain(|(n, _)| *n != from);
                self.print_message(&from, &text, false, width)
            }
            RoomEvent::Typing { from } => {
                v.typing.retain(|(n, _)| *n != from);
                v.typing.push((from, Instant::now()));
                Ok(())
            }
            RoomEvent::JoinRequest { from } => {
                v.requests.retain(|(n, _)| *n != from);
                v.requests.push((from, Instant::now()));
                Ok(())
            }
            RoomEvent::RequestCancelled { from } => {
                v.requests.retain(|(n, _)| *n != from);
                Ok(())
            }
            RoomEvent::MemberJoined { nick, fingerprint } => {
                let c = v.color(&nick, &me);
                let mut l = vec![
                    Span::styled("→ ", fg(OK)),
                    Span::styled(sanitize(&nick), bold(c)),
                    Span::styled(" joined", fg(DIM)),
                ];
                if let Some(fp) = fingerprint {
                    l.push(Span::styled("  ·  fingerprint ", fg(DIM)));
                    l.push(Span::styled(sanitize(&fp), fg(WARN)));
                }
                self.print_system(vec![Line::from(l)])
            }
            RoomEvent::Fingerprint { nick, fingerprint } => {
                let c = v.color(&nick, &me);
                let mut l = Vec::new();
                if nick == room.owner() && !room.is_owner() {
                    l.push(Span::styled("★ ", fg(WARN)));
                    l.push(Span::styled(sanitize(&nick), bold(c)));
                    l.push(Span::styled(" now owns the room", fg(DIM)));
                } else {
                    l.push(Span::styled("  ", fg(DIM)));
                    l.push(Span::styled(sanitize(&nick), bold(c)));
                }
                l.push(Span::styled("  ·  fingerprint ", fg(DIM)));
                l.push(Span::styled(sanitize(&fingerprint), fg(WARN)));
                self.print_system(vec![Line::from(l)])
            }
            RoomEvent::MemberLeft { nick } => {
                v.typing.retain(|(n, _)| *n != nick);
                let c = v.color(&nick, &me);
                self.print_system(vec![Line::from(vec![
                    Span::styled("← ", fg(BAD)),
                    Span::styled(sanitize(&nick), bold(c)),
                    Span::styled(" left", fg(DIM)),
                ])])
            }
            RoomEvent::OwnerChanged { owner } => {
                v.requests.clear();
                let line = if owner == me {
                    Line::from(vec![
                        Span::styled("★ ", fg(WARN)),
                        Span::styled("you now own the room", fg(FG)),
                    ])
                } else {
                    let c = v.color(&owner, &me);
                    Line::from(vec![
                        Span::styled("★ ", fg(WARN)),
                        Span::styled(sanitize(&owner), bold(c)),
                        Span::styled(" now owns the room", fg(DIM)),
                    ])
                };
                self.print_system(vec![line])
            }
            RoomEvent::Rekeyed { .. } => {
                if matches!(&v.notice, Some((t, _)) if t == "securing room...") {
                    v.notice = None;
                }
                Ok(())
            }
            RoomEvent::ConnectionLost => self.lost(),
        }
    }

    async fn on_lobby(&mut self, ev: Result<LobbyEvent, CoreError>) -> io::Result<()> {
        let Screen::Knock { room, .. } = &self.screen else {
            return Ok(());
        };
        let room = room.clone();
        match ev {
            Ok(LobbyEvent::Refused) => {
                let n = (format!("{} refused", sanitize(&room)), BAD);
                self.refresh(Some(n)).await
            }
            Ok(LobbyEvent::RoomGone) => {
                let n = (format!("{} is no longer available", sanitize(&room)), WARN);
                self.refresh(Some(n)).await
            }
            Ok(joined @ LobbyEvent::Joined { .. }) => {
                let Some(conn) = self.conn.take() else {
                    return self.back_to_nick(None).await;
                };
                self.screen = Screen::Knock {
                    room: room.clone(),
                    since: Instant::now(),
                };
                match conn.enter(joined).await {
                    Ok(r) => self.enter_room(r).await,
                    Err(_) => {
                        let e = format!("could not secure {}", sanitize(&room));
                        self.back_to_nick(Some(e)).await
                    }
                }
            }
            Err(_) => {
                let e = "connection to the server lost".to_owned();
                self.back_to_nick(Some(e)).await
            }
        }
    }

    fn on_tick(&mut self) {
        if let Screen::Room(v) = &mut self.screen {
            v.typing.retain(|(_, t)| t.elapsed() < TYPING_HIDE);
            v.requests
                .retain(|(_, t)| t.elapsed() < REQUEST_TTL + Duration::from_secs(5));
        }
    }
}
