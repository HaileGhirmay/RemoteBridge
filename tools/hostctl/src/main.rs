//! `rb-hostctl`: developer CLI for RA-HOST-V1.
//!
//! Build and run with the dev-only software key:
//! `cargo run -p rb-hostctl --features dev-key -- poll`

mod file_key;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use rb_core::Permissions;
use rb_core::protocol::types::{
    DecideRequest, Decision, EndReason, HostPlatform, SessionAction, SessionRequest,
};
use rb_core::protocol::{
    ClientConfig, HostClient, HostError, NonceSource, OsNonceSource, ReqwestTransport,
};
use rb_core::traits::{Clock, KeyStore, SystemClock};
use serde::Serialize;

use file_key::FileKeyStore;

const HOST_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-hostctl");

#[derive(Parser)]
#[command(
    name = "rb-hostctl",
    about = "Exercise RA-HOST-V1 with a dev-only software key"
)]
struct Cli {
    /// File holding the dev key and device id. Never commit it.
    #[arg(
        long,
        env = "RB_HOSTCTL_STATE",
        default_value = "rb-hostctl.devkey",
        global = true
    )]
    state: PathBuf,

    /// Control-plane base URL.
    #[arg(long, env = "RB_HOSTCTL_BASE_URL", global = true)]
    base_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enroll this machine with an 8-digit code from the website's Devices page.
    Enroll {
        code: String,
        /// Device name, 1-60 characters.
        #[arg(long)]
        name: Option<String>,
    },
    /// Heartbeat: list pending and live sessions.
    Poll,
    /// Create a 12-digit support code (valid 10 minutes, single use).
    Invite,
    /// Approve or deny a pending session. Approve is view-only unless --grant says more.
    Decide {
        session_id: String,
        decision: DecisionArg,
        /// Extra permissions to grant (must be a subset of what was requested).
        #[arg(long, value_delimiter = ',')]
        grant: Vec<GrantArg>,
        /// Consent window, 30-480 minutes (attended sessions).
        #[arg(long)]
        minutes: Option<u32>,
        /// DTLS fingerprint, `sha-256 AB:CD:...`.
        #[arg(long)]
        host_fingerprint: Option<String>,
    },
    /// End a session.
    End {
        session_id: String,
        #[arg(long, value_enum, default_value = "host-stop")]
        reason: EndReasonArg,
    },
    /// Send one signed poll twice with the same nonce; the second must be refused with 401.
    ReplayTest,
}

#[derive(Clone, Copy, ValueEnum)]
enum DecisionArg {
    Approve,
    Deny,
}

#[derive(Clone, Copy, ValueEnum)]
enum GrantArg {
    Control,
    SystemAudio,
    Microphone,
    Clipboard,
}

#[derive(Clone, Copy, ValueEnum)]
enum EndReasonArg {
    HostStop,
    EmergencyDisconnect,
    LocalUiLost,
    HostShutdown,
}

/// Always the same nonce. Only `replay-test` uses it, to prove the server
/// refuses a repeat; the real client never reuses a nonce.
struct FixedNonce(String);

impl NonceSource for FixedNonce {
    fn next(&self) -> String {
        self.0.clone()
    }
}

enum CliError {
    Host(HostError),
    Msg(String),
}

impl From<HostError> for CliError {
    fn from(e: HostError) -> Self {
        Self::Host(e)
    }
}

impl From<rb_core::PlatformError> for CliError {
    fn from(e: rb_core::PlatformError) -> Self {
        Self::Host(HostError::Key(e))
    }
}

fn msg(s: impl Into<String>) -> CliError {
    CliError::Msg(s.into())
}

fn print_json<T: Serialize>(value: &T) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value).map_err(|e| msg(e.to_string()))?;
    println!("{text}");
    Ok(())
}

fn default_name() -> String {
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "dev".into());
    format!("{host} (hostctl)").chars().take(60).collect()
}

fn platform() -> Result<HostPlatform, CliError> {
    if cfg!(windows) {
        Ok(HostPlatform::Windows)
    } else if cfg!(target_os = "macos") {
        Ok(HostPlatform::Macos)
    } else {
        Err(msg("RA-HOST-V1 hosts are Windows or macOS only"))
    }
}

fn build_client(
    cli: &Cli,
    store: &Arc<FileKeyStore>,
    nonces: Arc<dyn NonceSource>,
) -> Result<HostClient<ReqwestTransport>, CliError> {
    let transport = ReqwestTransport::new().map_err(|e| msg(format!("{e:?}")))?;
    let mut config = ClientConfig::new(HOST_VERSION);
    if let Some(url) = &cli.base_url {
        config.base_url = url.clone();
    }
    let keys: Arc<dyn KeyStore> = store.clone();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
    Ok(HostClient::new(transport, config, keys, clock, nonces))
}

async fn run(cli: &Cli, store: &Arc<FileKeyStore>) -> Result<(), CliError> {
    let client = build_client(cli, store, Arc::new(OsNonceSource))?;

    if let Command::Enroll { code, name } = &cli.command {
        if store.device_id()?.is_some() {
            return Err(msg(format!(
                "already enrolled (state: {}). Revoke the device on the website, then delete the state file.",
                store.path().display()
            )));
        }
        let name = name.clone().unwrap_or_else(default_name);
        let enrolled = client.enroll(code, &name, platform()?).await?;
        store.set_device_id(&enrolled.device_id)?;
        println!("Enrolled device {}", enrolled.device_id);
        println!("Key fingerprint {}", enrolled.fingerprint);
        return Ok(());
    }

    let device_id = store
        .device_id()?
        .ok_or_else(|| msg("not enrolled; run `rb-hostctl enroll <code>` first"))?;

    if matches!(cli.command, Command::ReplayTest) {
        return replay_test(cli, store, &device_id).await;
    }
    let client = client.with_device_id(device_id);

    match &cli.command {
        Command::Enroll { .. } | Command::ReplayTest => unreachable!("handled above"),
        Command::Poll => print_json(&client.poll().await?),
        Command::Invite => print_json(&client.invite().await?),
        Command::Decide {
            session_id,
            decision,
            grant,
            minutes,
            host_fingerprint,
        } => {
            let approve = matches!(decision, DecisionArg::Approve);
            let request = DecideRequest {
                session_id: session_id.clone(),
                decision: if approve {
                    Decision::Approve
                } else {
                    Decision::Deny
                },
                // View-only unless the operator names more.
                grant: approve.then(|| grant_set(grant)),
                approved_minutes: approve.then_some(*minutes).flatten(),
                host_fingerprint: host_fingerprint.clone(),
            };
            print_json(&client.decide(&request).await?)
        }
        Command::End { session_id, reason } => {
            let request = SessionRequest {
                session_id: session_id.clone(),
                action: SessionAction::End {
                    reason: match reason {
                        EndReasonArg::HostStop => EndReason::HostStop,
                        EndReasonArg::EmergencyDisconnect => EndReason::EmergencyDisconnect,
                        EndReasonArg::LocalUiLost => EndReason::LocalUiLost,
                        EndReasonArg::HostShutdown => EndReason::HostShutdown,
                    },
                },
            };
            print_json(&client.session(&request).await?)
        }
    }
}

fn grant_set(grant: &[GrantArg]) -> Permissions {
    let mut p = Permissions::NONE;
    for g in grant {
        match g {
            GrantArg::Control => p.control = true,
            GrantArg::SystemAudio => p.system_audio = true,
            GrantArg::Microphone => p.microphone = true,
            GrantArg::Clipboard => p.clipboard = true,
        }
    }
    p
}

async fn replay_test(
    cli: &Cli,
    store: &Arc<FileKeyStore>,
    device_id: &str,
) -> Result<(), CliError> {
    let nonce = OsNonceSource.next();
    let client = build_client(cli, store, Arc::new(FixedNonce(nonce)))?.with_device_id(device_id);
    client.poll().await?;
    println!("first poll with the nonce: accepted");
    match client.poll().await {
        Err(HostError::Api {
            status: 401,
            message,
            ..
        }) => {
            println!("second poll with the same nonce: refused (401: {message})");
            Ok(())
        }
        Ok(_) => Err(msg("FAIL: the server accepted a replayed nonce")),
        Err(e) => Err(e.into()),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let store = Arc::new(FileKeyStore::new(&cli.state));
    match run(&cli, &store).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Host(HostError::DeviceRevoked)) => {
            // Revoked on the website: forget everything, like the real host must.
            let wiped = store.wipe();
            eprintln!("This device was removed from your account. Local key deleted.");
            if let Err(e) = wiped {
                eprintln!("warning: could not delete {}: {e}", store.path().display());
            }
            ExitCode::from(3)
        }
        Err(CliError::Host(e)) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
        Err(CliError::Msg(m)) => {
            eprintln!("error: {m}");
            ExitCode::FAILURE
        }
    }
}
