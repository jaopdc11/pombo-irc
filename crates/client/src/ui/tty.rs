//! Terminal input and the ratatui backend, arranged so a cursor-position query never starves.
//!
//! An inline viewport asks the terminal where the cursor is (`ESC [ 6 n`) whenever the
//! viewport is created or the screen is resized, and crossterm reads the reply from the same
//! global event reader that keyboard input goes through. crossterm's `EventStream` parks a
//! thread in a blocking read that holds that reader's lock until a *key* arrives, so the reply
//! is never handed over and the query fails after two seconds. Input is therefore read here by
//! a thread that polls in short slices, releasing the lock between them, and a query that still
//! fails falls back to the bottom row instead of ending the program.
use std::io::{self, Stdout};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tokio::sync::mpsc;

/// Longest time the input thread holds crossterm's event reader in one go.
const SLICE: Duration = Duration::from_millis(40);

/// Starts the input thread. It ends once the receiver is dropped or the terminal fails.
pub fn input() -> mpsc::Receiver<io::Result<Event>> {
    let (tx, rx) = mpsc::channel(64);
    thread::spawn(move || {
        while !tx.is_closed() {
            let next = match event::poll(SLICE) {
                Ok(true) => event::read(),
                Ok(false) => continue,
                Err(e) => Err(e),
            };
            let failed = next.is_err();
            if tx.blocking_send(next).is_err() || failed {
                break;
            }
            // Leave a gap so a waiting cursor query takes the reader before the next slice.
            thread::yield_now();
        }
    });
    rx
}

/// `CrosstermBackend` whose cursor query is retried once and then falls back to the bottom row.
pub struct Tty(CrosstermBackend<Stdout>);

impl Tty {
    pub fn stdout() -> Self {
        Tty(CrosstermBackend::new(io::stdout()))
    }
}

impl Backend for Tty {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        self.0.append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.0.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.0.show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        if let Ok(p) = self.0.get_cursor_position() {
            return Ok(p);
        }
        if let Ok(p) = self.0.get_cursor_position() {
            return Ok(p);
        }
        let size = self.0.size()?;
        Ok(Position::new(0, size.height.saturating_sub(1)))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.0.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.0.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.0.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.0.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.0.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        Backend::flush(&mut self.0)
    }
}
