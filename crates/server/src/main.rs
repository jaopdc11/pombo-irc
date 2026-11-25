use std::process::ExitCode;

use server::{serve, ServerConfig};

#[tokio::main]
async fn main() -> ExitCode {
    // POMBO_BIND wins; otherwise PORT (set by process managers) on loopback.
    let bind = std::env::var("POMBO_BIND")
        .ok()
        .filter(|b| !b.is_empty())
        .or_else(|| {
            std::env::var("PORT")
                .ok()
                .filter(|p| p.parse::<u16>().is_ok())
                .map(|p| format!("127.0.0.1:{p}"))
        })
        .unwrap_or_else(|| "127.0.0.1:7466".to_owned());
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("pombo-irc-server: cannot bind {bind}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let addr = listener.local_addr().map_or(bind, |a| a.to_string());
    println!("pombo-irc-server listening on {addr}");
    match serve(listener, ServerConfig::default()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pombo-irc-server: listener failed: {e}");
            ExitCode::FAILURE
        }
    }
}
