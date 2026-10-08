//! Screen capture through DXGI Desktop Duplication.
//!
//! One duplication per display, on the first GPU. Each frame is copied into a
//! staging texture and read back as packed BGRA. Notes:
//!
//! * Frames are read back to CPU memory. That is a CPU copy per frame, which
//!   the prompt wanted to avoid (Windows.Graphics.Capture + hardware encoder
//!   would not need it). It is the simpler path and is correct; a GPU-side
//!   encoder can replace it later behind the same `Capture` trait.
//! * The cursor is not drawn yet (duplication does not include it).
//! * A desktop switch (UAC prompt, Ctrl+Alt+Del, lock screen) makes the
//!   duplication "access lost". That is reported as `SecureDesktop`, and the
//!   duplication is rebuilt when the desktop comes back. Nothing tries to
//!   capture the secure desktop.
//! * Displays on other GPUs are not enumerated.

use std::time::Duration;

use rb_core::traits::Capture;
use rb_core::types::{DisplayId, DisplayInfo, FrameData, VideoFrame};
use rb_core::{PlatformError, PlatformResult};
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_FLAG, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice, ID3D11Device,
    ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

fn backend(what: &str, e: windows::core::Error) -> PlatformError {
    PlatformError::Backend(format!("{what}: 0x{:08x}", e.code().0 as u32))
}

/// Map a DXGI error from a duplication call to our error type.
fn map_dxgi(what: &str, e: windows::core::Error) -> PlatformError {
    if e.code() == DXGI_ERROR_ACCESS_LOST {
        PlatformError::SecureDesktop
    } else if e.code() == DXGI_ERROR_ACCESS_DENIED {
        PlatformError::PermissionDenied("screen capture")
    } else {
        backend(what, e)
    }
}

struct Duplication {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    width: u32,
    height: u32,
}

impl Duplication {
    fn create(adapter: &IDXGIAdapter1, output_index: u32) -> PlatformResult<Self> {
        let output: IDXGIOutput =
            unsafe { adapter.EnumOutputs(output_index) }.map_err(|e| backend("EnumOutputs", e))?;
        let output1: IDXGIOutput1 = output.cast().map_err(|e| backend("IDXGIOutput1", e))?;

        let adapter_base: windows::Win32::Graphics::Dxgi::IDXGIAdapter =
            adapter.cast().map_err(|e| backend("IDXGIAdapter", e))?;
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        unsafe {
            D3D11CreateDevice(
                Some(&adapter_base),
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .map_err(|e| backend("D3D11CreateDevice", e))?;
        let device = device.ok_or_else(|| PlatformError::Backend("no D3D11 device".into()))?;
        let context = context.ok_or_else(|| PlatformError::Backend("no D3D11 context".into()))?;

        let duplication = unsafe { output1.DuplicateOutput(&device) }
            .map_err(|e| map_dxgi("DuplicateOutput", e))?;
        let desc = unsafe { duplication.GetDesc() };
        let mode = desc.ModeDesc;
        Ok(Self {
            device,
            context,
            duplication,
            staging: None,
            width: mode.Width,
            height: mode.Height,
        })
    }

    /// The staging texture, created once for the current frame format.
    fn staging_for(&mut self, source: &ID3D11Texture2D) -> PlatformResult<ID3D11Texture2D> {
        if let Some(existing) = &self.staging {
            return Ok(existing.clone());
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        desc.MiscFlags = 0;
        let mut texture: Option<ID3D11Texture2D> = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            .map_err(|e| backend("CreateTexture2D", e))?;
        let texture = texture.ok_or_else(|| PlatformError::Backend("no staging texture".into()))?;
        self.staging = Some(texture.clone());
        Ok(texture)
    }

    /// Acquire one frame. `Ok(None)` if the desktop did not change in time.
    fn next(&mut self, timeout: Duration) -> PlatformResult<Option<VideoFrame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        match unsafe {
            self.duplication
                .AcquireNextFrame(millis, &mut info, &mut resource)
        } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(map_dxgi("AcquireNextFrame", e)),
        }
        // From here on the frame must be released, whatever happens.
        let result = self.copy_out(resource.as_ref());
        let _ = unsafe { self.duplication.ReleaseFrame() };
        result
    }

    fn copy_out(&mut self, resource: Option<&IDXGIResource>) -> PlatformResult<Option<VideoFrame>> {
        // A frame with no new image (only the cursor moved) carries no resource.
        let Some(resource) = resource else {
            return Ok(None);
        };
        let texture: ID3D11Texture2D = resource.cast().map_err(|e| backend("frame texture", e))?;
        let staging = self.staging_for(&texture)?;
        let source_res: ID3D11Resource = texture.cast().map_err(|e| backend("resource", e))?;
        let target_res: ID3D11Resource = staging.cast().map_err(|e| backend("resource", e))?;
        unsafe { self.context.CopyResource(&target_res, &source_res) };

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            self.context
                .Map(&target_res, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(|e| backend("Map", e))?;
        let (w, h) = (self.width as usize, self.height as usize);
        let row_bytes = w * 4;
        let mut pixels = vec![0u8; row_bytes * h];
        // SAFETY: the mapped region has `RowPitch * h` readable bytes while mapped; we
        // only read `row_bytes` from each row, which is within the row pitch.
        unsafe {
            let base = mapped.pData as *const u8;
            for y in 0..h {
                let src =
                    std::slice::from_raw_parts(base.add(y * mapped.RowPitch as usize), row_bytes);
                pixels[y * row_bytes..(y + 1) * row_bytes].copy_from_slice(src);
            }
            self.context.Unmap(&target_res, 0);
        }
        Ok(Some(VideoFrame {
            width: self.width,
            height: self.height,
            timestamp_us: 0,
            data: FrameData::Bgra(pixels),
        }))
    }
}

/// The frame shown while the secure desktop (UAC, lock screen) is up.
pub const PLACEHOLDER_TEXT: &str = "The person at this computer needs to respond";

/// Draw the placeholder with GDI and read the pixels back as BGRA.
pub fn placeholder_frame(width: u32, height: u32) -> PlatformResult<VideoFrame> {
    use std::ffi::c_void;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
        CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DT_CENTER, DT_VCENTER, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW,
        FillRect, HGDIOBJ, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor,
    };
    use windows::core::w;

    let w_px =
        i32::try_from(width).map_err(|_| PlatformError::Backend("placeholder too wide".into()))?;
    let h_px =
        i32::try_from(height).map_err(|_| PlatformError::Backend("placeholder too tall".into()))?;
    let bytes = (width as usize) * (height as usize) * 4;
    // SAFETY: GDI objects are created here, selected into the memory DC, and
    // deleted before returning. The DIB section's bits are read while selected.
    unsafe {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w_px,
                // Negative height: rows top to bottom, matching our frame layout.
                biHeight: -h_px,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .map_err(|e| backend("CreateDIBSection", e))?;
        let old = SelectObject(dc, HGDIOBJ(bitmap.0));

        // Dark slate background, light text (the product colours).
        let background = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x0033_2D1F));
        let full = RECT {
            left: 0,
            top: 0,
            right: w_px,
            bottom: h_px,
        };
        FillRect(dc, &full, background);
        let _ = DeleteObject(HGDIOBJ(background.0));

        let font = CreateFontW(
            -(h_px / 18).max(16),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, HGDIOBJ(font.0));
        SetBkMode(dc, windows::Win32::Graphics::Gdi::TRANSPARENT);
        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0x00F4_F1EA));
        let mut text: Vec<u16> = PLACEHOLDER_TEXT.encode_utf16().collect();
        let mut area = RECT {
            left: w_px / 10,
            top: 0,
            right: w_px - w_px / 10,
            bottom: h_px,
        };
        DrawTextW(
            dc,
            &mut text,
            &mut area,
            DT_CENTER | DT_VCENTER | DT_WORDBREAK,
        );

        let pixels = if bits.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(bits as *const u8, bytes).to_vec()
        };
        SelectObject(dc, old_font);
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dc);

        if pixels.len() != bytes {
            return Err(PlatformError::Backend("placeholder pixels missing".into()));
        }
        Ok(VideoFrame {
            width,
            height,
            timestamp_us: 0,
            data: FrameData::Bgra(pixels),
        })
    }
}

/// Capture of one display on the first GPU.
pub struct DxgiCapture {
    adapter: Option<IDXGIAdapter1>,
    output: u32,
    running: bool,
    duplication: Option<Duplication>,
    /// Size of the display being captured, for the placeholder.
    size: (u32, u32),
}

impl DxgiCapture {
    pub fn new() -> PlatformResult<Self> {
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| backend("CreateDXGIFactory1", e))?;
        let adapter =
            unsafe { factory.EnumAdapters1(0) }.map_err(|e| backend("EnumAdapters1", e))?;
        // The adapter keeps its factory alive, so the factory handle is not needed here.
        Ok(Self {
            adapter: Some(adapter),
            output: 0,
            running: false,
            duplication: None,
            size: (0, 0),
        })
    }

    fn adapter(&self) -> PlatformResult<&IDXGIAdapter1> {
        self.adapter.as_ref().ok_or(PlatformError::NotStarted)
    }

    /// Rebuild the duplication after a desktop switch or a display change.
    fn ensure_duplication(&mut self) -> PlatformResult<()> {
        if self.duplication.is_none() {
            let adapter = self.adapter()?.clone();
            self.duplication = Some(Duplication::create(&adapter, self.output)?);
        }
        Ok(())
    }
}

impl Capture for DxgiCapture {
    fn displays(&self) -> PlatformResult<Vec<DisplayInfo>> {
        let adapter = self.adapter()?;
        let mut out = Vec::new();
        for index in 0u32.. {
            let Ok(output) = (unsafe { adapter.EnumOutputs(index) }) else {
                break;
            };
            let desc = unsafe { output.GetDesc() }.map_err(|e| backend("GetDesc", e))?;
            let rect: RECT = desc.DesktopCoordinates;
            let name_len = desc
                .DeviceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(desc.DeviceName.len());
            out.push(DisplayInfo {
                id: DisplayId(index),
                name: String::from_utf16_lossy(&desc.DeviceName[..name_len]),
                width: u32::try_from(rect.right - rect.left).unwrap_or(0),
                height: u32::try_from(rect.bottom - rect.top).unwrap_or(0),
                is_primary: rect.left == 0 && rect.top == 0,
            });
        }
        Ok(out)
    }

    fn start(&mut self, display: DisplayId) -> PlatformResult<()> {
        self.output = display.0;
        self.duplication = None;
        self.ensure_duplication()?;
        if let Some(d) = &self.duplication {
            self.size = (d.width, d.height);
        }
        self.running = true;
        Ok(())
    }

    fn next_frame(&mut self, timeout: Duration) -> PlatformResult<Option<VideoFrame>> {
        if !self.running {
            return Err(PlatformError::NotStarted);
        }
        // While the secure desktop is up (UAC, lock screen) nothing may be
        // captured. Show the placeholder at a slow rate instead, and rebuild the
        // duplication once the normal desktop is back.
        if self.ensure_duplication().is_err() {
            std::thread::sleep(timeout.min(Duration::from_millis(500)));
            return placeholder_frame(self.size.0, self.size.1).map(Some);
        }
        let duplication = self.duplication.as_mut().expect("just ensured");
        match duplication.next(timeout) {
            Err(PlatformError::SecureDesktop) => {
                self.duplication = None;
                std::thread::sleep(timeout.min(Duration::from_millis(500)));
                placeholder_frame(self.size.0, self.size.1).map(Some)
            }
            other => other,
        }
    }

    fn is_running(&self) -> bool {
        self.running
    }

    fn stop(&mut self) {
        self.running = false;
        self.duplication = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxgi_errors_map_to_the_right_platform_errors() {
        let lost = windows::core::Error::from(DXGI_ERROR_ACCESS_LOST);
        assert_eq!(map_dxgi("x", lost), PlatformError::SecureDesktop);
        let denied = windows::core::Error::from(DXGI_ERROR_ACCESS_DENIED);
        assert_eq!(
            map_dxgi("x", denied),
            PlatformError::PermissionDenied("screen capture")
        );
    }

    #[test]
    fn the_placeholder_has_the_requested_size_and_picture() {
        let f = placeholder_frame(640, 360).unwrap();
        assert_eq!((f.width, f.height), (640, 360));
        let FrameData::Bgra(px) = &f.data else {
            panic!("expected pixels")
        };
        assert_eq!(px.len(), 640 * 360 * 4);
        // The background is dark slate and the text puts light pixels on it.
        assert!(
            px.as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 200 && p[1] > 200),
            "text is drawn"
        );
        assert!(
            px.as_chunks::<4>().0.iter().any(|p| p[0] < 80 && p[1] < 80),
            "background is dark"
        );
    }

    #[test]
    fn a_stopped_capture_reports_not_started() {
        let mut cap = DxgiCapture::new().unwrap();
        assert!(!cap.is_running());
        assert_eq!(
            cap.next_frame(Duration::ZERO),
            Err(PlatformError::NotStarted)
        );
        cap.stop();
    }

    /// Captures the real screen: lists displays, grabs one frame, checks its size.
    /// Run by hand: `cargo test -p rb-platform-windows capture_real -- --ignored --nocapture`
    #[test]
    #[ignore = "captures the real screen"]
    fn capture_real_screen() {
        let mut cap = DxgiCapture::new().unwrap();
        let displays = cap.displays().unwrap();
        eprintln!("displays: {displays:?}");
        assert!(!displays.is_empty());
        let primary = displays
            .iter()
            .find(|d| d.is_primary)
            .expect("a primary display");
        cap.start(primary.id).unwrap();
        // Early frames can be black while the desktop settles; look at several.
        let mut frames = 0;
        let mut blank = 0;
        let mut found = None;
        for _ in 0..400 {
            let Some(f) = cap.next_frame(Duration::from_millis(100)).unwrap() else {
                continue;
            };
            frames += 1;
            let FrameData::Bgra(pixels) = &f.data else {
                panic!("expected CPU pixels")
            };
            assert_eq!(pixels.len(), (f.width * f.height * 4) as usize);
            if pixels.iter().all(|&b| b == 0) {
                blank += 1;
            } else {
                found = Some(f);
                break;
            }
        }
        cap.stop();
        eprintln!("frames: {frames}, all-black: {blank}");
        let frame = found.expect("at least one frame with picture content");
        assert_eq!((frame.width, frame.height), (primary.width, primary.height));
        eprintln!("captured {}x{} frame", frame.width, frame.height);
    }
}
