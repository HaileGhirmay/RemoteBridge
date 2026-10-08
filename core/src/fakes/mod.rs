//! Fake platform implementations for tests.
//!
//! Every fake is `Clone` and shares its state between clones, so a test can
//! hand a clone to the code under test (as `Box<dyn Trait>`) and keep another
//! to script inputs and inspect what happened.

mod clipboard;
mod clock;
mod effects;
mod host_api;
mod hotkeys;
mod input;
mod key_store;
mod media;
mod trace;
mod ui;

pub use clipboard::FakeClipboard;
pub use clock::FakeClock;
pub use effects::FakeEffects;
pub use host_api::{FakeHostApi, Recorded, test_session};
pub use hotkeys::FakeHotkeys;
pub use input::{FakeInputInjector, InputCall};
pub use key_store::FakeKeyStore;
pub use media::{FakeAudioCapture, FakeCapture, FakeMicCapture};
pub use trace::Trace;
pub use ui::{FakeBanner, FakeConsentUi, FakeTray};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::*;

    /// Every fake must be usable as the trait object the session manager holds.
    #[test]
    fn fakes_work_as_trait_objects() {
        let _: Box<dyn Clock> = Box::new(FakeClock::new(0));
        let _: Box<dyn KeyStore> = Box::new(FakeKeyStore::new());
        let _: Box<dyn Capture> = Box::new(FakeCapture::new());
        let _: Box<dyn AudioCapture> = Box::new(FakeAudioCapture::new());
        let _: Box<dyn MicCapture> = Box::new(FakeMicCapture::new());
        let _: Box<dyn InputInjector> = Box::new(FakeInputInjector::new());
        let _: Box<dyn Tray> = Box::new(FakeTray::new());
        let _: Box<dyn Banner> = Box::new(FakeBanner::new());
        let _: Box<dyn Hotkeys> = Box::new(FakeHotkeys::new());
        let _: Box<dyn ConsentUi> = Box::new(FakeConsentUi::new());
        let _: Box<dyn crate::session::SessionEffects> = Box::new(FakeEffects::new());
    }
}
