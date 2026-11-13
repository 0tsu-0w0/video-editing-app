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
// マスクとワイプ(素材のピクセルで計算する)
cbuffer Mask : register(b2) {
    float4 m_rect;   // 中心 x, y(左上原点のピクセル)、半分の幅・高さ
    float4 m_opt;    // 形(0 無し, 1 四角, 2 楕円)、反転、ぼかし、_
    float4 m_wipe;   // 種類(0 無し, 1 左→右, 2 右→左, 3 上→下, 4 下→上, 5 円)、見えている割合、_、_
    float4 m_size;   // 素材の幅・高さ
};
float coverage(float e, float feather) {
    return feather > 0.5 ? saturate(e / feather + 0.5) : (e >= 0 ? 1.0 : 0.0);
}
float mask_at(float2 uv) {
    float m = 1;
    float2 px = uv * m_size.xy;
    if (m_opt.x > 0.5) {
        float2 d = abs(px - m_rect.xy);
        float2 hs = max(m_rect.zw, 1e-3);
        float e;
        if (m_opt.x < 1.5) {
            e = min(hs.x - d.x, hs.y - d.y);
        } else {
            e = (1 - length(d / hs)) * min(hs.x, hs.y);
        }
        m = coverage(e, m_opt.z);
        if (m_opt.y > 0.5) m = 1 - m;
    }
    if (m_wipe.x > 0.5) {
        float v;
        int k = (int)(m_wipe.x + 0.5);
        if (k == 1) v = uv.x;
        else if (k == 2) v = 1 - uv.x;
        else if (k == 3) v = uv.y;
        else if (k == 4) v = 1 - uv.y;
        else v = length(px - m_size.xy * 0.5) / length(m_size.xy * 0.5);
        float soft = 0.04;
        m *= saturate((m_wipe.y * (1 + soft) - v) / soft);
    }
    return m;
}
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
    return c * (opacity * mask_at(i.uv));
}

// 四隅の貼り付け: 画面全体を覆う三角形を描き、各画素から素材の座標を射影変換で逆算する
cbuffer Pin : register(b1) {
    float4 h0;   // 画面の画素 → 素材の uv(3x3 の各行、xyz を使う)
    float4 h1;
    float4 h2;
    float pin_opacity;
    float3 _pad2;
};
VSOut vs_full(uint id : SV_VertexID) {
    VSOut o;
    float2 p = float2(id == 2 ? 3.0 : -1.0, id == 1 ? 3.0 : -1.0);
    o.pos = float4(p, 0, 1);
    o.uv = 0;
    return o;
}
float4 ps_pin(VSOut i) : SV_Target {
    float3 s = float3(i.pos.xy, 1);
    float3 u = float3(dot(h0.xyz, s), dot(h1.xyz, s), dot(h2.xyz, s));
    if (u.z <= 0) discard;
    float2 uv = u.xy / u.z;
    if (any(uv < 0) || any(uv > 1)) discard;
    float4 c = tex.SampleLevel(smp, uv, 0);
    c.rgb *= c.a;
    return c * (pin_opacity * mask_at(uv));
}
"#;

pub fn compile(entry: &str, target: &str) -> Result<Vec<u8>, String> {
    compile_src(SHADER, entry, target)
}

pub fn compile_src(src: &str, entry: &str, target: &str) -> Result<Vec<u8>, String> {
    let (e, t) = (CString::new(entry).unwrap(), CString::new(target).unwrap());
    let (mut code, mut err): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    let r = unsafe {
        D3DCompile(src.as_ptr() as _, src.len(), PCSTR::null(), None, None, PCSTR(e.as_ptr() as _), PCSTR(t.as_ptr() as _), 0, 0, &mut code, Some(&mut err))
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
    vs_full: ID3D11VertexShader,
    ps_pin: ID3D11PixelShader,
    cb_pin: ID3D11Buffer,
    cb_mask: ID3D11Buffer,
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

#[repr(C)]
struct PinCb {
    h: [[f32; 4]; 3],
    opacity: f32,
    _pad: [f32; 3],
}

type M3 = [[f64; 3]; 3];

/// レイヤーのマスクとワイプ(`Gpu::set_mask` で、次に描くレイヤーに使う)。
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct MaskCb {
    /// 中心 x, y(素材の左上原点のピクセル)、半分の幅・高さ
    pub rect: [f32; 4],
    /// 形(0 無し, 1 四角, 2 楕円)、反転(0/1)、ぼかし(ピクセル)、未使用
    pub opt: [f32; 4],
    /// ワイプの種類(0 無し, 1 左→右, 2 右→左, 3 上→下, 4 下→上, 5 円)、見えている割合
    pub wipe: [f32; 4],
    /// 素材の幅・高さ
    pub size: [f32; 4],
}

/// 単位正方形(u, v ∈ [0,1])を四角形 q(左上・右上・右下・左下)へ写す射影変換。
pub fn square_to_quad(q: &[[f64; 2]; 4]) -> M3 {
    let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = *q;
    let (dx1, dx2, dx3) = (x1 - x2, x3 - x2, x0 - x1 + x2 - x3);
    let (dy1, dy2, dy3) = (y1 - y2, y3 - y2, y0 - y1 + y2 - y3);
    let det = dx1 * dy2 - dx2 * dy1;
    let (g, h) = if det.abs() < 1e-12 || (dx3.abs() < 1e-12 && dy3.abs() < 1e-12) { (0.0, 0.0) } else { ((dx3 * dy2 - dx2 * dy3) / det, (dx1 * dy3 - dx3 * dy1) / det) };
    [[x1 - x0 + g * x1, x3 - x0 + h * x3, x0], [y1 - y0 + g * y1, y3 - y0 + h * y3, y0], [g, h, 1.0]]
}

pub fn invert3(m: &M3) -> Option<M3> {
    let c = |r: usize, k: usize| m[(r + 1) % 3][(k + 1) % 3] * m[(r + 2) % 3][(k + 2) % 3] - m[(r + 1) % 3][(k + 2) % 3] * m[(r + 2) % 3][(k + 1) % 3];
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if det.abs() < 1e-15 {
        return None;
    }
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for k in 0..3 {
            o[k][r] = c(r, k) / det;
        }
    }
    Some(o)
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
        let vfb = compile("vs_full", "vs_5_0")?;
        let ppb = compile("ps_pin", "ps_5_0")?;
        let (mut vs, mut ps, mut cb, mut blend, mut sampler, mut raster) = (None, None, None, None, None, None);
        let (mut vs_full, mut ps_pin, mut cb_pin, mut cb_mask) = (None, None, None, None);
        unsafe {
            device.CreateVertexShader(&vsb, None, Some(&mut vs)).map_err(e("頂点シェーダ"))?;
            device.CreatePixelShader(&psb, None, Some(&mut ps)).map_err(e("ピクセルシェーダ"))?;
            device.CreateVertexShader(&vfb, None, Some(&mut vs_full)).map_err(e("頂点シェーダ"))?;
            device.CreatePixelShader(&ppb, None, Some(&mut ps_pin)).map_err(e("ピクセルシェーダ"))?;
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: std::mem::size_of::<PinCb>() as u32,
                        Usage: D3D11_USAGE_DYNAMIC,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        ..Default::default()
                    },
                    None,
                    Some(&mut cb_pin),
                )
                .map_err(e("定数バッファ"))?;
            let zero = MaskCb::default();
            device
                .CreateBuffer(
                    &D3D11_BUFFER_DESC {
                        ByteWidth: std::mem::size_of::<MaskCb>() as u32,
                        Usage: D3D11_USAGE_DYNAMIC,
                        BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                        CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                        ..Default::default()
                    },
                    Some(&D3D11_SUBRESOURCE_DATA { pSysMem: &zero as *const MaskCb as _, ..Default::default() }),
                    Some(&mut cb_mask),
                )
                .map_err(e("定数バッファ"))?;
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
        Ok(Self { device, ctx, vs: vs.unwrap(), ps: ps.unwrap(), cb: cb.unwrap(), vs_full: vs_full.unwrap(), ps_pin: ps_pin.unwrap(), cb_pin: cb_pin.unwrap(), cb_mask: cb_mask.unwrap(), blend: blend.unwrap(), sampler: sampler.unwrap(), raster: raster.unwrap() })
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

    /// 次に描くレイヤーのマスクとワイプを決める(無しにするには `MaskCb::default()`)。
    pub fn set_mask(&self, m: &MaskCb) {
        unsafe {
            let mut mp = D3D11_MAPPED_SUBRESOURCE::default();
            if self.ctx.Map(&self.cb_mask, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mp)).is_ok() {
                std::ptr::copy_nonoverlapping(m as *const MaskCb as *const u8, mp.pData as *mut u8, std::mem::size_of::<MaskCb>());
                self.ctx.Unmap(&self.cb_mask, 0);
            }
        }
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
            ctx.PSSetConstantBuffers(2, Some(&[Some(self.cb_mask.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(src.srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(4, 0);
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
        }
    }

    /// レイヤーを四隅の位置(画面のピクセル、中央が原点、y は下向き。左上・右上・右下・左下)に合わせて描く。
    pub fn draw_pinned(&self, rtv: &ID3D11RenderTargetView, target: (u32, u32), src: &Texture, corners: &[[f64; 2]; 4], opacity: f64) {
        let (tw, th) = (target.0 as f64, target.1 as f64);
        let q = corners.map(|p| [p[0] + tw / 2.0, p[1] + th / 2.0]);
        let Some(mut inv) = invert3(&square_to_quad(&q)) else { return };
        // 四角形の内側で w が正になる向きにそろえる
        let (cx, cy) = (q.iter().map(|p| p[0]).sum::<f64>() / 4.0, q.iter().map(|p| p[1]).sum::<f64>() / 4.0);
        if inv[2][0] * cx + inv[2][1] * cy + inv[2][2] < 0.0 {
            inv = inv.map(|r| r.map(|v| -v));
        }
        let row = |r: [f64; 3]| [r[0] as f32, r[1] as f32, r[2] as f32, 0.0];
        let cb = PinCb { h: [row(inv[0]), row(inv[1]), row(inv[2])], opacity: opacity as f32, _pad: [0.0; 3] };
        unsafe {
            let ctx = &self.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            if ctx.Map(&self.cb_pin, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).is_err() {
                return;
            }
            std::ptr::copy_nonoverlapping(&cb as *const PinCb as *const u8, m.pData as *mut u8, std::mem::size_of::<PinCb>());
            ctx.Unmap(&self.cb_pin, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
            ctx.OMSetBlendState(&self.blend, None, 0xffff_ffff);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: tw as f32, Height: th as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs_full, None);
            ctx.PSSetShader(&self.ps_pin, None);
            ctx.PSSetConstantBuffers(1, Some(&[Some(self.cb_pin.clone())]));
            ctx.PSSetConstantBuffers(2, Some(&[Some(self.cb_mask.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(src.srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
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
