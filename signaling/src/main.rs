//! `rb-signaling-server`
//!
//! Environment:
//! * `SIGNALING_JWT_SECRET` (required, at least 32 bytes): the HS256 key shared with the website.
//! * `PORT` (default 8080) and `BIND` (default 0.0.0.0).
//! * `RUST_LOG` (default `info`). Message contents and tokens are never logged.
//!
//! TLS is terminated in front of this process (Fly.io, a reverse proxy, a load balancer).

use rb_signaling::{Config, serve};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let secret = std::env::var("SIGNALING_JWT_SECRET").unwrap_or_default();
    if secret.len() < 32 {
        eprintln!("SIGNALING_JWT_SECRET must be set and at least 32 bytes long");
        std::process::exit(2);
    }
    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0".into());
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let listener = match tokio::net::TcpListener::bind(format!("{bind}:{port}")).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot listen on {bind}:{port}: {e}");
            std::process::exit(1);
        }
    };
    tracing::info!(%bind, %port, "signaling server listening");

    tokio::select! {
        result = serve(listener, Config::new(secret.into_bytes())) => {
            if let Err(e) = result {
                eprintln!("server error: {e}");
                std::process::exit(1);
            }
        }
        _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
    }
}
