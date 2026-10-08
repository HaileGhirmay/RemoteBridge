//! Plain data types shared by the platform traits and the core logic.

use serde::{Deserialize, Serialize};

/// The four separately opt-in permissions. View is implicit and always on.
/// Serialized as `{control, systemAudio, microphone, clipboard}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub control: bool,
    pub system_audio: bool,
    pub microphone: bool,
    pub clipboard: bool,
}

impl Permissions {
    /// View-only.
    pub const NONE: Self = Self {
        control: false,
        system_audio: false,
        microphone: false,
        clipboard: false,
    };

    /// True if every permission set in `self` is also set in `other`.
    pub fn is_subset_of(&self, other: &Self) -> bool {
        (!self.control || other.control)
            && (!self.system_audio || other.system_audio)
            && (!self.microphone || other.microphone)
            && (!self.clipboard || other.clipboard)
    }

    /// Permissions set in `self` that were not set in `previous`.
    pub fn added_over(&self, previous: &Self) -> Self {
        Self {
            control: self.control && !previous.control,
            system_audio: self.system_audio && !previous.system_audio,
            microphone: self.microphone && !previous.microphone,
            clipboard: self.clipboard && !previous.clipboard,
        }
    }

    pub fn any(&self) -> bool {
        self.control || self.system_audio || self.microphone || self.clipboard
    }
}

/// Public half of the device key, as JWK coordinates (base64url, 32 bytes each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKeyJwk {
    pub x: String,
    pub y: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureEncoding {
    /// IEEE P1363: `r || s`, 64 bytes. What the server expects.
    P1363,
    /// ASN.1 DER, as returned by OpenSSL and Security.framework. The protocol
    /// client converts it to P1363.
    Der,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSignature {
    pub encoding: SignatureEncoding,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub id: DisplayId,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameData {
    /// Tightly packed BGRA pixels, `width * height * 4` bytes.
    Bgra(Vec<u8>),
    /// Opaque platform texture handle (for example a D3D11 shared handle) that
    /// goes to the encoder without a CPU copy.
    Native(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Monotonic capture time in microseconds.
    pub timestamp_us: u64,
    pub data: FrameData,
}

/// Interleaved signed 16-bit PCM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioChunk {
    pub sample_rate: u32,
    pub channels: u16,
    pub timestamp_us: u64,
    pub samples: Vec<i16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW_ONLY: Permissions = Permissions::NONE;
    const CONTROL: Permissions = Permissions {
        control: true,
        ..Permissions::NONE
    };
    const CONTROL_AND_CLIPBOARD: Permissions = Permissions {
        control: true,
        clipboard: true,
        ..Permissions::NONE
    };

    #[test]
    fn default_is_view_only() {
        assert_eq!(Permissions::default(), VIEW_ONLY);
        assert!(!VIEW_ONLY.any());
    }

    #[test]
    fn subset_rules() {
        assert!(VIEW_ONLY.is_subset_of(&VIEW_ONLY));
        assert!(VIEW_ONLY.is_subset_of(&CONTROL));
        assert!(CONTROL.is_subset_of(&CONTROL_AND_CLIPBOARD));
        assert!(!CONTROL_AND_CLIPBOARD.is_subset_of(&CONTROL));
        assert!(!CONTROL.is_subset_of(&VIEW_ONLY));
    }

    #[test]
    fn added_over_lists_only_new_permissions() {
        let added = CONTROL_AND_CLIPBOARD.added_over(&CONTROL);
        assert_eq!(
            added,
            Permissions {
                clipboard: true,
                ..Permissions::NONE
            }
        );
        assert_eq!(CONTROL.added_over(&CONTROL_AND_CLIPBOARD), VIEW_ONLY);
    }
}
