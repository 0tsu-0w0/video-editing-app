//! 検証1: D3D11 の1つのデバイスで、UI(egui)とエフェクト描画(Forge の HLSL)を両立できるか。
//! 1920x1080 の入力に VHS エフェクトを毎フレームかけ、そのテクスチャを
//! コピー無しで egui に表示する。数秒ごとに描画時間を標準出力に出し、指定秒数で終了する。
//!
//! 使い方: ui_d3d11 [秒数(既定 10)]

use std::ffi::CString;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Image, TextureId, Vec2};
use windows::core::PCSTR;
use windows::Win32::Foundation::{HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{Window, WindowAttributes, WindowId};

const W: u32 = 1920;
const H: u32 = 1080;

const VS: &str = r#"
struct VSOut { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
VSOut vsmain(uint id : SV_VertexID) {
    VSOut o; float2 p = float2((id << 1) & 2, id & 2);
    o.uv = p; o.pos = float4(p * float2(2, -2) + float2(-1, 1), 0, 1); return o;
}"#;

fn compile(src: &str, entry: &str, target: &str) -> Vec<u8> {
    let e = CString::new(entry).unwrap();
    let t = CString::new(target).unwrap();
    let mut code: Option<ID3DBlob> = None;
    let mut err: Option<ID3DBlob> = None;
    let r = unsafe {
        D3DCompile(src.as_ptr() as _, src.len(), PCSTR::null(), None, None,
            PCSTR(e.as_ptr() as _), PCSTR(t.as_ptr() as _), 0, 0, &mut code, Some(&mut err))
    };
    if r.is_err() {
        let e = err.unwrap();
        let s = unsafe { std::slice::from_raw_parts(e.GetBufferPointer() as *const u8, e.GetBufferSize()) };
        panic!("compile: {}", String::from_utf8_lossy(s));
    }
    let c = code.unwrap();
    unsafe { std::slice::from_raw_parts(c.GetBufferPointer() as *const u8, c.GetBufferSize()).to_vec() }
}

struct Effect {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    cb: ID3D11Buffer,
    input_srv: ID3D11ShaderResourceView,
    random_srv: ID3D11ShaderResourceView,
    out_rtv: ID3D11RenderTargetView,
    out_srv: ID3D11ShaderResourceView,
    sampler: ID3D11SamplerState,
    query_begin: ID3D11Query,
    query_end: ID3D11Query,
    query_disjoint: ID3D11Query,
}

fn tex(device: &ID3D11Device, w: u32, h: u32, fmt: DXGI_FORMAT, data: &[u8], pitch: u32, rt: bool) -> ID3D11Texture2D {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w, Height: h, MipLevels: 1, ArraySize: 1, Format: fmt,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | if rt { D3D11_BIND_RENDER_TARGET.0 } else { 0 }) as u32,
        ..Default::default()
    };
    let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr() as _, SysMemPitch: pitch, SysMemSlicePitch: 0 };
    let mut t = None;
    unsafe { device.CreateTexture2D(&desc, if data.is_empty() { None } else { Some(&init) }, Some(&mut t)) }.unwrap();
    t.unwrap()
}

fn srv(device: &ID3D11Device, t: &ID3D11Texture2D) -> ID3D11ShaderResourceView {
    let mut v = None;
    unsafe { device.CreateShaderResourceView(t, None, Some(&mut v)) }.unwrap();
    v.unwrap()
}

impl Effect {
    fn new(device: &ID3D11Device) -> Self {
        let vs_code = compile(VS, "vsmain", "vs_5_0");
        let ps_code = compile(include_str!("vhs.hlsl"), "psmain", "ps_5_0");
        let (mut vs, mut ps) = (None, None);
        unsafe {
            device.CreateVertexShader(&vs_code, None, Some(&mut vs)).unwrap();
            device.CreatePixelShader(&ps_code, None, Some(&mut ps)).unwrap();
        }
        // 入力: 1080p のテストパターン(RGBA16F 相当の代わりに RGBA8 で十分)
        let mut img = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let i = ((y * W + x) * 4) as usize;
                let c = if ((x / 64) + (y / 64)) % 2 == 0 { 255 } else { 140 };
                img[i] = (x * 255 / W) as u8 & c;
                img[i + 1] = (y * 255 / H) as u8 & c;
                img[i + 2] = (255 - x * 255 / W) as u8;
                img[i + 3] = 255;
            }
        }
        let input = tex(device, W, H, DXGI_FORMAT_R8G8B8A8_UNORM, &img, W * 4, false);
        let mut s: u32 = 0x1234_5678;
        let rnd: Vec<u8> = (0..256 * 256).flat_map(|_| {
            s ^= s << 13; s ^= s >> 17; s ^= s << 5;
            ((s >> 8) as f32 / (1u32 << 24) as f32).to_le_bytes()
        }).collect();
        let random = tex(device, 256, 256, DXGI_FORMAT_R32_FLOAT, &rnd, 256 * 4, false);
        let out = tex(device, W, H, DXGI_FORMAT_R16G16B16A16_FLOAT, &[], 0, true);
        let mut rtv = None;
        unsafe { device.CreateRenderTargetView(&out, None, Some(&mut rtv)) }.unwrap();

        let cb_desc = D3D11_BUFFER_DESC {
            ByteWidth: 64, Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default()
        };
        let mut cb = None;
        unsafe { device.CreateBuffer(&cb_desc, None, Some(&mut cb)) }.unwrap();
        let sd = D3D11_SAMPLER_DESC {
            Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
            AddressU: D3D11_TEXTURE_ADDRESS_WRAP, AddressV: D3D11_TEXTURE_ADDRESS_WRAP, AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
            MaxLOD: f32::MAX, ..Default::default()
        };
        let mut sampler = None;
        unsafe { device.CreateSamplerState(&sd, Some(&mut sampler)) }.unwrap();
        let q = |kind| {
            let mut x = None;
            unsafe { device.CreateQuery(&D3D11_QUERY_DESC { Query: kind, MiscFlags: 0 }, Some(&mut x)) }.unwrap();
            x.unwrap()
        };
        Self {
            vs: vs.unwrap(), ps: ps.unwrap(), cb: cb.unwrap(),
            input_srv: srv(device, &input), random_srv: srv(device, &random),
            out_srv: srv(device, &out), out_rtv: rtv.unwrap(), sampler: sampler.unwrap(),
            query_begin: q(D3D11_QUERY_TIMESTAMP), query_end: q(D3D11_QUERY_TIMESTAMP),
            query_disjoint: q(D3D11_QUERY_TIMESTAMP_DISJOINT),
        }
    }

    /// 計測なしで1回描く(ベンチマーク用)。
    fn draw_only(&self, ctx: &ID3D11DeviceContext, time: f32) {
        let c: [f32; 16] = [W as f32, H as f32, time, 0.5, (time * 30.0).floor(), 12345.0,
            0.25, 6.0, 3.0, 1.0, 1.0, 232.0 / 255.0, 208.0 / 255.0, 0.0, 0.0, 0.0];
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).unwrap();
            std::ptr::copy_nonoverlapping(c.as_ptr(), m.pData as *mut f32, 16);
            ctx.Unmap(&self.cb, 0);
            ctx.OMSetRenderTargets(Some(&[Some(self.out_rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: W as f32, Height: H as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(self.input_srv.clone()), Some(self.random_srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
        }
    }

    /// エフェクトを1回描き、GPU 上の所要時間(ms)を返す(取得できなければ None)。
    fn draw(&self, ctx: &ID3D11DeviceContext, time: f32) -> Option<f64> {
        // Forge の定数の並び: fg_res_x, fg_res_y, time, progress, frame, seed,
        // noise_intensity, color_shift, jitter, scanlines, tint_r, tint_g, tint_b, grain
        let c: [f32; 16] = [W as f32, H as f32, time, 0.5, (time * 30.0).floor(), 12345.0,
            0.25, 6.0, 3.0, 1.0, 1.0, 232.0 / 255.0, 208.0 / 255.0, 0.0, 0.0, 0.0];
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).unwrap();
            std::ptr::copy_nonoverlapping(c.as_ptr(), m.pData as *mut f32, 16);
            ctx.Unmap(&self.cb, 0);

            ctx.Begin(&self.query_disjoint);
            ctx.End(&self.query_begin);
            ctx.OMSetRenderTargets(Some(&[Some(self.out_rtv.clone())]), None);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: W as f32, Height: H as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.RSSetState(None);
            ctx.IASetInputLayout(None);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&self.vs, None);
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShaderResources(0, Some(&[Some(self.input_srv.clone()), Some(self.random_srv.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
            ctx.Draw(3, 0);
            ctx.PSSetShaderResources(0, Some(&[None, None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.End(&self.query_end);
            ctx.End(&self.query_disjoint);

            // 結果を待つ(検証用。実装では1フレーム遅らせて読む)
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            while ctx.GetData(&self.query_disjoint, Some(&mut dj as *mut _ as _), std::mem::size_of_val(&dj) as u32, 0) != Ok(()) {}
            let (mut a, mut b) = (0u64, 0u64);
            while ctx.GetData(&self.query_begin, Some(&mut a as *mut _ as _), 8, 0) != Ok(()) {}
            while ctx.GetData(&self.query_end, Some(&mut b as *mut _ as _), 8, 0) != Ok(()) {}
            if dj.Disjoint.as_bool() { None } else { Some((b - a) as f64 * 1000.0 / dj.Frequency as f64) }
        }
    }
}

struct Stats { frames: u32, gpu_ms: Vec<f64>, cpu_ms: Vec<f64>, since: Instant }

struct App {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain,
    rtv: Option<ID3D11RenderTargetView>,
    egui_ctx: egui::Context,
    renderer: egui_directx11::Renderer,
    winit_state: egui_winit::State,
    effect: Effect,
    tex: TextureId,
    start: Instant,
    stats: Stats,
    last: String,
    adapter: String,
}

impl App {
    fn new(window: &Window) -> Self {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(h) = window.window_handle().unwrap().as_raw() else { panic!() };
        let hwnd = HWND(h.hwnd.get() as _);
        let size = window.inner_size();
        let factory: IDXGIFactory = unsafe { CreateDXGIFactory() }.unwrap();
        let adapter: IDXGIAdapter = unsafe { factory.EnumAdapters(0) }.unwrap();
        let desc = unsafe { adapter.GetDesc() }.unwrap();
        let name = String::from_utf16_lossy(&desc.Description).trim_end_matches('\0').to_string();
        let (mut device, mut ctx) = (None, None);
        unsafe {
            D3D11CreateDevice(&adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION,
                Some(&mut device), None, Some(&mut ctx)).unwrap();
        }
        let device: ID3D11Device = device.unwrap();
        let sd = DXGI_SWAP_CHAIN_DESC {
            BufferDesc: DXGI_MODE_DESC { Width: size.width, Height: size.height, Format: DXGI_FORMAT_R8G8B8A8_UNORM, ..Default::default() },
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT, BufferCount: 2, OutputWindow: hwnd,
            Windowed: true.into(), SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD, Flags: 0,
        };
        let mut swap = None;
        unsafe { factory.CreateSwapChain(&device, &sd, &mut swap) }.ok().unwrap();
        let swap = swap.unwrap();
        let egui_ctx = egui::Context::default();
        let mut renderer = egui_directx11::Renderer::new(&device).unwrap();
        let winit_state = egui_winit::State::new(egui_ctx.clone(), egui_ctx.viewport_id(), &window, None, None, None);
        let effect = Effect::new(&device);
        // エフェクトの出力テクスチャを、コピーせずに egui へ登録する
        let tex = renderer.register_user_texture(effect.out_srv.clone());
        let rtv = Some(Self::make_rtv(&device, &swap));
        Self {
            device, ctx: ctx.unwrap(), swap, rtv, egui_ctx, renderer, winit_state, effect, tex,
            start: Instant::now(),
            stats: Stats { frames: 0, gpu_ms: vec![], cpu_ms: vec![], since: Instant::now() },
            last: String::new(), adapter: name,
        }
    }

    fn make_rtv(device: &ID3D11Device, swap: &IDXGISwapChain) -> ID3D11RenderTargetView {
        let back: ID3D11Texture2D = unsafe { swap.GetBuffer(0) }.unwrap();
        let mut rtv = None;
        unsafe { device.CreateRenderTargetView(&back, None, Some(&mut rtv)) }.unwrap();
        rtv.unwrap()
    }

    fn render(&mut self, window: &Window) {
        let t0 = Instant::now();
        let time = self.start.elapsed().as_secs_f32();
        let gpu = self.effect.draw(&self.ctx, time);
        let input = self.winit_state.take_egui_input(window);
        let (tex, last, adapter) = (self.tex, self.last.clone(), self.adapter.clone());
        let out = self.egui_ctx.run_ui(input, |ui| {
            CentralPanel::default().show(ui, |ui| {
                ui.label(format!("検証: D3D11 1デバイスで egui + Forge HLSL ({adapter})"));
                ui.label(&last);
                ui.add(Image::from_texture((tex, Vec2::new(W as f32, H as f32))).shrink_to_fit());
            });
        });
        let (r, platform, _) = egui_directx11::split_output(out);
        self.winit_state.handle_platform_output(window, platform);
        let rtv = self.rtv.clone().unwrap();
        unsafe { self.ctx.ClearRenderTargetView(&rtv, &[0.1, 0.1, 0.1, 1.0]) };
        let _ = self.renderer.render(&self.ctx, &rtv, &self.egui_ctx, r);
        let _ = unsafe { self.swap.Present(1, DXGI_PRESENT(0)) };

        self.stats.frames += 1;
        self.stats.cpu_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        if let Some(g) = gpu { self.stats.gpu_ms.push(g) }
        if self.stats.since.elapsed() >= Duration::from_secs(2) {
            let fps = self.stats.frames as f64 / self.stats.since.elapsed().as_secs_f64();
            let avg = |v: &Vec<f64>| if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 };
            let max = |v: &Vec<f64>| v.iter().cloned().fold(0.0, f64::max);
            self.last = format!(
                "表示 {:.1} fps / エフェクト(GPU) 平均 {:.2} ms 最大 {:.2} ms / 1フレームのCPU処理 平均 {:.2} ms",
                fps, avg(&self.stats.gpu_ms), max(&self.stats.gpu_ms), avg(&self.stats.cpu_ms)
            );
            println!("{}", self.last);
            self.stats = Stats { frames: 0, gpu_ms: vec![], cpu_ms: vec![], since: Instant::now() };
        }
    }

    fn resize(&mut self, s: PhysicalSize<u32>) {
        if s.width == 0 || s.height == 0 { return; }
        self.rtv = None;
        unsafe { self.swap.ResizeBuffers(2, s.width, s.height, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0)) }.unwrap();
        self.rtv = Some(Self::make_rtv(&self.device, &self.swap));
    }
}

struct Runner { window: Option<Window>, app: Option<App>, quit_at: Instant }

impl ApplicationHandler for Runner {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let w = el.create_window(WindowAttributes::default().with_title("studio spike: ui_d3d11")
            .with_inner_size(PhysicalSize::new(1280, 800))).unwrap();
        let app = App::new(&w);
        println!("アダプタ: {}", app.adapter);
        self.app = Some(app);
        self.window = Some(w);
    }
    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if Instant::now() >= self.quit_at { el.exit(); return; }
        if let Some(w) = &self.window { w.request_redraw(); }
    }
    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, ev: WindowEvent) {
        let (Some(w), Some(app)) = (&self.window, &mut self.app) else { return };
        let _ = app.winit_state.on_window_event(w, &ev);
        match ev {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(s) => app.resize(s),
            WindowEvent::RedrawRequested => app.render(w),
            _ => {}
        }
    }
}

/// 画面を出さずに、エフェクトを n 回描いて1回あたりの時間を測る。
fn bench(n: u32) {
    let (mut device, mut ctx) = (None, None);
    unsafe {
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0),
            Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)).unwrap();
    }
    let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device.unwrap(), ctx.unwrap());
    let fx = Effect::new(&device);
    let mut ev = None;
    unsafe { device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut ev)) }.unwrap();
    let ev = ev.unwrap();
    let wait = || unsafe {
        ctx.End(&ev);
        let mut done: i32 = 0;
        while ctx.GetData(&ev, Some(&mut done as *mut _ as _), 4, 0) != Ok(()) || done == 0 {}
    };
    for i in 0..20 { fx.draw_only(&ctx, i as f32 / 30.0); }
    wait();
    let t = Instant::now();
    for i in 0..n { fx.draw_only(&ctx, i as f32 / 30.0); }
    wait();
    let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("VHS エフェクト 1920x1080: 1回 {:.3} ms(60fps の予算 16.7 ms の {:.1}%)", ms, ms / 16.67 * 100.0);
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("bench") {
        bench(300);
        return;
    }
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut r = Runner { window: None, app: None, quit_at: Instant::now() + Duration::from_secs(secs) };
    EventLoop::new().unwrap().run_app(&mut r).unwrap();
}
