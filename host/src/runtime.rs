//! The host runtime: one loop that ties everything together.
//!
//! Each tick it
//! 1. answers the media supervisor (credentials requests, media reports),
//! 2. handles the **local** controls: shortcuts, banner buttons, tray menu,
//! 3. runs the session manager (poll, consent, timers),
//! 4. updates the banner and tray, and reports dismissals and restores
//!    (metadata only) to the server.
//!
//! Remote input never reaches step 2: nothing in the media layer can hide,
//! restore, renew or change anything here.

use std::future::Future;
use std::time::Duration;

use rb_core::indicators::{IndicatorController, IndicatorIntent};
use rb_core::session::{HostApi, ManagerState, MediaReport, SessionManager};
use rb_core::traits::{Banner, Hotkeys, Notice, Tray, TrayAction};
use tokio::sync::mpsc;

use crate::supervisor::SupervisorEvent;

/// How often the loop runs.
pub const TICK: Duration = Duration::from_millis(250);

pub struct HostRuntime<A: HostApi> {
    manager: SessionManager<A>,
    indicators: IndicatorController,
    banner: Box<dyn Banner>,
    tray: Box<dyn Tray>,
    hotkeys: Box<dyn Hotkeys>,
    supervisor: mpsc::UnboundedReceiver<SupervisorEvent>,
    default_hide_minutes: u32,
    shortcut_collision: bool,
}

impl<A: HostApi> HostRuntime<A> {
    pub fn new(
        manager: SessionManager<A>,
        indicators: IndicatorController,
        banner: Box<dyn Banner>,
        tray: Box<dyn Tray>,
        hotkeys: Box<dyn Hotkeys>,
        supervisor: mpsc::UnboundedReceiver<SupervisorEvent>,
    ) -> Self {
        Self {
            manager,
            indicators,
            banner,
            tray,
            hotkeys,
            supervisor,
            default_hide_minutes: 30,
            shortcut_collision: false,
        }
    }

    pub fn manager(&self) -> &SessionManager<A> {
        &self.manager
    }

    pub fn manager_mut(&mut self) -> &mut SessionManager<A> {
        &mut self.manager
    }

    /// Register the shortcuts. If the OS refuses either one, warn the user
    /// and rely on the tray menu items, and tell the server (metadata only).
    pub async fn startup(&mut self) {
        let registration = self.hotkeys.register();
        let (restore_ok, disconnect_ok) = match registration {
            Ok(r) => (r.restore, r.emergency_disconnect),
            Err(e) => {
                log::warn!("shortcuts unavailable: {e}");
                (false, false)
            }
        };
        self.shortcut_collision = !(restore_ok && disconnect_ok);
        if self.shortcut_collision {
            let mut failed = Vec::new();
            if !restore_ok {
                failed.push(self.hotkeys.restore_label());
            }
            if !disconnect_ok {
                failed.push(self.hotkeys.disconnect_label());
            }
            self.manager.notify(Notice::ShortcutCollision {
                shortcut: failed.join(", "),
            });
        }
        self.send_report(None).await;
    }

    pub async fn tick(&mut self) {
        self.drain_supervisor().await;
        self.handle_local_controls().await;
        self.manager.tick().await;
        self.refresh_indicators().await;
    }

    /// Run until `shutdown` finishes, then end every session in an orderly way.
    pub async fn run_until<F: Future>(&mut self, shutdown: F) {
        tokio::pin!(shutdown);
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => self.tick().await,
                _ = &mut shutdown => break,
            }
        }
        let _ = self.manager.shutdown().await;
        self.refresh_indicators().await;
        self.hotkeys.unregister();
    }

    // ---- supervisor -----------------------------------------------------------------------

    async fn drain_supervisor(&mut self) {
        while let Ok(event) = self.supervisor.try_recv() {
            match event {
                SupervisorEvent::NeedCredentials { session_id, reply } => {
                    let result = self.manager.media_credentials(&session_id).await;
                    let _ = reply.send(result);
                }
                SupervisorEvent::Report { session_id, report } => {
                    self.manager.report_media(&session_id, report);
                }
                SupervisorEvent::CaptureFailed(reason) => {
                    for session in self.manager.sessions() {
                        self.manager
                            .report_media(&session.id, MediaReport::Failed(reason));
                    }
                }
            }
        }
    }

    // ---- local controls ---------------------------------------------------------------------

    async fn handle_local_controls(&mut self) {
        let mut disconnect = false;

        while let Some(event) = self.hotkeys.poll() {
            disconnect |=
                self.indicators.handle_hotkey(event) == IndicatorIntent::EmergencyDisconnect;
        }
        while let Some(action) = self.banner.poll_action() {
            match self.indicators.handle_banner_action(action) {
                Ok((outcome, intent)) => {
                    if let Some(outcome) = outcome
                        && outcome.show_first_time_notice
                    {
                        self.manager.notify(Notice::FirstHide {
                            restore_shortcut: self.hotkeys.restore_label(),
                        });
                    }
                    disconnect |= intent == IndicatorIntent::EmergencyDisconnect;
                }
                Err(e) => log::warn!("hide request refused: {e:?}"),
            }
        }
        while let Some(action) = self.tray.poll_action() {
            if action == TrayAction::ShareThisComputer {
                self.share_this_computer().await;
                continue;
            }
            disconnect |=
                self.indicators.handle_tray_action(action) == IndicatorIntent::EmergencyDisconnect;
        }

        // Disconnect last, so a restore and a disconnect in the same tick both happen.
        if disconnect {
            let errors = self.manager.emergency_disconnect().await;
            if !errors.is_empty() {
                log::warn!(
                    "emergency disconnect: the server was not told ({} errors); local media is stopped",
                    errors.len()
                );
            }
        }
    }

    /// Tray "Share this computer…": ask the server for a support code and
    /// show it to the person at the machine. Only the server decides whether
    /// the code may be used; the host just displays it.
    async fn share_this_computer(&mut self) {
        match self.manager.get_help().await {
            Ok(info) => {
                let minutes_left = self.manager.invite_seconds_left().unwrap_or(0).div_ceil(60);
                self.manager.notify(Notice::SupportCode {
                    code: info.code,
                    minutes_left,
                });
            }
            Err(e) => {
                log::warn!("support code unavailable: {e}");
                self.manager.notify(Notice::SupportCodeUnavailable {
                    reason: e.to_string(),
                });
            }
        }
    }

    // ---- indicators ---------------------------------------------------------------------------

    async fn refresh_indicators(&mut self) {
        let sessions = if self.manager.state() == ManagerState::Revoked {
            Vec::new()
        } else {
            self.manager.sessions()
        };
        self.indicators
            .sync(&sessions, self.banner.as_mut(), self.tray.as_mut());
        for event in self.indicators.take_events() {
            self.send_report(Some(&event)).await;
        }
    }

    async fn send_report(&mut self, event: Option<&rb_core::indicators::IndicatorEvent>) {
        if self.manager.state() == ManagerState::Revoked {
            return;
        }
        let request = IndicatorController::report(
            event,
            self.default_hide_minutes,
            &self.hotkeys.restore_label(),
            &self.hotkeys.disconnect_label(),
            self.shortcut_collision,
        );
        if let Err(e) = self.manager.banner_report(&request).await {
            log::warn!("banner report failed: {e}");
        }
    }
}
