//! Audio through WASAPI.
//!
//! * System audio is a **loopback** capture of the default render device, so
//!   it hears what the speakers play (Windows' own loopback, no driver).
//! * The microphone is a normal capture from the default capture device.
//!
//! Both are polled in shared mode. Samples come out as interleaved signed
//! 16-bit PCM at the device's own rate and channel count (usually 48 kHz,
//! stereo). Turning that into Opus is the encoder's job, and it may need a
//! resampler first.
//!
//! Threading: COM objects are only used on the thread that is capturing (the
//! supervisor's capture thread). `stop()` only sets a flag; the capture
//! thread notices it within one poll and stops the client, so a stop completes
//! before the supervisor's join returns.

use std::time::{Duration, Instant};

use rb_core::traits::{AudioCapture, MicCapture};
use rb_core::types::AudioChunk;
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eCapture, eConsole, eRender,
};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// 100 ms buffer, in 100-nanosecond units.
const BUFFER_HNS: i64 = 1_000_000;
const POLL: Duration = Duration::from_millis(5);

fn backend(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Backend(format!("{what}: 0x{:08x}", e.code().0 as u32))
}

/// How a mix format's samples are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Float32,
    Pcm16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixFormat {
    pub kind: SampleKind,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Read a mix format. Only 32-bit float and 16-bit PCM are supported.
///
/// # Safety
/// `format` must point to a valid `WAVEFORMATEX`, and to a
/// `WAVEFORMATEXTENSIBLE` when its tag is `WAVE_FORMAT_EXTENSIBLE`.
pub unsafe fn parse_format(format: *const WAVEFORMATEX) -> PlatformResult<MixFormat> {
    // SAFETY: the caller guarantees the pointer is valid. Fields of the packed
    // structures are read unaligned.
    unsafe {
        let base = std::ptr::read_unaligned(format);
        let tag = base.wFormatTag;
        let bits = base.wBitsPerSample;
        let kind = if tag == WAVE_FORMAT_IEEE_FLOAT as u16 {
            SampleKind::Float32
        } else if tag == WAVE_FORMAT_PCM {
            SampleKind::Pcm16
        } else if tag == WAVE_FORMAT_EXTENSIBLE {
            let ext = std::ptr::read_unaligned(format as *const WAVEFORMATEXTENSIBLE);
            let sub = std::ptr::read_unaligned(std::ptr::addr_of!(ext.SubFormat));
            if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                SampleKind::Float32
            } else {
                // Extensible PCM is 16-bit here; anything else is unsupported below.
                SampleKind::Pcm16
            }
        } else {
            return Err(PlatformError::Unsupported("unknown audio mix format"));
        };
        match (kind, bits) {
            (SampleKind::Float32, 32) | (SampleKind::Pcm16, 16) => {}
            _ => {
                return Err(PlatformError::Unsupported(
                    "audio sample format (need 32-bit float or 16-bit PCM)",
                ));
            }
        }
        Ok(MixFormat {
            kind,
            sample_rate: base.nSamplesPerSec,
            channels: base.nChannels,
        })
    }
}

/// Float samples in -1..=1 to 16-bit PCM.
pub fn float_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

/// One WASAPI capture session, started on the default device of a flow.
struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: MixFormat,
}

/// Shared implementation for loopback and microphone capture.
struct Device {
    loopback: bool,
    flow: windows::Win32::Media::Audio::EDataFlow,
    stream: Option<Stream>,
    stop_requested: bool,
    started_at: Option<Instant>,
}

// SAFETY: the COM objects are only used by one thread at a time: the owner
// (supervisor) holds a mutex around every call, and COM is initialised in
// the multithreaded apartment on every thread that makes a call.
unsafe impl Send for Device {}

fn init_com() {
    // S_FALSE (already initialised) and RPC_E_CHANGED_MODE are both fine here.
    // SAFETY: standard COM initialisation for the current thread.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

impl Device {
    fn new(loopback: bool, flow: windows::Win32::Media::Audio::EDataFlow) -> Self {
        Self {
            loopback,
            flow,
            stream: None,
            stop_requested: false,
            started_at: None,
        }
    }

    fn open(&self) -> PlatformResult<Stream> {
        init_com();
        // SAFETY: standard WASAPI setup; each object is released by windows-rs on drop.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| backend("device enumerator", e))?;
            let device: IMMDevice = enumerator
                .GetDefaultAudioEndpoint(self.flow, eConsole)
                .map_err(|_| PlatformError::Unsupported("no default audio device"))?;
            let client: IAudioClient = device
                .Activate(CLSCTX_ALL, None)
                .map_err(|e| backend("activate audio client", e))?;
            let mix = client
                .GetMixFormat()
                .map_err(|e| backend("GetMixFormat", e))?;
            let format = parse_format(mix);
            let format = match format {
                Ok(f) => f,
                Err(e) => {
                    CoTaskMemFree(Some(mix as *const _));
                    return Err(e);
                }
            };
            let flags = if self.loopback {
                AUDCLNT_STREAMFLAGS_LOOPBACK
            } else {
                0
            };
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, BUFFER_HNS, 0, mix, None);
            CoTaskMemFree(Some(mix as *const _));
            init.map_err(|e| backend("Initialize", e))?;
            let capture: IAudioCaptureClient = client
                .GetService()
                .map_err(|e| backend("capture service", e))?;
            client.Start().map_err(|e| backend("Start", e))?;
            Ok(Stream {
                client,
                capture,
                format,
            })
        }
    }

    fn start(&mut self) -> PlatformResult<()> {
        if self.stream.is_some() {
            return Ok(());
        }
        self.stream = Some(self.open()?);
        self.stop_requested = false;
        self.started_at = Some(Instant::now());
        Ok(())
    }

    fn next_chunk(&mut self, timeout: Duration) -> PlatformResult<Option<AudioChunk>> {
        if self.stop_requested {
            self.close();
            return Err(PlatformError::NotStarted);
        }
        let Some(stream) = self.stream.as_ref() else {
            return Err(PlatformError::NotStarted);
        };
        let deadline = Instant::now() + timeout;
        let timestamp_us = self.started_at.map_or(0, |t| {
            u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
        });
        loop {
            // SAFETY: the capture client belongs to a started stream on this thread.
            let packet = unsafe { stream.capture.GetNextPacketSize() }
                .map_err(|e| backend("GetNextPacketSize", e))?;
            if packet > 0 {
                return self.read_packet(timestamp_us).map(Some);
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
    }

    fn read_packet(&mut self, timestamp_us: u64) -> PlatformResult<AudioChunk> {
        let stream = self.stream.as_ref().ok_or(PlatformError::NotStarted)?;
        let channels = stream.format.channels.max(1) as usize;
        // SAFETY: GetBuffer hands out `frames` frames that must be released with ReleaseBuffer.
        unsafe {
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            stream
                .capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .map_err(|e| backend("GetBuffer", e))?;
            let count = frames as usize * channels;
            let mut samples = vec![0i16; count];
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 == 0 && !data.is_null() {
                match stream.format.kind {
                    SampleKind::Float32 => {
                        let src = std::slice::from_raw_parts(data as *const f32, count);
                        for (dst, v) in samples.iter_mut().zip(src) {
                            *dst = float_to_i16(*v);
                        }
                    }
                    SampleKind::Pcm16 => {
                        let src = std::slice::from_raw_parts(data as *const i16, count);
                        samples.copy_from_slice(src);
                    }
                }
            }
            stream
                .capture
                .ReleaseBuffer(frames)
                .map_err(|e| backend("ReleaseBuffer", e))?;
            Ok(AudioChunk {
                sample_rate: stream.format.sample_rate,
                channels: stream.format.channels,
                timestamp_us,
                samples,
            })
        }
    }

    /// Ask the capture thread to stop. The next `next_chunk` call stops the client.
    fn stop(&mut self) {
        self.stop_requested = true;
    }

    fn close(&mut self) {
        if let Some(stream) = self.stream.take() {
            // SAFETY: stopping the client we started, on the thread that uses it.
            unsafe {
                let _ = stream.client.Stop();
            }
        }
    }

    fn is_running(&self) -> bool {
        self.stream.is_some() && !self.stop_requested
    }
}

/// What the machine plays: a loopback of the default output device.
pub struct SystemAudio(Device);

impl SystemAudio {
    pub fn new() -> Self {
        Self(Device::new(true, eRender))
    }
}

impl Default for SystemAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioCapture for SystemAudio {
    fn start(&mut self) -> PlatformResult<()> {
        self.0.start()
    }

    fn next_chunk(&mut self, timeout: Duration) -> PlatformResult<Option<AudioChunk>> {
        self.0.next_chunk(timeout)
    }

    fn is_running(&self) -> bool {
        self.0.is_running()
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

/// The default microphone.
pub struct Microphone(Device);

impl Microphone {
    pub fn new() -> Self {
        Self(Device::new(false, eCapture))
    }
}

impl Default for Microphone {
    fn default() -> Self {
        Self::new()
    }
}

impl MicCapture for Microphone {
    fn start(&mut self) -> PlatformResult<()> {
        self.0.start()
    }

    fn next_chunk(&mut self, timeout: Duration) -> PlatformResult<Option<AudioChunk>> {
        self.0.next_chunk(timeout)
    }

    fn is_running(&self) -> bool {
        self.0.is_running()
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::Audio::WAVEFORMATEX;

    fn plain(tag: u16, channels: u16, rate: u32, bits: u16) -> WAVEFORMATEX {
        let block = channels * bits / 8;
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: 0,
        }
    }

    #[test]
    fn float_stereo_48k_is_recognised() {
        let f = plain(WAVE_FORMAT_IEEE_FLOAT as u16, 2, 48_000, 32);
        let m = unsafe { parse_format(&f) }.unwrap();
        assert_eq!(
            m,
            MixFormat {
                kind: SampleKind::Float32,
                sample_rate: 48_000,
                channels: 2
            }
        );
    }

    #[test]
    fn pcm16_mono_44k_is_recognised() {
        let f = plain(WAVE_FORMAT_PCM, 1, 44_100, 16);
        let m = unsafe { parse_format(&f) }.unwrap();
        assert_eq!(m.kind, SampleKind::Pcm16);
        assert_eq!(m.channels, 1);
    }

    #[test]
    fn unsupported_formats_are_refused_not_misread() {
        // 24-bit PCM, 8-bit PCM and an unknown tag.
        for f in [
            plain(WAVE_FORMAT_PCM, 2, 48_000, 24),
            plain(WAVE_FORMAT_PCM, 2, 48_000, 8),
            plain(0x1234, 2, 48_000, 16),
        ] {
            assert!(unsafe { parse_format(&f) }.is_err());
        }
    }

    #[test]
    fn float_conversion_clamps_and_rounds() {
        assert_eq!(float_to_i16(0.0), 0);
        assert_eq!(float_to_i16(1.0), 32767);
        assert_eq!(float_to_i16(-1.0), -32767);
        assert_eq!(float_to_i16(3.0), 32767, "overs are clamped");
        assert_eq!(float_to_i16(-3.0), -32767);
        assert_eq!(float_to_i16(0.5), 16384);
    }

    #[test]
    fn a_device_that_was_never_started_reports_not_started() {
        let mut dev = Device::new(true, eRender);
        assert!(!dev.is_running());
        assert!(matches!(
            dev.next_chunk(Duration::ZERO),
            Err(PlatformError::NotStarted)
        ));
    }

    /// Captures a second of the default output and checks that chunks arrive.
    /// Run by hand: `cargo test -p rb-platform-windows system_audio_real -- --ignored --nocapture`
    #[test]
    #[ignore = "opens the real audio devices"]
    fn system_audio_real() {
        let mut audio = SystemAudio::new();
        audio
            .start()
            .expect("loopback starts on the default output");
        let started = Instant::now();
        let mut chunks = 0;
        let mut samples = 0usize;
        let mut format = None;
        while started.elapsed() < Duration::from_secs(1) {
            if let Some(chunk) = audio.next_chunk(Duration::from_millis(100)).unwrap() {
                chunks += 1;
                samples += chunk.samples.len();
                format = Some((chunk.sample_rate, chunk.channels));
            }
        }
        audio.stop();
        eprintln!("format {format:?}, chunks {chunks}, samples {samples}");
        assert!(chunks > 0, "no audio chunks in a second");
        let _ = audio.next_chunk(Duration::ZERO);
        assert!(!audio.is_running());
    }
}
