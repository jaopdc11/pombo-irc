use std::io::Write;

use pombo_irc::{core, harden, ui};

/// Leaves raw mode and shows the cursor again.
fn restore_terminal() {
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableBracketedPaste,
        crossterm::cursor::Show
    );
    let _ = std::io::stdout().write_all(b"\r\n");
}

/// Blocks SIGTERM and SIGHUP in this thread (and so in every thread started after it) and
/// waits for them on a thread of their own, which restores the terminal and exits.
/// Must run before any other thread exists.
fn exit_on_term_signals() {
    // SAFETY: `sigset_t` is plain data; the libc calls get valid pointers to it.
    let set = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        set
    };
    // SAFETY: valid set pointer; a null old-set pointer is allowed.
    if unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut()) } != 0 {
        return;
    }
    std::thread::spawn(move || {
        let mut sig: libc::c_int = 0;
        // SAFETY: valid pointers to a local set and a local int.
        if unsafe { libc::sigwait(&set, &mut sig) } == 0 {
            restore_terminal();
            std::process::exit(128 + sig);
        }
    });
}

fn main() {
    harden::apply();
    exit_on_term_signals();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pombo-irc: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = runtime.block_on(ui::app::run(core::server_url())) {
        eprintln!("pombo-irc: {e}");
        std::process::exit(1);
    }
}
