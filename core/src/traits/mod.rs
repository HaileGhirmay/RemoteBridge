//! Platform interfaces. Core logic depends only on these.
//!
//! All traits are object-safe so the session manager can hold `Box<dyn …>`.

mod clock;
mod hotkeys;
mod input;
mod key_store;
mod media;
mod ui;

pub use clock::{Clock, SystemClock};
pub use hotkeys::{HotkeyEvent, HotkeyRegistration, Hotkeys};
pub use input::InputInjector;
pub use key_store::KeyStore;
pub use media::{AudioCapture, Capture, MicCapture};
pub use ui::{
    Banner, BannerAction, BannerContent, ConsentAnswer, ConsentPrompt, ConsentUi, HideChoice,
    Notice, PromptId, Tray, TrayAction, TrayModel,
};
