// Reference: heraklessii/Muivly (Apache-2.0), decoder/video.rs — Media Foundation + D3D11 hardware decode flow.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, GUID, HSTRING, PROPVARIANT};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC;
use windows::Win32::Media::MediaFoundation::*;

static MF_INIT: OnceLock<Result<(), String>> = OnceLock::new();
static MF_SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

extern "C" fn mf_atexit() {
    shutdown_mf();
}

fn ensure_mf() -> Result<()> {
    let state = MF_INIT.get_or_init(|| unsafe {
        let result = MFStartup(MF_VERSION, MFSTARTUP_FULL);
        if result.is_ok() {
            let _ = atexit(mf_atexit);
        }
        result.map_err(|e| format!("MFStartup failed: {e:?}"))
    });
    match state {
        Ok(()) => Ok(()),
        Err(msg) => Err(anyhow!(msg.clone())),
    }
}

/// Pair the single process-wide `MFStartup` with `MFShutdown`. Idempotent.
///
/// Called by the C runtime `atexit` hook registered on first startup and by the
/// engine on a graceful shutdown, so normal process exit always pairs the two.
pub fn shutdown_mf() {
    if MF_SHUTDOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    if !matches!(MF_INIT.get(), Some(Ok(()))) {
        return;
    }
    unsafe {
        match MFShutdown() {
            Ok(()) => log::info!("decoder: MFShutdown complete"),
            Err(e) => log::warn!("decoder: MFShutdown failed: {e:?}"),
        }
    }
}

pub struct FrameTex {
    pub texture: ID3D11Texture2D,
    pub ts: i64,
    pub width: u32,
    pub height: u32,
    pub tex_width: u32,
    pub tex_height: u32,
    pub format: GUID,
    pub subresource: u32,
    /// Keeps the source `IMFSample` (and therefore its pooled DXGI surface)
    /// alive until the consumer finishes copying the texture. Dropping the
    /// sample lets Media Foundation recycle that surface for the next decode,
    /// which would race the renderer's `CopySubresourceRegion`. The texture
    /// must not be copied after this `FrameTex` is dropped.
    _sample: IMFSample,
}

impl FrameTex {
    pub fn format_name(&self) -> String {
        format_name(self.format)
    }
}

pub struct Decoder {
    reader: IMFSourceReader,
    manager: IMFDXGIDeviceManager,
    _device: ID3D11Device,
    path: String,
    width: u32,
    height: u32,
    format: GUID,
    stream: u32,
    eos_count: u64,
}

impl Decoder {
    pub fn open(path: &str, device: &ID3D11Device) -> Result<Self> {
        ensure_mf()?;
        unsafe {
            let mut token = 0u32;
            let mut manager: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)
                .context("MFCreateDXGIDeviceManager")?;
            let manager =
                manager.ok_or_else(|| anyhow!("MFCreateDXGIDeviceManager returned null"))?;
            manager
                .ResetDevice(device, token)
                .context("IMFDXGIDeviceManager::ResetDevice")?;

            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let (reader, width, height, format, native) = create_reader(&manager, path, stream)?;
            log::info!(
                "decoder: opened {:?} {}x{} {} (native {})",
                path,
                width,
                height,
                format_name(format),
                format_name(native)
            );
            Ok(Self {
                reader,
                manager,
                _device: device.clone(),
                path: path.to_string(),
                width,
                height,
                format,
                stream,
                eos_count: 0,
            })
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> GUID {
        self.format
    }

    /// Number of times the reader has wrapped around at end of stream.
    pub fn eos_count(&self) -> u64 {
        self.eos_count
    }

    pub fn next_frame(&mut self) -> Result<FrameTex> {
        let mut rewinds = 0;
        loop {
            let mut actual_stream = 0u32;
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            unsafe {
                self.reader
                    .ReadSample(
                        self.stream,
                        0,
                        Some(&mut actual_stream),
                        Some(&mut flags),
                        Some(&mut ts),
                        Some(&mut sample),
                    )
                    .context("IMFSourceReader::ReadSample")?;
            }
            let flags = MF_SOURCE_READER_FLAG(flags as i32);
            if flags.0 & MF_SOURCE_READERF_ERROR.0 != 0 {
                bail!("decoder: ReadSample reported MF_SOURCE_READERF_ERROR");
            }
            if flags.0 & MF_SOURCE_READERF_ENDOFSTREAM.0 != 0 {
                rewinds += 1;
                if rewinds > 4 {
                    bail!("decoder: still at end of stream after {rewinds} rewind attempts");
                }
                self.eos_count += 1;
                log::info!(
                    "decoder: end of stream; restarting from frame 0 (eos #{})",
                    self.eos_count
                );
                self.restart()?;
                continue;
            }
            if flags.0 & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 != 0 {
                self.refresh_media_type()?;
            }
            if flags.0 & MF_SOURCE_READERF_STREAMTICK.0 != 0 {
                continue;
            }
            let Some(sample) = sample else {
                continue;
            };
            let ts = unsafe { sample.GetSampleTime() }.unwrap_or(ts);
            return Self::sample_to_frame(&sample, ts, self.width, self.height, self.format);
        }
    }

    fn restart(&mut self) -> Result<()> {
        unsafe {
            let position = PROPVARIANT::from(0i64);
            let time_format = GUID::zeroed();
            match self.reader.SetCurrentPosition(&time_format, &position) {
                Ok(()) => {
                    log::info!("decoder: rewind via SetCurrentPosition(0) ok");
                    Ok(())
                }
                Err(e) => {
                    log::warn!(
                        "decoder: SetCurrentPosition(0) failed: {e:?}; reopening source reader"
                    );
                    self.reopen()
                }
            }
        }
    }

    fn reopen(&mut self) -> Result<()> {
        let (reader, width, height, format, _native) =
            create_reader(&self.manager, &self.path, self.stream)?;
        self.reader = reader;
        self.width = width;
        self.height = height;
        self.format = format;
        log::info!(
            "decoder: reopened {:?} {}x{} {}",
            self.path,
            width,
            height,
            format_name(format)
        );
        Ok(())
    }

    fn refresh_media_type(&mut self) -> Result<()> {
        unsafe {
            let current = self
                .reader
                .GetCurrentMediaType(self.stream)
                .context("GetCurrentMediaType (type changed)")?;
            let format = current
                .GetGUID(&MF_MT_SUBTYPE)
                .context("current MF_MT_SUBTYPE")?;
            let size = current
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .context("current MF_MT_FRAME_SIZE")?;
            let (w, h) = unpack_size(size);
            log::info!(
                "decoder: media type changed -> format {} allocation {}x{} (display {}x{})",
                format_name(format),
                w,
                h,
                self.width,
                self.height
            );
            self.format = format;
        }
        Ok(())
    }

    fn sample_to_frame(
        sample: &IMFSample,
        ts: i64,
        width: u32,
        height: u32,
        format: GUID,
    ) -> Result<FrameTex> {
        unsafe {
            let buffer = sample
                .GetBufferByIndex(0)
                .context("IMFSample::GetBufferByIndex(0)")?;
            let dxgi: IMFDXGIBuffer = buffer
                .cast()
                .context("buffer is not IMFDXGIBuffer (no hardware D3D11 surface)")?;
            let subresource = dxgi
                .GetSubresourceIndex()
                .context("IMFDXGIBuffer::GetSubresourceIndex")?;
            let mut raw: *mut core::ffi::c_void = std::ptr::null_mut();
            dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
                .context("IMFDXGIBuffer::GetResource(ID3D11Texture2D)")?;
            if raw.is_null() {
                bail!("decoder: IMFDXGIBuffer::GetResource returned null texture");
            }
            let texture = ID3D11Texture2D::from_raw(raw);
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            Ok(FrameTex {
                texture,
                ts,
                width,
                height,
                tex_width: desc.Width,
                tex_height: desc.Height,
                format,
                subresource,
                _sample: sample.clone(),
            })
        }
    }
}

/// Build a SourceReader bound to `manager`, select the first video stream, and
/// negotiate NV12 output. Returns `(reader, display_w, display_h, format, native)`.
fn create_reader(
    manager: &IMFDXGIDeviceManager,
    path: &str,
    stream: u32,
) -> Result<(IMFSourceReader, u32, u32, GUID, GUID)> {
    unsafe {
        let url = HSTRING::from(path);
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 3).context("MFCreateAttributes")?;
        let attrs = attrs.ok_or_else(|| anyhow!("MFCreateAttributes returned null"))?;
        attrs
            .SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, manager)
            .context("set MF_SOURCE_READER_D3D_MANAGER")?;
        attrs
            .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
            .context("set MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS")?;
        let reader = MFCreateSourceReaderFromURL(&url, &attrs)
            .with_context(|| format!("MFCreateSourceReaderFromURL({path:?})"))?;

        reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, BOOL(0))?;
        reader
            .SetStreamSelection(stream, BOOL(1))
            .context("select first video stream")?;

        let native = reader
            .GetNativeMediaType(stream, 0)
            .context("GetNativeMediaType(first video stream)")?;
        let native_subtype = native
            .GetGUID(&MF_MT_SUBTYPE)
            .context("native MF_MT_SUBTYPE")?;
        let native_size = native
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .context("native MF_MT_FRAME_SIZE")?;

        let requested = if native_subtype == MFVideoFormat_NV12 {
            native_subtype
        } else {
            log::info!(
                "decoder: native subtype {} -> requesting NV12",
                format_name(native_subtype)
            );
            MFVideoFormat_NV12
        };

        let mt = MFCreateMediaType().context("MFCreateMediaType")?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        mt.SetGUID(&MF_MT_SUBTYPE, &requested)?;
        mt.SetUINT64(&MF_MT_FRAME_SIZE, native_size)?;
        mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        reader
            .SetCurrentMediaType(stream, None, &mt)
            .context("SetCurrentMediaType(NV12 + D3D manager)")?;

        let current = reader
            .GetCurrentMediaType(stream)
            .context("GetCurrentMediaType after negotiation")?;
        let format = current
            .GetGUID(&MF_MT_SUBTYPE)
            .context("current MF_MT_SUBTYPE")?;
        let current_size = current
            .GetUINT64(&MF_MT_FRAME_SIZE)
            .context("current MF_MT_FRAME_SIZE")?;
        let (width, height) = unpack_size(native_size);
        let (cur_w, cur_h) = unpack_size(current_size);
        if width == 0 || height == 0 {
            bail!("decoder: native media type has empty frame size {width}x{height}");
        }
        if (cur_w, cur_h) != (width, height) {
            log::info!(
                "decoder: negotiated allocation {}x{} for display {}x{}",
                cur_w,
                cur_h,
                width,
                height
            );
        }
        Ok((reader, width, height, format, native_subtype))
    }
}

fn unpack_size(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, (v & 0xffff_ffff) as u32)
}

pub fn format_name(g: GUID) -> String {
    let name = if g == MFVideoFormat_NV12 {
        "NV12"
    } else if g == MFVideoFormat_P010 {
        "P010"
    } else if g == MFVideoFormat_YUY2 {
        "YUY2"
    } else if g == MFVideoFormat_RGB32 {
        "RGB32"
    } else {
        return format!("unknown({g:?})");
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
    };

    fn test_video() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("ACTIVEDESKTOP_TEST_VIDEO") {
            let p = PathBuf::from(p);
            return p.exists().then_some(p);
        }
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("video/Autumn Waterfall.mp4");
        p.exists().then_some(p)
    }

    fn create_device() -> ID3D11Device {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )
            .expect("D3D11CreateDevice");
            device.expect("device")
        }
    }

    #[test]
    fn smoke_decode_first_frame() {
        let Some(path) = test_video() else {
            eprintln!("skip: test video not found; set ACTIVEDESKTOP_TEST_VIDEO");
            return;
        };
        let device = create_device();
        let mut decoder = Decoder::open(path.to_str().unwrap(), &device).expect("open decoder");
        eprintln!(
            "decoder: {}x{} format={}",
            decoder.width(),
            decoder.height(),
            format_name(decoder.format())
        );
        let frame = decoder.next_frame().expect("next_frame");
        eprintln!(
            "decoded frame: {}x{} format={} ts={} subresource={} texture={}x{}",
            frame.width,
            frame.height,
            frame.format_name(),
            frame.ts,
            frame.subresource,
            frame.tex_width,
            frame.tex_height
        );
        assert_eq!((frame.width, frame.height), (1920, 1080));
        assert_eq!(frame.format, MFVideoFormat_NV12);
        assert!(frame.tex_width >= frame.width && frame.tex_height >= frame.height);
        assert!(!frame.texture.as_raw().is_null());
    }
}
