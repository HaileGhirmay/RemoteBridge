//! `rb-host`: the RemoteBridge host for Windows 11.
//!
//! ```text
//! rb-host enroll <8-digit code> [--name <name>]   register this PC (once)
//! rb-host run                                     stay online and serve sessions
//! ```
//!
//! Everything here is wiring. The rules (consent, permissions, timers, the
//! shortcuts) live in `rb-core` and `rb-host` and are tested there.

// `state` is only called on Windows; elsewhere the binary just says so.
#![cfg_attr(not(windows), allow(dead_code))]

mod state;

use std::process::ExitCode;

const HOST_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-windows");

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match platform::main(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rb-host: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;

    use rb_core::PlatformError;
    use rb_core::indicators::{IndicatorController, MemoryIndicatorStore};
    use rb_core::protocol::types::HostPlatform;
    use rb_core::protocol::{ClientConfig, HostClient, OsNonceSource, ReqwestTransport};
    use rb_core::session::{HostSettings, SessionManager};
    use rb_core::traits::{Capture, Clock, KeyStore, SystemClock};
    use rb_host::{
        HostRuntime, SharedClipboard, SharedInjector, SupervisorConfig, spawn_supervisor,
    };
    use rb_media::HostIdentity;
    use rb_media::opus_encoder::OpusAudioEncoder;
    use rb_media::video::OpenH264Encoder;
    use rb_platform_windows::{
        DxgiCapture, KeyScope, Microphone, SystemAudio, WindowsClipboard, WindowsConsent,
        WindowsHotkeys, WindowsInput, WindowsKeyStore, WindowsUi, primary_display,
    };

    use crate::{HOST_VERSION, state};

    /// Key name in the Windows key store. One key per PC.
    const KEY_NAME: &str = "RemoteBridge device key";

    pub fn main(args: &[String]) -> Result<(), String> {
        let path = state::default_path();
        match args {
            [cmd, code] if cmd == "enroll" => enroll(&path, code, &default_name()),
            [cmd, code, flag, name] if cmd == "enroll" && flag == "--name" => {
                enroll(&path, code, name)
            }
            [cmd] if cmd == "run" => run(&path),
            [cmd] if cmd == "invite" => invite(&path),
            _ => Err(usage()),
        }
    }

    fn usage() -> String {
        "usage: rb-host enroll <8-digit code> [--name <name>] | rb-host run | rb-host invite".into()
    }

    fn default_name() -> String {
        std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into())
    }

    fn keys() -> Arc<dyn KeyStore> {
        Arc::new(WindowsKeyStore::new(KEY_NAME, KeyScope::User))
    }

    fn client(device_id: Option<String>) -> Result<HostClient<ReqwestTransport>, String> {
        let transport = ReqwestTransport::new().map_err(|e| format!("network: {e:?}"))?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let client = HostClient::new(
            transport,
            ClientConfig::new(HOST_VERSION),
            keys(),
            clock,
            Arc::new(OsNonceSource),
        );
        Ok(match device_id {
            Some(id) => client.with_device_id(id),
            None => client,
        })
    }

    fn enroll(path: &std::path::Path, code: &str, name: &str) -> Result<(), String> {
        if let Some(id) = state::load(path).map_err(|e| e.to_string())? {
            return Err(format!(
                "this PC is already enrolled as {id}. Remove it on the website before enrolling again."
            ));
        }
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let enrolled = client(None)?
                .enroll(code, name, HostPlatform::Windows)
                .await
                .map_err(|e| format!("enrollment failed: {e}"))?;
            state::save(path, &enrolled.device_id).map_err(|e| e.to_string())?;
            println!("Enrolled device {}", enrolled.device_id);
            println!("Key fingerprint {}", enrolled.fingerprint);
            Ok(())
        })
    }

    /// A 12-digit support code for a viewer to enter on the website. Valid for
    /// 10 minutes and single use. The host keeps running separately; this only
    /// asks the server for a code.
    fn invite(path: &std::path::Path) -> Result<(), String> {
        let device_id = state::load(path)
            .map_err(|e| e.to_string())?
            .ok_or("this PC is not enrolled yet; run `rb-host enroll <code>` first")?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let response = client(Some(device_id))?
                .invite()
                .await
                .map_err(|e| format!("could not create a support code: {e}"))?;
            println!("Support code: {}", response.code);
            println!("Valid for 10 minutes, single use.");
            Ok(())
        })
    }

    fn run(path: &std::path::Path) -> Result<(), String> {
        let device_id = state::load(path)
            .map_err(|e| e.to_string())?
            .ok_or("this PC is not enrolled yet; run `rb-host enroll <code>` first")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(serve(device_id))
    }

    fn platform_error(what: &str, e: PlatformError) -> String {
        format!("{what}: {e}")
    }

    async fn serve(device_id: String) -> Result<(), String> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::new());
        let identity = HostIdentity::generate().map_err(|e| format!("media identity: {e}"))?;

        // Adapters. The UI owns its own thread; the rest are plain objects.
        let ui = WindowsUi::start().map_err(|e| platform_error("user interface", e))?;
        let capture = DxgiCapture::new().map_err(|e| platform_error("screen capture", e))?;
        let display = capture
            .displays()
            .map_err(|e| platform_error("displays", e))?
            .first()
            .map(|d| d.id)
            .ok_or("no display found")?;
        let input = SharedInjector::new(Box::new(WindowsInput::new(primary_display())));
        let clipboard = SharedClipboard::new(Box::new(WindowsClipboard::new()));

        let (effects, events, _supervisor) = spawn_supervisor(SupervisorConfig {
            identity: identity.clone(),
            input,
            clipboard,
            capture: Box::new(capture),
            system_audio: Some(Box::new(SystemAudio::new())),
            microphone: Some(Box::new(Microphone::new())),
            display,
            udp_ip: "0.0.0.0".into(),
            video_encoder: Arc::new(|| Box::new(OpenH264Encoder::default())),
            audio_encoder: Arc::new(|| Box::new(OpusAudioEncoder::new())),
            credential_attempts: 3,
            credential_retry: std::time::Duration::from_secs(2),
        });

        let manager = SessionManager::new(
            client(Some(device_id))?,
            clock.clone(),
            Box::new(WindowsConsent::new()),
            Box::new(effects),
            keys(),
            HostSettings {
                unattended_opt_in: false,
                host_fingerprint: Some(identity.fingerprint().to_owned()),
            },
        );
        let indicators = IndicatorController::new(Box::new(MemoryIndicatorStore::new()), clock);
        let mut runtime = HostRuntime::new(
            manager,
            indicators,
            Box::new(ui.banner()),
            Box::new(ui.tray()),
            Box::new(WindowsHotkeys::new()),
            events,
        );

        log::info!("rb-host {HOST_VERSION} running; press Ctrl+C to stop");
        runtime.startup().await;
        runtime
            .run_until(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await;
        log::info!("stopped");
        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn main(_args: &[String]) -> Result<(), String> {
        Err("rb-host runs on Windows 11 only".into())
    }
}
