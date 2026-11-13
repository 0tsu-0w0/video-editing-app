//! Direct3D 11 の薄いラッパー。

use crate::Image;
use forge_core::spec::{Blend, Sampler};
use std::ffi::CString;
use windows::core::{Interface, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const VS_SOURCE: &str = r#"
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vsmain(uint id : SV_VertexID) {
    VSOut o;
    float2 p = float2((id << 1) & 2, id & 2);
    o.uv = p;
    o.pos = float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}
"#;

fn blob_bytes(blob: &ID3DBlob) -> Vec<u8> {
    unsafe {
        std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()).to_vec()
    }
}

/// HLSL をコンパイルしてバイトコードを返す。失敗時はコンパイラのログを返す。
pub fn compile(source: &str, entry: &str, target: &str) -> Result<Vec<u8>, String> {
    let entry_c = CString::new(entry).map_err(|_| "エントリ名が不正です".to_string())?;
    let target_c = CString::new(target).map_err(|_| "ターゲットが不正です".to_string())?;
    let mut code: Option<ID3DBlob> = None;
    let mut errors: Option<ID3DBlob> = None;
    let result = unsafe {
        D3DCompile(
            source.as_ptr() as *const _,
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr() as *const u8),
            PCSTR(target_c.as_ptr() as *const u8),
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    match (result, code) {
        (Ok(()), Some(code)) => Ok(blob_bytes(&code)),
        _ => {
            let log = errors
                .map(|e| String::from_utf8_lossy(&blob_bytes(&e)).trim_end_matches('\0').trim().to_string())
                .unwrap_or_else(|| "原因不明のコンパイルエラー".to_string());
            Err(log)
        }
    }
}

pub struct PixelShader(ID3D11PixelShader);
pub struct Constants(ID3D11Buffer);

pub struct Texture {
    tex: ID3D11Texture2D,
    rtv: Option<ID3D11RenderTargetView>,
    srv: ID3D11ShaderResourceView,
    width: u32,
    height: u32,
}

pub struct Gpu {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vs: ID3D11VertexShader,
    raster: ID3D11RasterizerState,
    /// ソフトウェア描画(WARP)で動いているか。
    pub software: bool,
}

fn e<T: std::fmt::Display>(what: &str) -> impl Fn(T) -> String + '_ {
    move |err| format!("{what}: {err}")
}

impl Gpu {
    /// デバイスを作る。`force_software` なら WARP(GPU 無しでも動く)。
    pub fn new(force_software: bool) -> Result<Self, String> {
        let try_create = |driver: D3D_DRIVER_TYPE| -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
            let mut device = None;
            let mut ctx = None;
            unsafe {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_FLAG(0),
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut ctx),
                )
            }
            .map_err(e("Direct3D 11 デバイスを作れません"))?;
            Ok((device.ok_or("デバイスがありません")?, ctx.ok_or("コンテキストがありません")?))
        };
        let (device, ctx, software) = if force_software {
            let (d, c) = try_create(D3D_DRIVER_TYPE_WARP)?;
            (d, c, true)
        } else {
            match try_create(D3D_DRIVER_TYPE_HARDWARE) {
                Ok((d, c)) => (d, c, false),
                Err(_) => {
                    let (d, c) = try_create(D3D_DRIVER_TYPE_WARP)?;
                    (d, c, true)
                }
            }
        };

        let vs_code = compile(VS_SOURCE, "vsmain", "vs_5_0")?;
        let mut vs = None;
        unsafe { device.CreateVertexShader(&vs_code, None, Some(&mut vs)) }
            .map_err(e("頂点シェーダを作れません"))?;

        let rd = D3D11_RASTERIZER_DESC {
            FillMode: D3D11_FILL_SOLID,
            CullMode: D3D11_CULL_NONE,
            DepthClipEnable: true.into(),
            ..Default::default()
        };
        let mut raster = None;
        unsafe { device.CreateRasterizerState(&rd, Some(&mut raster)) }
            .map_err(e("ラスタライザ状態を作れません"))?;

        Ok(Self { device, ctx, vs: vs.ok_or("頂点シェーダがありません")?, raster: raster.ok_or("ラスタライザ状態がありません")?, software })
    }

    pub fn create_pixel_shader(&self, bytecode: &[u8]) -> Result<PixelShader, String> {
        let mut ps = None;
        unsafe { self.device.CreatePixelShader(bytecode, None, Some(&mut ps)) }
            .map_err(e("ピクセルシェーダを作れません"))?;
        Ok(PixelShader(ps.ok_or("ピクセルシェーダがありません")?))
    }

    /// float の配列を定数バッファ(b0)にする。
    pub fn create_constants(&self, values: &[f32]) -> Result<Constants, String> {
        let mut padded = values.to_vec();
        while padded.is_empty() || padded.len() % 4 != 0 {
            padded.push(0.0);
        }
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: (padded.len() * 4) as u32,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: padded.as_ptr() as *const _, ..Default::default() };
        let mut buf = None;
        unsafe { self.device.CreateBuffer(&desc, Some(&init), Some(&mut buf)) }
            .map_err(e("定数バッファを作れません"))?;
        Ok(Constants(buf.ok_or("定数バッファがありません")?))
    }

    fn make_texture(
        &self,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        data: &[f32],
        channels: u32,
        render_target: bool,
    ) -> Result<Texture, String> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(format!("画像サイズ {width}x{height} は扱えません"));
        }
        if data.len() != (width * height * channels) as usize {
            return Err("画像データの長さがサイズと合いません".into());
        }
        let mut bind = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        if render_target {
            bind |= D3D11_BIND_RENDER_TARGET.0 as u32;
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            ..Default::default()
        };
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: data.as_ptr() as *const _,
            SysMemPitch: width * channels * 4,
            SysMemSlicePitch: 0,
        };
        let mut tex = None;
        unsafe { self.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }
            .map_err(e("テクスチャを作れません"))?;
        let tex = tex.ok_or("テクスチャがありません")?;
        let mut srv = None;
        unsafe { self.device.CreateShaderResourceView(&tex, None, Some(&mut srv)) }
            .map_err(e("SRV を作れません"))?;
        let mut rtv = None;
        if render_target {
            unsafe { self.device.CreateRenderTargetView(&tex, None, Some(&mut rtv)) }
                .map_err(e("RTV を作れません"))?;
        }
        Ok(Texture { tex, rtv, srv: srv.ok_or("SRV がありません")?, width, height })
    }

    pub fn create_texture(&self, image: &Image) -> Result<Texture, String> {
        self.make_texture(image.width, image.height, DXGI_FORMAT_R32G32B32A32_FLOAT, &image.data, 4, true)
    }

    /// ホストの "random" バッファ相当(256x256、r のみ、0.0〜1.0)。
    pub fn random_texture(&self) -> Result<Texture, String> {
        let mut state: u32 = 0x1234_5678;
        let data: Vec<f32> = (0..256 * 256)
            .map(|_| {
                // xorshift32
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 8) as f32 / (1u32 << 24) as f32
            })
            .collect();
        self.make_texture(256, 256, DXGI_FORMAT_R32_FLOAT, &data, 1, false)
    }

    pub fn clone_texture(&self, src: &Texture) -> Result<Texture, String> {
        let blank = Image::new(src.width, src.height);
        let dst = self.create_texture(&blank)?;
        unsafe { self.ctx.CopyResource(&dst.tex, &src.tex) };
        Ok(dst)
    }

    fn blend_state(&self, blend: Blend) -> Result<Option<ID3D11BlendState>, String> {
        let (src, dst) = match blend {
            Blend::Copy => return Ok(None),
            Blend::Draw => (D3D11_BLEND_ONE, D3D11_BLEND_INV_SRC_ALPHA),
            Blend::Add => (D3D11_BLEND_ONE, D3D11_BLEND_ONE),
            Blend::Mask => (D3D11_BLEND_ZERO, D3D11_BLEND_SRC_ALPHA),
        };
        let mut desc = D3D11_BLEND_DESC::default();
        desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
            BlendEnable: true.into(),
            SrcBlend: src,
            DestBlend: dst,
            BlendOp: D3D11_BLEND_OP_ADD,
            SrcBlendAlpha: src,
            DestBlendAlpha: dst,
            BlendOpAlpha: D3D11_BLEND_OP_ADD,
            RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
        };
        let mut bs = None;
        unsafe { self.device.CreateBlendState(&desc, Some(&mut bs)) }
            .map_err(e("ブレンド状態を作れません"))?;
        Ok(bs)
    }

    fn sampler_state(&self, sampler: Sampler) -> Result<ID3D11SamplerState, String> {
        let (filter, address) = match sampler {
            Sampler::Clip => (D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_BORDER),
            Sampler::Clamp => (D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP),
            Sampler::Loop => (D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_WRAP),
            Sampler::Mirror => (D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_MIRROR),
            Sampler::Dot => (D3D11_FILTER_MIN_MAG_MIP_POINT, D3D11_TEXTURE_ADDRESS_BORDER),
        };
        let desc = D3D11_SAMPLER_DESC {
            Filter: filter,
            AddressU: address,
            AddressV: address,
            AddressW: address,
            ComparisonFunc: D3D11_COMPARISON_NEVER,
            BorderColor: [0.0; 4],
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut s = None;
        unsafe { self.device.CreateSamplerState(&desc, Some(&mut s)) }
            .map_err(e("サンプラを作れません"))?;
        s.ok_or_else(|| "サンプラがありません".to_string())
    }

    /// 全画面に1回描く(obj.pixelshader 相当)。
    pub fn draw(
        &self,
        ps: &PixelShader,
        target: &Texture,
        resources: &[&Texture],
        constants: &Constants,
        blend: Blend,
        sampler: Sampler,
    ) -> Result<(), String> {
        let rtv = target.rtv.clone().ok_or("描画先にできないテクスチャです")?;
        let blend_state = self.blend_state(blend)?;
        let sampler_state = self.sampler_state(sampler)?;
        let srvs: Vec<Option<ID3D11ShaderResourceView>> =
            resources.iter().map(|t| Some(t.srv.clone())).collect();
        let viewport = D3D11_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: target.width as f32,
            Height: target.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        unsafe {
            let ctx = &self.ctx;
            ctx.OMSetRenderTargets(Some(&[Some(rtv)]), None);
            ctx.OMSetBlendState(blend_state.as_ref(), None, 0xffff_ffff);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[viewport]));
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&ps.0, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(constants.0.clone())]));
            ctx.PSSetShaderResources(0, Some(&srvs));
            ctx.PSSetSamplers(0, Some(&[Some(sampler_state)]));
            ctx.Draw(3, 0);
            // 次のパスで同じテクスチャを描画先にできるよう、束縛を外す。
            let none: Vec<Option<ID3D11ShaderResourceView>> = vec![None; srvs.len()];
            ctx.PSSetShaderResources(0, Some(&none));
            ctx.OMSetRenderTargets(None, None);
        }
        Ok(())
    }

    /// テクスチャの内容を読み戻す。
    pub fn read_texture(&self, tex: &Texture) -> Result<Image, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: tex.width,
            Height: tex.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        let mut staging = None;
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging)) }
            .map_err(e("読み戻し用テクスチャを作れません"))?;
        let staging = staging.ok_or("読み戻し用テクスチャがありません")?;
        let resource: ID3D11Resource = staging.cast().map_err(e("キャストに失敗"))?;
        let mut image = Image::new(tex.width, tex.height);
        unsafe {
            self.ctx.CopyResource(&staging, &tex.tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx
                .Map(&resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(e("読み戻しに失敗しました"))?;
            let row_floats = (tex.width * 4) as usize;
            for y in 0..tex.height as usize {
                let src = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize) as *const f32;
                let row = std::slice::from_raw_parts(src, row_floats);
                image.data[y * row_floats..(y + 1) * row_floats].copy_from_slice(row);
            }
            self.ctx.Unmap(&resource, 0);
        }
        Ok(image)
    }
}
