// Reference: heraklessii/Muivly (Apache-2.0), compositor/* — NV12 Y/UV plane sampling and D3D11 draw flow.
use anyhow::{anyhow, bail, Context, Result};
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::{BOOL, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::{
    D3DCompile, D3DCOMPILE_ENABLE_STRICTNESS, D3DCOMPILE_OPTIMIZATION_LEVEL3,
};
use windows::Win32::Graphics::Direct3D::{
    ID3DBlob, ID3DInclude, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST, D3D11_SRV_DIMENSION_TEXTURE2D,
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0,
};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Buffer, ID3D11ClassLinkage, ID3D11DepthStencilView, ID3D11Device,
    ID3D11DeviceContext, ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_SHADER_RESOURCE, D3D11_BUFFER_DESC,
    D3D11_COMPARISON_ALWAYS, D3D11_CPU_ACCESS_WRITE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_CULL_NONE, D3D11_FILL_SOLID,
    D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_WRITE_DISCARD,
    D3D11_RASTERIZER_DESC, D3D11_SAMPLER_DESC, D3D11_SDK_VERSION, D3D11_SHADER_RESOURCE_VIEW_DESC,
    D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
    D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_USAGE_DEFAULT, D3D11_USAGE_DYNAMIC, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_FORMAT_R8G8_UNORM,
    DXGI_FORMAT_R8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIFactory2, IDXGISwapChain1, DXGI_ERROR_DEVICE_REMOVED,
    DXGI_ERROR_DEVICE_RESET, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Media::MediaFoundation::MFVideoFormat_NV12;

use super::decoder::FrameTex;

/// Compute the normalized (u0, v0, u1, v1) source sub-rectangle used to fill a
/// destination with "cover" scaling (preserve aspect ratio, crop the overflow).
///
/// The returned coordinates are in normalized source space [0, 1] and always
/// describe a centred crop. Same aspect ratio yields the full `(0, 0, 1, 1)`.
pub fn cover_rect(src: (u32, u32), dst: (u32, u32)) -> (f32, f32, f32, f32) {
    let (sw, sh) = (src.0 as f32, src.1 as f32);
    let (dw, dh) = (dst.0 as f32, dst.1 as f32);
    if sw <= 0.0 || sh <= 0.0 || dw <= 0.0 || dh <= 0.0 {
        return (0.0, 0.0, 1.0, 1.0);
    }
    let src_aspect = sw / sh;
    let dst_aspect = dw / dh;
    if (src_aspect - dst_aspect).abs() < 1e-6 {
        (0.0, 0.0, 1.0, 1.0)
    } else if dst_aspect > src_aspect {
        // Destination is wider: scale to width and crop top/bottom.
        let visible = src_aspect / dst_aspect;
        let v0 = (1.0 - visible) * 0.5;
        (0.0, v0, 1.0, 1.0 - v0)
    } else {
        // Destination is taller/narrower: scale to height and crop left/right.
        let visible = dst_aspect / src_aspect;
        let u0 = (1.0 - visible) * 0.5;
        (u0, 0.0, 1.0 - u0, 1.0)
    }
}

const SHADER_SRC: &str = r#"
cbuffer Params : register(b0)
{
    float4 uv_rect;
};

struct VSOut
{
    float4 pos : SV_POSITION;
    float2 uv  : TEXCOORD0;
};

VSOut VSMain(uint id : SV_VertexID)
{
    float2 pos[3] = { float2(-1.0, -1.0), float2(3.0, -1.0), float2(-1.0, 3.0) };
    float2 uv[3]  = { float2(0.0, 1.0),  float2(2.0, 1.0),  float2(0.0, -1.0) };
    VSOut o;
    o.pos = float4(pos[id], 0.0, 1.0);
    o.uv = uv[id];
    return o;
}

Texture2D<float>  texY  : register(t0);
Texture2D<float2> texUV : register(t1);
SamplerState      samp  : register(s0);

float4 PSMain(VSOut i) : SV_Target
{
    float2 uv = lerp(uv_rect.xy, uv_rect.zw, i.uv);
    float y  = texY.Sample(samp, uv).r;
    float2 c = texUV.Sample(samp, uv);

    // BT.709, limited (studio) range -> full-range RGB.
    float cb = (c.x * 255.0 - 128.0) / 224.0;
    float cr = (c.y * 255.0 - 128.0) / 224.0;
    y = (y * 255.0 - 16.0) / 219.0;

    float3 rgb;
    rgb.r = y + 1.5748 * cr;
    rgb.g = y - 0.1873 * cb - 0.4681 * cr;
    rgb.b = y + 1.8556 * cb;
    return float4(saturate(rgb), 1.0);
}
"#;

pub struct Renderer {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    rtv: ID3D11RenderTargetView,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
    cbuffer: ID3D11Buffer,
    owned: Option<ID3D11Texture2D>,
    srv_y: Option<ID3D11ShaderResourceView>,
    srv_uv: Option<ID3D11ShaderResourceView>,
    tex_dims: (u32, u32),
}

impl Renderer {
    /// Create the shared D3D11 device (and its immediate context) used by every
    /// monitor renderer and by Media Foundation decoding.
    pub fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext)> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            let mut level = D3D_FEATURE_LEVEL_11_0;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut ctx),
            )?;
            let device = device.ok_or_else(|| anyhow!("D3D11CreateDevice returned null device"))?;
            let ctx = ctx.ok_or_else(|| anyhow!("D3D11CreateDevice returned null context"))?;
            // MF/DXVA may touch this device from its own threads; enable the
            // D3D10 multithread layer so concurrent access is serialized.
            match device.cast::<ID3D10Multithread>() {
                Ok(mt) => {
                    let previous = mt.SetMultithreadProtected(BOOL(1));
                    log::info!(
                        "renderer: SetMultithreadProtected(TRUE) ok (previous={})",
                        previous.as_bool()
                    );
                }
                Err(e) => log::warn!(
                    "renderer: ID3D10Multithread unavailable ({e:?}); D3D/MF concurrency not protected"
                ),
            }
            Ok((device, ctx))
        }
    }

    pub fn new(hwnd: HWND, w: u32, h: u32) -> Result<Self> {
        let (device, ctx) = Self::create_device()?;
        Self::new_with_device(hwnd, w, h, &device, &ctx)
    }

    /// Build a renderer on an existing shared device/context.
    pub fn new_with_device(
        hwnd: HWND,
        w: u32,
        h: u32,
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
    ) -> Result<Self> {
        unsafe {
            let device = device.clone();
            let ctx = ctx.clone();
            let dxgi: IDXGIDevice = device.cast()?;
            let adapter = dxgi.GetAdapter()?;
            let factory: IDXGIFactory2 = adapter.GetParent()?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: w,
                Height: h,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                ..Default::default()
            };
            let swap = factory.CreateSwapChainForHwnd(&device, hwnd, &desc, None, None)?;
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut rtv: Option<ID3D11RenderTargetView> = None;
            device.CreateRenderTargetView(&back, None, Some(&mut rtv))?;
            let rtv = rtv.ok_or_else(|| anyhow!("CreateRenderTargetView returned null view"))?;

            let vs_bytes = compile(SHADER_SRC, "VSMain", "vs_5_0")?;
            let ps_bytes = compile(SHADER_SRC, "PSMain", "ps_5_0")?;
            let mut vs: Option<ID3D11VertexShader> = None;
            let mut ps: Option<ID3D11PixelShader> = None;
            device.CreateVertexShader(
                blob_bytes(&vs_bytes),
                None::<&ID3D11ClassLinkage>,
                Some(&mut vs),
            )?;
            device.CreatePixelShader(
                blob_bytes(&ps_bytes),
                None::<&ID3D11ClassLinkage>,
                Some(&mut ps),
            )?;
            let vs = vs.ok_or_else(|| anyhow!("CreateVertexShader returned null"))?;
            let ps = ps.ok_or_else(|| anyhow!("CreatePixelShader returned null"))?;

            let samp_desc = D3D11_SAMPLER_DESC {
                Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D11_COMPARISON_ALWAYS,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: f32::MAX,
            };
            let mut sampler: Option<ID3D11SamplerState> = None;
            device.CreateSamplerState(&samp_desc, Some(&mut sampler))?;
            let sampler = sampler.ok_or_else(|| anyhow!("CreateSamplerState returned null"))?;

            let raster_desc = D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                FrontCounterClockwise: BOOL(0),
                DepthBias: 0,
                DepthBiasClamp: 0.0,
                SlopeScaledDepthBias: 0.0,
                DepthClipEnable: BOOL(1),
                ScissorEnable: BOOL(0),
                MultisampleEnable: BOOL(0),
                AntialiasedLineEnable: BOOL(0),
            };
            let mut raster: Option<ID3D11RasterizerState> = None;
            device.CreateRasterizerState(&raster_desc, Some(&mut raster))?;
            let raster = raster.ok_or_else(|| anyhow!("CreateRasterizerState returned null"))?;

            let cb_desc = D3D11_BUFFER_DESC {
                ByteWidth: 16,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            };
            let mut cbuffer: Option<ID3D11Buffer> = None;
            device.CreateBuffer(&cb_desc, None, Some(&mut cbuffer))?;
            let cbuffer = cbuffer.ok_or_else(|| anyhow!("CreateBuffer(constant) returned null"))?;

            Ok(Self {
                device,
                ctx,
                swap,
                rtv,
                vs,
                ps,
                sampler,
                raster,
                cbuffer,
                owned: None,
                srv_y: None,
                srv_uv: None,
                tex_dims: (0, 0),
            })
        }
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    fn ensure_frame_resources(&mut self, tex_w: u32, tex_h: u32) -> Result<()> {
        if self.owned.is_some() && self.tex_dims == (tex_w, tex_h) {
            return Ok(());
        }
        unsafe {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: tex_w,
                Height: tex_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut owned: Option<ID3D11Texture2D> = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut owned))
                .with_context(|| {
                    format!("CreateTexture2D(NV12 {tex_w}x{tex_h}, SHADER_RESOURCE)")
                })?;
            let owned =
                owned.ok_or_else(|| anyhow!("CreateTexture2D(NV12) returned null texture"))?;

            let make_srv = |format| -> Result<ID3D11ShaderResourceView> {
                let sd = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: format,
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                        },
                    },
                };
                let mut srv: Option<ID3D11ShaderResourceView> = None;
                self.device
                    .CreateShaderResourceView(&owned, Some(&sd), Some(&mut srv))
                    .with_context(|| format!("CreateShaderResourceView({format:?})"))?;
                srv.ok_or_else(|| anyhow!("CreateShaderResourceView returned null"))
            };
            let srv_y = make_srv(DXGI_FORMAT_R8_UNORM)?;
            let srv_uv = make_srv(DXGI_FORMAT_R8G8_UNORM)?;

            log::info!(
                "renderer: allocated NV12 frame texture {}x{} with R8/R8G8 views",
                tex_w,
                tex_h
            );
            self.owned = Some(owned);
            self.srv_y = Some(srv_y);
            self.srv_uv = Some(srv_uv);
            self.tex_dims = (tex_w, tex_h);
        }
        Ok(())
    }

    /// Copy the decoded NV12 frame into an owned texture and draw it covering
    /// the destination. The copy must complete while `frame` is alive: `FrameTex`
    /// holds the source `IMFSample` precisely so MF cannot recycle the pooled
    /// surface underneath this `CopySubresourceRegion`. Only the visible
    /// `width x height` region of the possibly padded texture is sampled.
    pub fn draw_nv12(&mut self, frame: &FrameTex, dst_w: u32, dst_h: u32) -> Result<()> {
        if frame.format != MFVideoFormat_NV12 {
            bail!(
                "renderer: unsupported frame format {} (expected NV12)",
                frame.format_name()
            );
        }
        let (tex_w, tex_h) = (frame.tex_width, frame.tex_height);
        if tex_w == 0 || tex_h == 0 || frame.width == 0 || frame.height == 0 {
            bail!("renderer: frame has empty dimensions");
        }
        self.ensure_frame_resources(tex_w, tex_h)?;

        let (u0, v0, u1, v1) = cover_rect((frame.width, frame.height), (dst_w, dst_h));
        let sx = frame.width as f32 / tex_w as f32;
        let sy = frame.height as f32 / tex_h as f32;
        let rect = [u0 * sx, v0 * sy, u1 * sx, v1 * sy];

        unsafe {
            let owned = self
                .owned
                .as_ref()
                .ok_or_else(|| anyhow!("renderer: owned frame texture missing"))?;
            let srv_y = self
                .srv_y
                .as_ref()
                .ok_or_else(|| anyhow!("renderer: Y SRV missing"))?;
            let srv_uv = self
                .srv_uv
                .as_ref()
                .ok_or_else(|| anyhow!("renderer: UV SRV missing"))?;

            self.ctx.CopySubresourceRegion(
                owned,
                0,
                0,
                0,
                0,
                &frame.texture,
                frame.subresource,
                None,
            );

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(
                &self.cbuffer,
                0,
                D3D11_MAP_WRITE_DISCARD,
                0,
                Some(&mut mapped),
            )?;
            std::ptr::copy_nonoverlapping(rect.as_ptr(), mapped.pData as *mut f32, 4);
            self.ctx.Unmap(&self.cbuffer, 0);

            self.ctx.OMSetRenderTargets(
                Some(&[Some(self.rtv.clone())]),
                None::<&ID3D11DepthStencilView>,
            );
            let vp = D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: dst_w as f32,
                Height: dst_h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            self.ctx.RSSetViewports(Some(&[vp]));
            self.ctx.RSSetState(&self.raster);
            self.ctx
                .IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            self.ctx.VSSetShader(&self.vs, None);
            self.ctx.PSSetShader(&self.ps, None);
            self.ctx
                .VSSetConstantBuffers(0, Some(&[Some(self.cbuffer.clone())]));
            self.ctx
                .PSSetConstantBuffers(0, Some(&[Some(self.cbuffer.clone())]));
            self.ctx
                .PSSetShaderResources(0, Some(&[Some(srv_y.clone()), Some(srv_uv.clone())]));
            self.ctx
                .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            self.ctx.Draw(3, 0);
            // Unbind the copied frame so the next CopySubresourceRegion does not
            // target a resource that is still bound as a shader input.
            self.ctx.PSSetShaderResources(0, Some(&[None, None]));
        }
        Ok(())
    }

    /// Present the current back buffer. Returns the device status so the
    /// engine can detect `DEVICE_REMOVED`/`DEVICE_RESET` and rebuild rather
    /// than silently rendering into a dead swap chain.
    pub fn present(&self) -> windows::core::Result<()> {
        unsafe { self.swap.Present(1, DXGI_PRESENT(0)).ok() }
    }
}

/// True when a present failure means the D3D device is gone and the renderer
/// must be recreated (adapter reset, driver update, TDR, etc.).
pub fn is_device_lost(error: &windows::core::Error) -> bool {
    let code = error.code();
    code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET
}

unsafe fn compile(src: &str, entry: &str, target: &str) -> Result<ID3DBlob> {
    let mut entry_bytes = entry.as_bytes().to_vec();
    entry_bytes.push(0);
    let mut target_bytes = target.as_bytes().to_vec();
    target_bytes.push(0);
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = D3DCompile(
        src.as_ptr() as *const core::ffi::c_void,
        src.len(),
        PCSTR::null(),
        None,
        None::<&ID3DInclude>,
        PCSTR::from_raw(entry_bytes.as_ptr()),
        PCSTR::from_raw(target_bytes.as_ptr()),
        D3DCOMPILE_ENABLE_STRICTNESS | D3DCOMPILE_OPTIMIZATION_LEVEL3,
        0,
        &mut code,
        Some(&mut errors),
    );
    if let Err(e) = result {
        let message = errors
            .as_ref()
            .map(|blob| blob_string(blob))
            .unwrap_or_default();
        bail!("D3DCompile({entry} / {target}) failed: {e:?}\n{message}");
    }
    code.ok_or_else(|| anyhow!("D3DCompile({entry} / {target}) returned no bytecode"))
}

unsafe fn blob_bytes(blob: &ID3DBlob) -> &[u8] {
    std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize())
}

unsafe fn blob_string(blob: &ID3DBlob) -> String {
    String::from_utf8_lossy(blob_bytes(blob)).into_owned()
}
