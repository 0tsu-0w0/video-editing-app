//! D3D11 での合成(各レイヤーを位置・拡大・回転・不透明度つきで重ねる)。

use std::ffi::CString;
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const SHADER: &str = r#"
cbuffer Layer : register(b0) {
    float2 axis_x;   // 単位四角形の x 方向 → 画面(NDC)
    float2 axis_y;
    float2 center;   // NDC
    float opacity;
    float _pad;
};
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vsmain(uint id : SV_VertexID) {
    float2 uv = float2(id & 1, id >> 1);          // 0,0 1,0 0,1 1,1(三角形ストリップ)
    float2 q = uv - 0.5;
    VSOut o;
    o.pos = float4(center + axis_x * q.x + axis_y * q.y, 0, 1);
    o.uv = uv;
    return o;
}
float4 psmain(VSOut i) : SV_Target {
    float4 c = tex.Sample(smp, i.uv);   // 素材はストレートアルファ
    c.rgb *= c.a;                       // 乗算済みにして重ねる
    return c * opacity;
}
"#;

pub fn compile(entry: &str, target: &str) -> Result<Vec<u8>, String> {
    let (e, t) = (CString::new(entry).unwrap(), CString::new(target).unwrap());
    let (mut code, mut err): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    let r = unsafe {
        D3DCompile(SHADER.as_ptr() as _, SHADER.len(), PCSTR::null(), None, None, PCSTR(e.as_ptr() as _), PCSTR(t.as_ptr() as _), 0, 0, &mut code, Some(&mut err))
    };
    let bytes = |b: &ID3DBlob| unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec() };
    match (r, code) {
        (Ok(()), Some(c)) => Ok(bytes(&c)),
        _ => Err(err.map(|e| String::from_utf8_lossy(&bytes(&e)).to_string()).unwrap_or_default()),
    }
}

fn e<T: std::fmt::Display>(what: &'static str) -> impl Fn(T) -> String {
    move |x| format!("{what}: {x}")
}

/// GPU 上の画像(BGRA 8bit)。
pub struct Texture {
    pub tex: ID3D11Texture2D,
    pub srv: ID3D11ShaderResourceView,
    pub width: u32,
    pub height: u32,
}

pub struct Gpu {
    pub device: ID3D11Device,
    pub ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    blend: ID3D11BlendState,
    sampler: ID3D11SamplerState,
    raster: ID3D11RasterizerState,
}

#[repr(C)]
struct LayerCb {
    axis_x: [f32; 2],
    axis_y: [f32; 2],
    center: [f32; 2],
    opacity: f32,
    _pad: f32,
}

/// 1 枚のレイヤーの置き方(ピクセル単位、画面中央が原点、y は下向き)。
pub struct Placement {
    pub width: f64,
    pub height: f64,
    pub x: f64,
    pub y: f64,
    pub scale: f64,
    /// 度、時計回り
    pub rotation: f64,
    pub opacity: f64,
}

impl Gpu {
    /// 既存のデバイス(UI と共有)から作る。None なら新しく作る。
    pub fn new(device: Option<(ID3D11Device, ID3D11DeviceContext)>) -> Result<Self, String> {
        let (device, ctx) = match device {
            Some(d) => d,
            None => {
                let (mut d, mut c) = (None, None);
                let mut ok = Err("".to_string());
                for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
                    ok = unsafe {
                        D3D11CreateDevice(None, driver, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut d), None, Some(&mut c))
                    }
                    .map_err(|x| x.to_string());
                    if ok.is_ok() {
                        break;
                    }
                }
                ok.map_err(|x| format!("Direct3D 11 を初期化できません: {x}"))?;
                (d.unwrap(), c.unwrap())
            }
        };
        let vsb = compile("vsmain", "vs_5_0")?;
        let psb = compile("psmain", "ps_5_0")?;
        let (mut vs, mut ps, mut cb, mut blend, mut sampler, mut raster) = (None, None, None, None, None, None);
        unsafe {
            device.CreateVertexShader(&vsb, None, Some(&mut vs)).map_err(e("頂点シェーダ"))?;
            device.CreatePixelShader(&psb, None, Some(&mut ps)).map_err(e("ピクセルシェーダ"))?;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: std::mem::size_of::<LayerCb>() as u32,
                        Usage: D3D11_USAGE_DYNAMIC,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut cb),
                )
                .map_err(e("定数バッファ"))?;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            device.CreateBlendState(&bd, Some(&mut blend)).map_err(e("ブレンド"))?;
            device
                .CreateSamplerState(
                    &D3D11_SAMPLER_DESC {
                        Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                        AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                        AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                        AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                        MaxLOD: f32::MAX,
                        ..Default::default()
                    },
                    Some(&mut sampler),
                )
                .map_err(e("サンプラ"))?;
            device
                .CreateRasterizerState(&D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() }, Some(&mut raster))
                .map_err(e("ラスタライザ"))?;
        }
        Ok(Self { device, ctx, vs: vs.unwrap(), ps: ps.unwrap(), cb: cb.unwrap(), blend: blend.unwrap(), sampler: sampler.unwrap(), raster: raster.unwrap() })
    }

    pub fn create_texture(&self, width: u32, height: u32, bgra: Option<&[u8]>, render_target: bool) -> Result<Texture, String> {
        let mut bind = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        if render_target {
            bind |= D3D11_BIND_RENDER_TARGET.0 as u32;
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            ..Default::default()
        };
        let init = bgra.map(|d| D3D11_SUBRESOURCE_DATA { pSysMem: d.as_ptr() as _, SysMemPitch: width * 4, SysMemSlicePitch: 0 });
        let (mut tex, mut srv) = (None, None);
        unsafe {
            self.device.CreateTexture2D(&desc, init.as_ref().map(|x| x as *const _), Some(&mut tex)).map_err(e("テクスチャ"))?;
            let tex = tex.unwrap();
            self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)).map_err(e("SRV"))?;
            Ok(Texture { tex, srv: srv.unwrap(), width, height })
        }
    }

    pub fn update_texture(&self, t: &Texture, bgra: &[u8]) {
        unsafe { self.ctx.UpdateSubresource(&t.tex, 0, None, bgra.as_ptr() as _, t.width * 4, 0) };
    }

    pub fn rtv(&self, t: &Texture) -> Result<ID3D11RenderTargetView, String> {
        let mut r = None;
        unsafe { self.device.CreateRenderTargetView(&t.tex, None, Some(&mut r)) }.map_err(e("RTV"))?;
        Ok(r.unwrap())
    }

    pub fn clear(&self, rtv: &ID3D11RenderTargetView, rgba: [f32; 4]) {
        unsafe { self.ctx.ClearRenderTargetView(rtv, &rgba) };
    }

    /// レイヤーを 1 枚描く。
    pub fn draw(&self, rtv: &ID3D11RenderTargetView, target: (u32, u32), src: &Texture, p: &Placement) {
        let (tw, th) = (target.0 as f64, target.1 as f64);
        let (s, c) = p.rotation.to_radians().sin_cos();
        let (w, h) = (p.width * p.scale, p.height * p.scale);
        // ピクセル(y 下向き)→ NDC(y 上向き)
        let to_ndc = |vx: f64, vy: f64| [(vx / (tw / 2.0)) as f32, (-vy / (th / 2.0)) as f32];
        let cb = LayerCb {
            axis_x: to_ndc(w * c, w * s),
            axis_y: to_ndc(-h * s, h * c),
            center: to_ndc(p.x, p.y),
            opacity: p.opacity as f32,
            _pad: 0.0,
        };
        unsafe {
            let ctx = &self.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_err() {
                return;
            }
            std::ptr::copy_nonoverlapping(&cb as *const LayerCb as *const u8, m.pData as *mut u8, std::mem::size_of::<LayerCb>());
            ctx.Unmap(&self.cb, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: tw as f32, Height: th as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(src.srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(4, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }

    /// テクスチャを BGRA で読み戻す。
    pub fn read(&self, t: &Texture) -> Result<Vec<u8>, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: t.width,
            Height: t.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut st = None;
        unsafe {
            self.device.CreateTexture2D(&desc, None, Some(&mut st)).map_err(e("読み戻し用テクスチャ"))?;
            let st: ID3D11Texture2D = st.unwrap();
            self.ctx.CopyResource(&st, &t.tex);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(e("読み戻し"))?;
            let row = (t.width * 4) as usize;
            let mut out = vec![0u8; row * t.height as usize];
            for y in 0..t.height as usize {
                let src = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), row);
                out[y * row..(y + 1) * row].copy_from_slice(src);
            }
            self.ctx.Unmap(&st, 0);
            Ok(out)
        }
    }
}
