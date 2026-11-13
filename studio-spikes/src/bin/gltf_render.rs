//! 検証5: glTF の読み込み・骨アニメーション(スキニング)・D3D11 での描画・映像への合成。
//!
//! 使い方: gltf_render <モデル.gltf> [出力先フォルダ]
//! - 時刻 0 / 0.5 / 1.0 秒の描画結果を、テスト背景に合成して PNG に保存する。
//! - 1920x1080 で 200 回描いて、1 回あたりの時間を測る。
//! - 骨が正しく動いているかを、描画結果の上端の位置(曲がると下がる)で確かめる。

use std::ffi::CString;
use std::time::Instant;
use windows::core::PCSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const W: u32 = 1920;
const H: u32 = 1080;
const MAX_JOINTS: usize = 64;

type M4 = [f32; 16]; // 列優先

fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

fn trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> M4 {
    let [x, y, z, w] = q;
    let r = [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        0.0, 0.0, 0.0, 1.0,
    ];
    let mut m = r;
    for c in 0..3 {
        for row in 0..3 {
            m[c * 4 + row] *= s[c];
        }
    }
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> M4 {
    // D3D の深度 0..1、右手系(カメラは -Z を向く)
    let f = 1.0 / (fovy / 2.0).tan();
    [
        f / aspect, 0.0, 0.0, 0.0,
        0.0, f, 0.0, 0.0,
        0.0, 0.0, far / (near - far), -1.0,
        0.0, 0.0, near * far / (near - far), 0.0,
    ]
}

fn look_at(eye: [f32; 3], at: [f32; 3]) -> M4 {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let norm = |v: [f32; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let f = norm(sub(at, eye));
    let s = norm(cross(f, [0.0, 1.0, 0.0]));
    let u = cross(s, f);
    [
        s[0], u[0], -f[0], 0.0,
        s[1], u[1], -f[1], 0.0,
        s[2], u[2], -f[2], 0.0,
        -dot(s, eye), -dot(u, eye), dot(f, eye), 1.0,
    ]
}

fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut d: f32 = (0..4).map(|i| a[i] * b[i]).sum();
    let mut b = b;
    if d < 0.0 {
        d = -d;
        b = b.map(|v| -v);
    }
    let (wa, wb) = if d > 0.9995 {
        (1.0 - t, t)
    } else {
        let th = d.acos();
        ((th * (1.0 - t)).sin() / th.sin(), (th * t).sin() / th.sin())
    };
    let q: Vec<f32> = (0..4).map(|i| a[i] * wa + b[i] * wb).collect();
    let l = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    [q[0] / l, q[1] / l, q[2] / l, q[3] / l]
}

struct Node {
    parent: Option<usize>,
    t: [f32; 3],
    r: [f32; 4],
    s: [f32; 3],
}

struct Channel {
    node: usize,
    times: Vec<f32>,
    rotations: Vec<[f32; 4]>,
}

struct Model {
    vertices: Vec<f32>, // pos3 nrm3 joints4(float) weights4 = 14 floats
    indices: Vec<u32>,
    nodes: Vec<Node>,
    joints: Vec<usize>,
    ibm: Vec<M4>,
    channels: Vec<Channel>,
    color: [f32; 4],
}

fn load(path: &str) -> Model {
    let (doc, buffers, _images) = gltf::import(path).expect("glTF を読めません");
    let mut nodes: Vec<Node> = doc
        .nodes()
        .map(|n| {
            let (t, r, s) = n.transform().decomposed();
            Node { parent: None, t, r, s }
        })
        .collect();
    for n in doc.nodes() {
        for c in n.children() {
            nodes[c.index()].parent = Some(n.index());
        }
    }
    let prim = doc.meshes().next().unwrap().primitives().next().unwrap();
    let rd = prim.reader(|b| Some(&buffers[b.index()]));
    let pos: Vec<[f32; 3]> = rd.read_positions().unwrap().collect();
    let nrm: Vec<[f32; 3]> = rd.read_normals().unwrap().collect();
    let jnt: Vec<[u16; 4]> = rd.read_joints(0).unwrap().into_u16().collect();
    let wgt: Vec<[f32; 4]> = rd.read_weights(0).unwrap().into_f32().collect();
    let indices: Vec<u32> = rd.read_indices().unwrap().into_u32().collect();
    let mut vertices = Vec::with_capacity(pos.len() * 14);
    for i in 0..pos.len() {
        vertices.extend(pos[i]);
        vertices.extend(nrm[i]);
        vertices.extend(jnt[i].map(|j| j as f32));
        vertices.extend(wgt[i]);
    }
    let skin = doc.skins().next().unwrap();
    let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
    let ibm: Vec<M4> = skin
        .reader(|b| Some(&buffers[b.index()]))
        .read_inverse_bind_matrices()
        .unwrap()
        .map(|m| {
            let mut r = [0.0; 16];
            for c in 0..4 {
                for row in 0..4 {
                    r[c * 4 + row] = m[c][row];
                }
            }
            r
        })
        .collect();
    let mut channels = Vec::new();
    if let Some(anim) = doc.animations().next() {
        for ch in anim.channels() {
            let rd = ch.reader(|b| Some(&buffers[b.index()]));
            if let Some(gltf::animation::util::ReadOutputs::Rotations(r)) = rd.read_outputs() {
                channels.push(Channel {
                    node: ch.target().node().index(),
                    times: rd.read_inputs().unwrap().collect(),
                    rotations: r.into_f32().collect(),
                });
            }
        }
    }
    let color = prim.material().pbr_metallic_roughness().base_color_factor();
    Model { vertices, indices, nodes, joints, ibm, channels, color }
}

impl Model {
    /// 時刻 t の骨の行列(グローバル × 逆バインド)。
    fn joint_matrices(&self, t: f32) -> Vec<M4> {
        let mut rot: Vec<[f32; 4]> = self.nodes.iter().map(|n| n.r).collect();
        for ch in &self.channels {
            let end = *ch.times.last().unwrap();
            let t = t % end;
            let k = ch.times.iter().rposition(|x| *x <= t).unwrap_or(0).min(ch.times.len() - 2);
            let f = ((t - ch.times[k]) / (ch.times[k + 1] - ch.times[k])).clamp(0.0, 1.0);
            rot[ch.node] = slerp(ch.rotations[k], ch.rotations[k + 1], f);
        }
        let global = |mut i: usize| {
            let mut m = trs(self.nodes[i].t, rot[i], self.nodes[i].s);
            while let Some(p) = self.nodes[i].parent {
                m = mul(&trs(self.nodes[p].t, rot[p], self.nodes[p].s), &m);
                i = p;
            }
            m
        };
        self.joints.iter().zip(&self.ibm).map(|(j, ibm)| mul(&global(*j), ibm)).collect()
    }
}

const SHADER: &str = r#"
cbuffer Frame : register(b0) {
    float4x4 viewproj;
    float4 base_color;
    float4 light_dir;
    float4x4 joints[64];
};
struct VSIn { float3 pos : POSITION; float3 nrm : NORMAL; float4 j : BLENDINDICES; float4 w : BLENDWEIGHT; };
struct VSOut { float4 pos : SV_Position; float3 nrm : NORMAL; };
VSOut vsmain(VSIn i) {
    float4x4 skin = joints[(uint)i.j.x] * i.w.x + joints[(uint)i.j.y] * i.w.y
                  + joints[(uint)i.j.z] * i.w.z + joints[(uint)i.j.w] * i.w.w;
    float4 world = mul(skin, float4(i.pos, 1));
    VSOut o;
    o.pos = mul(viewproj, world);
    o.nrm = normalize(mul((float3x3)skin, i.nrm));
    return o;
}
float4 psmain(VSOut i) : SV_Target {
    float3 n = normalize(i.nrm);
    float diff = saturate(dot(n, -light_dir.xyz));
    float3 c = base_color.rgb * (0.25 + 0.75 * diff);
    return float4(c, 1);   // 不透明(乗算済みα = そのまま)
}
"#;

fn compile(entry: &str, target: &str) -> Vec<u8> {
    let (e, t) = (CString::new(entry).unwrap(), CString::new(target).unwrap());
    let (mut code, mut err): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    let r = unsafe {
        D3DCompile(SHADER.as_ptr() as _, SHADER.len(), PCSTR::null(), None, None,
            PCSTR(e.as_ptr() as _), PCSTR(t.as_ptr() as _), 0, 0, &mut code, Some(&mut err))
    };
    if r.is_err() {
        let e = err.unwrap();
        panic!("{}", String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(e.GetBufferPointer() as *const u8, e.GetBufferSize()) }));
    }
    let c = code.unwrap();
    unsafe { std::slice::from_raw_parts(c.GetBufferPointer() as *const u8, c.GetBufferSize()).to_vec() }
}

#[repr(C)]
struct FrameCb {
    viewproj: M4,
    base_color: [f32; 4],
    light_dir: [f32; 4],
    joints: [M4; MAX_JOINTS],
}

struct Renderer {
    ctx: ID3D11DeviceContext,
    device: ID3D11Device,
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    vb: ID3D11Buffer,
    ib: ID3D11Buffer,
    cb: ID3D11Buffer,
    color: ID3D11Texture2D,
    rtv: ID3D11RenderTargetView,
    dsv: ID3D11DepthStencilView,
    raster: ID3D11RasterizerState,
    depth: ID3D11DepthStencilState,
    index_count: u32,
}

impl Renderer {
    fn new(model: &Model) -> Self {
        let (mut device, mut ctx) = (None, None);
        unsafe {
            D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_FLAG(0),
                Some(&[D3D_FEATURE_LEVEL_11_0]), D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut ctx)).unwrap();
        }
        let (device, ctx): (ID3D11Device, ID3D11DeviceContext) = (device.unwrap(), ctx.unwrap());
        let vsb = compile("vsmain", "vs_5_0");
        let psb = compile("psmain", "ps_5_0");
        let (mut vs, mut ps, mut layout) = (None, None, None);
        let el = |name: &'static [u8], fmt, off| D3D11_INPUT_ELEMENT_DESC {
            SemanticName: PCSTR(name.as_ptr()), SemanticIndex: 0, Format: fmt, InputSlot: 0,
            AlignedByteOffset: off, InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA, InstanceDataStepRate: 0,
        };
        let elems = [
            el(b"POSITION\0", DXGI_FORMAT_R32G32B32_FLOAT, 0),
            el(b"NORMAL\0", DXGI_FORMAT_R32G32B32_FLOAT, 12),
            el(b"BLENDINDICES\0", DXGI_FORMAT_R32G32B32A32_FLOAT, 24),
            el(b"BLENDWEIGHT\0", DXGI_FORMAT_R32G32B32A32_FLOAT, 40),
        ];
        unsafe {
            device.CreateVertexShader(&vsb, None, Some(&mut vs)).unwrap();
            device.CreatePixelShader(&psb, None, Some(&mut ps)).unwrap();
            device.CreateInputLayout(&elems, &vsb, Some(&mut layout)).unwrap();
        }
        let buffer = |bytes: &[u8], bind: D3D11_BIND_FLAG| {
            let d = D3D11_BUFFER_DESC { ByteWidth: bytes.len() as u32, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: bind.0 as u32, ..Default::default() };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: bytes.as_ptr() as _, ..Default::default() };
            let mut b = None;
            unsafe { device.CreateBuffer(&d, Some(&init), Some(&mut b)) }.unwrap();
            b.unwrap()
        };
        let as_bytes = |v: &[f32]| unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 4).to_vec() };
        let vb = buffer(&as_bytes(&model.vertices), D3D11_BIND_VERTEX_BUFFER);
        let ib_bytes = unsafe { std::slice::from_raw_parts(model.indices.as_ptr() as *const u8, model.indices.len() * 4) };
        let ib = buffer(ib_bytes, D3D11_BIND_INDEX_BUFFER);
        let cbd = D3D11_BUFFER_DESC {
            ByteWidth: std::mem::size_of::<FrameCb>() as u32, Usage: D3D11_USAGE_DYNAMIC,
            BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32, CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32, ..Default::default()
        };
        let mut cb = None;
        unsafe { device.CreateBuffer(&cbd, None, Some(&mut cb)) }.unwrap();

        let tex = |fmt, bind: u32| {
            let d = D3D11_TEXTURE2D_DESC {
                Width: W, Height: H, MipLevels: 1, ArraySize: 1, Format: fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_DEFAULT, BindFlags: bind, ..Default::default()
            };
            let mut t = None;
            unsafe { device.CreateTexture2D(&d, None, Some(&mut t)) }.unwrap();
            t.unwrap()
        };
        let color = tex(DXGI_FORMAT_R16G16B16A16_FLOAT, (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32);
        let depth_tex = tex(DXGI_FORMAT_D32_FLOAT, D3D11_BIND_DEPTH_STENCIL.0 as u32);
        let (mut rtv, mut dsv, mut raster, mut depth) = (None, None, None, None);
        unsafe {
            device.CreateRenderTargetView(&color, None, Some(&mut rtv)).unwrap();
            device.CreateDepthStencilView(&depth_tex, None, Some(&mut dsv)).unwrap();
            device.CreateRasterizerState(&D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default()
            }, Some(&mut raster)).unwrap();
            device.CreateDepthStencilState(&D3D11_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(), DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ALL, DepthFunc: D3D11_COMPARISON_LESS, ..Default::default()
            }, Some(&mut depth)).unwrap();
        }
        Self {
            ctx, device, vs: vs.unwrap(), ps: ps.unwrap(), layout: layout.unwrap(), vb, ib, cb: cb.unwrap(), color,
            rtv: rtv.unwrap(), dsv: dsv.unwrap(), raster: raster.unwrap(), depth: depth.unwrap(),
            index_count: model.indices.len() as u32,
        }
    }

    fn draw(&self, model: &Model, t: f32) {
        let mut cb = FrameCb {
            viewproj: mul(&perspective(40f32.to_radians(), W as f32 / H as f32, 0.1, 100.0), &look_at([0.0, 1.2, 5.0], [0.0, 1.0, 0.0])),
            base_color: model.color,
            light_dir: [-0.4, -0.7, -0.6, 0.0],
            joints: [[0.0; 16]; MAX_JOINTS],
        };
        for (i, m) in model.joint_matrices(t).into_iter().enumerate().take(MAX_JOINTS) {
            cb.joints[i] = m;
        }
        unsafe {
            let ctx = &self.ctx;
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&self.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).unwrap();
            std::ptr::copy_nonoverlapping(&cb as *const FrameCb as *const u8, m.pData as *mut u8, std::mem::size_of::<FrameCb>());
            ctx.Unmap(&self.cb, 0);
            ctx.ClearRenderTargetView(&self.rtv, &[0.0; 4]);
            ctx.ClearDepthStencilView(&self.dsv, D3D11_CLEAR_DEPTH.0 as u32, 1.0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(self.rtv.clone())]), &self.dsv);
            ctx.OMSetDepthStencilState(&self.depth, 0);
            ctx.RSSetState(&self.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: W as f32, Height: H as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetInputLayout(&self.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            let stride = 14 * 4u32;
            let offset = 0u32;
            ctx.IASetVertexBuffers(0, 1, Some(&Some(self.vb.clone())), Some(&stride), Some(&offset));
            ctx.IASetIndexBuffer(&self.ib, DXGI_FORMAT_R32_UINT, 0);
            ctx.VSSetShader(&self.vs, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.PSSetShader(&self.ps, None);
            ctx.PSSetConstantBuffers(0, Some(&[Some(self.cb.clone())]));
            ctx.DrawIndexed(self.index_count, 0, 0);
        }
    }

    fn wait(&self) {
        let mut q = None;
        unsafe {
            self.device.CreateQuery(&D3D11_QUERY_DESC { Query: D3D11_QUERY_EVENT, MiscFlags: 0 }, Some(&mut q)).unwrap();
            let q: ID3D11Query = q.unwrap();
            self.ctx.End(&q);
            let mut done: i32 = 0;
            while self.ctx.GetData(&q, Some(&mut done as *mut _ as _), 4, 0) != Ok(()) || done == 0 {}
        }
    }

    /// 描画結果を RGBA(f32、乗算済み)で読み戻す。
    fn read(&self) -> Vec<f32> {
        unsafe {
            let d = D3D11_TEXTURE2D_DESC {
                Width: W, Height: H, MipLevels: 1, ArraySize: 1, Format: DXGI_FORMAT_R16G16B16A16_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 }, Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32, ..Default::default()
            };
            let mut st = None;
            self.device.CreateTexture2D(&d, None, Some(&mut st)).unwrap();
            let st: ID3D11Texture2D = st.unwrap();
            self.ctx.CopyResource(&st, &self.color);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            self.ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
            let mut out = Vec::with_capacity((W * H * 4) as usize);
            for y in 0..H as usize {
                let row = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize) as *const u16, W as usize * 4);
                out.extend(row.iter().map(|h| half_to_f32(*h)));
            }
            self.ctx.Unmap(&st, 0);
            out
        }
    }
}

fn half_to_f32(h: u16) -> f32 {
    let s = ((h >> 15) & 1) as u32;
    let e = ((h >> 10) & 0x1f) as u32;
    let f = (h & 0x3ff) as u32;
    let bits = if e == 0 {
        if f == 0 { s << 31 } else {
            // 非正規化数
            let mut e = 127 - 15 + 1;
            let mut f = f;
            while f & 0x400 == 0 { f <<= 1; e -= 1; }
            (s << 31) | (e << 23) | ((f & 0x3ff) << 13)
        }
    } else if e == 31 {
        (s << 31) | 0x7f80_0000 | (f << 13)
    } else {
        (s << 31) | ((e + 127 - 15) << 23) | (f << 13)
    };
    f32::from_bits(bits)
}

/// 背景(グラデーション+格子)に、乗算済みの前景を合成した 8bit RGB を返す。
fn composite(fg: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let grid = if x % 120 < 3 || y % 120 < 3 { 0.35 } else { 0.0 };
            let bg = [0.15 + 0.3 * y as f32 / H as f32 + grid, 0.2 + grid, 0.3 + 0.2 * x as f32 / W as f32 + grid];
            let a = fg[i + 3];
            for c in 0..3 {
                out.push(((fg[i + c] + bg[c] * (1.0 - a)).clamp(0.0, 1.0) * 255.0) as u8);
            }
        }
    }
    out
}

/// 描画された画素(α>0.5)の最上端の y と、左右の中心 x。
fn top_and_center(fg: &[f32]) -> (u32, f32) {
    let mut top = H;
    let (mut sx, mut n) = (0.0, 0.0);
    for y in 0..H {
        for x in 0..W {
            if fg[((y * W + x) * 4 + 3) as usize] > 0.5 {
                top = top.min(y);
                sx += x as f32;
                n += 1.0;
            }
        }
    }
    (top, if n > 0.0 { sx / n } else { f32::NAN })
}

fn main() {
    let path = std::env::args().nth(1).unwrap_or("assets/bend_light.gltf".into());
    let out_dir = std::env::args().nth(2).unwrap_or("out".into());
    std::fs::create_dir_all(&out_dir).ok();
    let t0 = Instant::now();
    let model = load(&path);
    println!(
        "[読み込み] {}: 頂点 {} / 三角形 {} / 骨 {} / アニメーションのチャンネル {}({:.0} ms)",
        path, model.vertices.len() / 14, model.indices.len() / 3, model.joints.len(), model.channels.len(),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    let r = Renderer::new(&model);
    for t in [0.0f32, 0.5, 1.0] {
        r.draw(&model, t);
        let fg = r.read();
        let (top, cx) = top_and_center(&fg);
        let name = format!("{}/{}_t{:.1}.png", out_dir, std::path::Path::new(&path).file_stem().unwrap().to_string_lossy(), t);
        image::save_buffer(&name, &composite(&fg), W, H, image::ExtendedColorType::Rgb8).unwrap();
        println!("[時刻 {t:.1} 秒] 上端 y = {top}px、中心 x = {cx:.0}px → {name}");
    }
    // 速度
    for i in 0..10 { r.draw(&model, i as f32 / 30.0); }
    r.wait();
    let n = 200;
    let t = Instant::now();
    for i in 0..n { r.draw(&model, i as f32 / 30.0); }
    r.wait();
    let ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
    println!("[描画速度] 1920x1080: 1 回 {:.3} ms(骨の計算と転送を含む。60fps の予算の {:.1}%)", ms, ms / 16.67 * 100.0);
}
