//! Banner and tray indicator logic.
//!
//! [`timer`] is a port of the website's tested reference (`helpers/bannerTimer`):
//! pure functions deciding whether a local dismissal is still valid.
//! [`controller`] adds the host rules on top: attended sessions may hide both
//! the banner and the tray, unattended sessions always keep the tray, only
//! local input can hide or restore, and every dismissal and restore is
//! reported (metadata only) to `host/banner_report`.
//!
//! Hiding an indicator never touches session authorization: nothing in here
//! reads or writes consent deadlines.

pub mod controller;
pub mod gate;
pub mod timer;

pub use controller::{
    HideError, HideOutcome, IndicatorController, IndicatorEvent, IndicatorIntent, IndicatorStore,
    MemoryIndicatorStore, ReportReason,
};
pub use gate::{HotkeyGate, Key, KeyEvent};
pub use timer::{
    Context, DURATIONS, DismissError, Dismissal, Evaluation, RestoreReason, Scope, dismiss,
    dismiss_all, evaluate, perms_increased, persist, restore,
};
