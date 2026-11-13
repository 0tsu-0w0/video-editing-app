//! 3D モデル(glTF / GLB / VRM)の読み込みと描画。
//!
//! モデルはクリップごとに、プロジェクトと同じ大きさの画像へ描いてから、ふつうのレイヤーとして重ねる。
//! そのため位置・拡大・画面上の回転・不透明度・四隅の貼り付けは、画像と同じように効く。
//! 3D の向き(rot_x / rot_y)とアニメーション(骨・ノードの動き)は、モデルを描くときに反映する。
//!
//! 大きさ: 拡大率 1 で、モデル全体を囲む球が画面の高さの約 85% に収まるようにカメラを置く。

use crate::gpu::{compile_src, Gpu, Texture};
use std::path::Path;
use studio_core::AssetKind;
use windows::core::PCSTR;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const MAX_JOINTS: usize = 256;
/// 頂点: 位置 3、法線 3、uv 2、骨の番号 4、骨の重み 4
const STRIDE: usize = 16;
const FOV_Y: f32 = 30.0;

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

const IDENTITY: M4 = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn translate(t: [f32; 3]) -> M4 {
    let mut m = IDENTITY;
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

fn trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> M4 {
    let [x, y, z, w] = q;
    let mut m = [
        1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w), 0.0,
        2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w), 0.0,
        2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y), 0.0,
        t[0], t[1], t[2], 1.0,
    ];
    for c in 0..3 {
        for row in 0..3 {
            m[c * 4 + row] *= s[c];
        }
    }
    m
}

fn rot_x(deg: f32) -> M4 {
    let (s, c) = deg.to_radians().sin_cos();
    [1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0]
}

fn rot_y(deg: f32) -> M4 {
    let (s, c) = deg.to_radians().sin_cos();
    [c, 0.0, -s, 0.0, 0.0, 1.0, 0.0, 0.0, s, 0.0, c, 0.0, 0.0, 0.0, 0.0, 1.0]
}

fn perspective(fovy: f32, aspect: f32, near: f32, far: f32) -> M4 {
    // D3D の深度 0..1、右手系(カメラは -Z を向く)
    let f = 1.0 / (fovy / 2.0).tan();
    [f / aspect, 0.0, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 0.0, far / (near - far), -1.0, 0.0, 0.0, near * far / (near - far), 0.0]
}

fn transform(m: &M4, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
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
    let q = [0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb);
    let l = q.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
    q.map(|v| v / l)
}

/// モデルとして読めるファイルか(拡張子で判断)。
pub fn is_model_path(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "gltf" | "glb" | "vrm"))
}

/// モデルを読んで、素材の種類(アニメーションの長さ)を調べる。
pub fn probe(path: &Path) -> Result<AssetKind, String> {
    let m = ModelData::load(path)?;
    Ok(AssetKind::Model { animation: (m.anim_len as f64 * 1000.0).round() / 1000.0 })
}

struct Node {
    parent: Option<usize>,
    t: [f32; 3],
    r: [f32; 4],
    s: [f32; 3],
}

enum Values {
    T(Vec<[f32; 3]>),
    R(Vec<[f32; 4]>),
    S(Vec<[f32; 3]>),
}

struct Channel {
    node: usize,
    times: Vec<f32>,
    values: Values,
    step: bool,
}

struct Skin {
    joints: Vec<usize>,
    ibm: Vec<M4>,
}

struct PrimData {
    node: usize,
    skin: Option<usize>,
    vertices: Vec<f32>,
    indices: Vec<u32>,
    color: [f32; 4],
    image: Option<usize>,
    /// 透明の扱い: 負なら不透明、そうでなければこの値未満の画素を描かない
    cutoff: f32,
}

/// GPU に依存しない、読み込んだモデル。
pub struct ModelData {
    nodes: Vec<Node>,
    skins: Vec<Skin>,
    channels: Vec<Channel>,
    prims: Vec<PrimData>,
    /// 画像(BGRA)
    images: Vec<Option<(u32, u32, Vec<u8>)>>,
    pub anim_len: f32,
    /// 静止時のモデルを囲む球(中心と半径)
    pub center: [f32; 3],
    pub radius: f32,
}

fn to_m4(m: [[f32; 4]; 4]) -> M4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = m[c][row];
        }
    }
    r
}

impl ModelData {
    pub fn load(path: &Path) -> Result<ModelData, String> {
        let (doc, buffers, images) = gltf::import(path).map_err(|e| format!("3D モデルを読めません: {e}"))?;
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
        let skins: Vec<Skin> = doc
            .skins()
            .map(|s| {
                let joints: Vec<usize> = s.joints().map(|j| j.index()).collect();
                let ibm: Vec<M4> = s.reader(|b| Some(&buffers[b.index()])).read_inverse_bind_matrices().map(|it| it.map(to_m4).collect()).unwrap_or_else(|| vec![IDENTITY; joints.len()]);
                Skin { joints, ibm }
            })
            .collect();
        let mut channels = Vec::new();
        let mut anim_len = 0.0f32;
        if let Some(anim) = doc.animations().next() {
            use gltf::animation::util::ReadOutputs;
            for ch in anim.channels() {
                let rd = ch.reader(|b| Some(&buffers[b.index()]));
                let Some(times) = rd.read_inputs().map(|i| i.collect::<Vec<f32>>()) else { continue };
                let interp = ch.sampler().interpolation();
                let cubic = interp == gltf::animation::Interpolation::CubicSpline;
                // 3 次スプラインは (入りの接線, 値, 出の接線) の組なので、値だけを使う
                fn pick<T: Copy>(v: Vec<T>, cubic: bool) -> Vec<T> {
                    if cubic { v.chunks(3).filter(|c| c.len() == 3).map(|c| c[1]).collect() } else { v }
                }
                let values = match rd.read_outputs() {
                    Some(ReadOutputs::Translations(t)) => Values::T(pick(t.collect(), cubic)),
                    Some(ReadOutputs::Rotations(r)) => Values::R(pick(r.into_f32().collect(), cubic)),
                    Some(ReadOutputs::Scales(s)) => Values::S(pick(s.collect(), cubic)),
                    _ => continue,
                };
                let n = match &values {
                    Values::T(v) | Values::S(v) => v.len(),
                    Values::R(v) => v.len(),
                };
                if times.is_empty() || n != times.len() {
                    continue;
                }
                anim_len = anim_len.max(*times.last().unwrap());
                channels.push(Channel { node: ch.target().node().index(), times, values, step: interp == gltf::animation::Interpolation::Step });
            }
        }
        let mut prims = Vec::new();
        for node in doc.nodes() {
            let Some(mesh) = node.mesh() else { continue };
            for prim in mesh.primitives() {
                if prim.mode() != gltf::mesh::Mode::Triangles {
                    continue;
                }
                let rd = prim.reader(|b| Some(&buffers[b.index()]));
                let Some(pos) = rd.read_positions().map(|p| p.collect::<Vec<[f32; 3]>>()) else { continue };
                let indices: Vec<u32> = rd.read_indices().map(|i| i.into_u32().collect()).unwrap_or_else(|| (0..pos.len() as u32).collect());
                if indices.iter().any(|&i| i as usize >= pos.len()) {
                    continue;
                }
                let nrm: Vec<[f32; 3]> = rd.read_normals().map(|n| n.collect()).unwrap_or_else(|| vertex_normals(&pos, &indices));
                let uv: Vec<[f32; 2]> = rd.read_tex_coords(0).map(|t| t.into_f32().collect()).unwrap_or_else(|| vec![[0.0; 2]; pos.len()]);
                let skin = node.skin().map(|s| s.index());
                let jnt: Vec<[u16; 4]> = rd.read_joints(0).filter(|_| skin.is_some()).map(|j| j.into_u16().collect()).unwrap_or_else(|| vec![[0; 4]; pos.len()]);
                let wgt: Vec<[f32; 4]> = rd.read_weights(0).filter(|_| skin.is_some()).map(|w| w.into_f32().collect()).unwrap_or_else(|| vec![[1.0, 0.0, 0.0, 0.0]; pos.len()]);
                let mut vertices = Vec::with_capacity(pos.len() * STRIDE);
                for i in 0..pos.len() {
                    vertices.extend(pos[i]);
                    vertices.extend(nrm.get(i).copied().unwrap_or([0.0, 0.0, 1.0]));
                    vertices.extend(uv.get(i).copied().unwrap_or([0.0; 2]));
                    vertices.extend(jnt.get(i).copied().unwrap_or([0; 4]).map(|j| (j as usize).min(MAX_JOINTS - 1) as f32));
                    vertices.extend(wgt.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]));
                }
                let mat = prim.material();
                let pbr = mat.pbr_metallic_roughness();
                let cutoff = match mat.alpha_mode() {
                    gltf::material::AlphaMode::Opaque => -1.0,
                    gltf::material::AlphaMode::Mask => mat.alpha_cutoff().unwrap_or(0.5),
                    gltf::material::AlphaMode::Blend => 0.5,
                };
                prims.push(PrimData {
                    node: node.index(),
                    skin,
                    vertices,
                    indices,
                    color: pbr.base_color_factor(),
                    image: pbr.base_color_texture().map(|t| t.texture().source().index()),
                    cutoff,
                });
            }
        }
        if prims.is_empty() {
            return Err("3D モデルに描ける形(三角形のメッシュ)がありません".into());
        }
        let images = images.into_iter().map(to_bgra).collect();
        let mut m = ModelData { nodes, skins, channels, prims, images, anim_len, center: [0.0; 3], radius: 1.0 };
        // 静止時の形から、囲む箱を求める
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        let globals = m.globals(None);
        for p in &m.prims {
            let palette = m.palette(p, &globals);
            for v in p.vertices.chunks_exact(STRIDE) {
                let q = skin_point(&palette, v);
                for k in 0..3 {
                    lo[k] = lo[k].min(q[k]);
                    hi[k] = hi[k].max(q[k]);
                }
            }
        }
        m.center = [0, 1, 2].map(|k| (lo[k] + hi[k]) / 2.0);
        m.radius = ((0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f32>().sqrt() / 2.0).max(1e-4);
        if !m.radius.is_finite() || m.center.iter().any(|v| !v.is_finite()) {
            return Err("3D モデルの大きさを求められません".into());
        }
        Ok(m)
    }

    /// 時刻 t(秒、None なら静止時)の各ノードのグローバル行列。
    fn globals(&self, t: Option<f32>) -> Vec<M4> {
        let mut tr: Vec<([f32; 3], [f32; 4], [f32; 3])> = self.nodes.iter().map(|n| (n.t, n.r, n.s)).collect();
        if let (Some(t), true) = (t, self.anim_len > 0.0) {
            let t = t.rem_euclid(self.anim_len);
            for ch in &self.channels {
                let ts = &ch.times;
                let k = ts.partition_point(|x| *x <= t);
                let (i, j, f) = if k == 0 {
                    (0, 0, 0.0)
                } else if k >= ts.len() {
                    (ts.len() - 1, ts.len() - 1, 0.0)
                } else {
                    let f = if ch.step { 0.0 } else { (t - ts[k - 1]) / (ts[k] - ts[k - 1]).max(1e-9) };
                    (k - 1, k, f)
                };
                let lerp3 = |a: [f32; 3], b: [f32; 3]| [0, 1, 2].map(|q| a[q] + (b[q] - a[q]) * f);
                let n = &mut tr[ch.node];
                match &ch.values {
                    Values::T(v) => n.0 = lerp3(v[i], v[j]),
                    Values::R(v) => n.1 = slerp(v[i], v[j], f),
                    Values::S(v) => n.2 = lerp3(v[i], v[j]),
                }
            }
        }
        let local: Vec<M4> = tr.iter().map(|(t, r, s)| trs(*t, *r, *s)).collect();
        let mut out: Vec<Option<M4>> = vec![None; self.nodes.len()];
        fn get(i: usize, nodes: &[Node], local: &[M4], out: &mut Vec<Option<M4>>, depth: usize) -> M4 {
            if let Some(m) = out[i] {
                return m;
            }
            let m = match nodes[i].parent {
                Some(p) if depth < 256 => mul(&get(p, nodes, local, out, depth + 1), &local[i]),
                _ => local[i],
            };
            out[i] = Some(m);
            m
        }
        (0..self.nodes.len()).map(|i| get(i, &self.nodes, &local, &mut out, 0)).collect()
    }

    /// 1 つの形を描くための骨の行列(骨が無ければ、ノードの行列 1 つ)。
    fn palette(&self, p: &PrimData, globals: &[M4]) -> Vec<M4> {
        match p.skin.and_then(|s| self.skins.get(s)) {
            Some(s) => s.joints.iter().zip(&s.ibm).take(MAX_JOINTS).map(|(j, ibm)| mul(&globals[*j], ibm)).collect(),
            None => vec![globals[p.node]],
        }
    }
}

fn skin_point(palette: &[M4], v: &[f32]) -> [f32; 3] {
    let p = [v[0], v[1], v[2]];
    let mut q = [0.0; 3];
    for k in 0..4 {
        let w = v[12 + k];
        if w != 0.0 {
            let m = palette.get(v[8 + k] as usize).unwrap_or(&palette[0]);
            let r = transform(m, p);
            for c in 0..3 {
                q[c] += r[c] * w;
            }
        }
    }
    q
}

fn vertex_normals(pos: &[[f32; 3]], idx: &[u32]) -> Vec<[f32; 3]> {
    let mut n = vec![[0.0f32; 3]; pos.len()];
    for t in idx.chunks_exact(3) {
        let (a, b, c) = (pos[t[0] as usize], pos[t[1] as usize], pos[t[2] as usize]);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let f = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        for &i in t {
            for k in 0..3 {
                n[i as usize][k] += f[k];
            }
        }
    }
    n.into_iter()
        .map(|v| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if l > 0.0 { v.map(|x| x / l) } else { [0.0, 0.0, 1.0] }
        })
        .collect()
}

fn to_bgra(img: gltf::image::Data) -> Option<(u32, u32, Vec<u8>)> {
    use gltf::image::Format::*;
    let n = (img.width * img.height) as usize;
    let px = &img.pixels;
    let out: Vec<u8> = match img.format {
        R8G8B8A8 => px.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect(),
        R8G8B8 => px.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
        R8G8 => px.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        R8 => px.iter().flat_map(|&p| [p, p, p, 255]).collect(),
        _ => return None,
    };
    (out.len() == n * 4 && n > 0).then_some((img.width, img.height, out))
}

const SHADER: &str = r#"
cbuffer Frame : register(b0) {
    float4x4 viewproj;
    float4x4 model;
    float4 base_color;
    float4 light_dir;   // xyz: 光の進む向き、w: 透明の閾値(負なら不透明)
    float4 has_tex;     // x: 1 なら画像を使う
    float4x4 joints[256];
};
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct VSIn { float3 pos : POSITION; float3 nrm : NORMAL; float2 uv : TEXCOORD0; float4 j : BLENDINDICES; float4 w : BLENDWEIGHT; };
struct VSOut { float4 pos : SV_Position; float3 nrm : NORMAL; float2 uv : TEXCOORD0; };
VSOut vsmain(VSIn i) {
    float4x4 skin = joints[(uint)i.j.x] * i.w.x + joints[(uint)i.j.y] * i.w.y
                  + joints[(uint)i.j.z] * i.w.z + joints[(uint)i.j.w] * i.w.w;
    float4 world = mul(model, mul(skin, float4(i.pos, 1)));
    VSOut o;
    o.pos = mul(viewproj, world);
    o.nrm = mul((float3x3)model, mul((float3x3)skin, i.nrm));
    o.uv = i.uv;
    return o;
}
float4 psmain(VSOut i, bool front : SV_IsFrontFace) : SV_Target {
    float4 c = base_color;
    if (has_tex.x > 0.5) c *= tex.Sample(smp, i.uv);
    if (light_dir.w >= 0 && c.a < light_dir.w) discard;
    float3 n = normalize(i.nrm);
    if (!front) n = -n;
    float diff = saturate(dot(n, -light_dir.xyz));
    // 半分は環境光(暗くなりすぎないように)、半分は光の当たり方
    return float4(c.rgb * (0.5 + 0.6 * diff), 1);
}
"#;

#[repr(C)]
struct FrameCb {
    viewproj: M4,
    model: M4,
    base_color: [f32; 4],
    light_dir: [f32; 4],
    has_tex: [f32; 4],
    joints: [M4; MAX_JOINTS],
}

struct Prim {
    vb: ID3D11Buffer,
    ib: ID3D11Buffer,
    count: u32,
}

/// GPU に載せたモデル。
pub struct Model {
    data: ModelData,
    prims: Vec<Prim>,
    textures: Vec<Option<Texture>>,
    target: Option<(Texture, ID3D11RenderTargetView, ID3D11DepthStencilView)>,
}

/// モデルを描くための、共通のシェーダなど。
pub struct ModelPipeline {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    cb: ID3D11Buffer,
    raster: ID3D11RasterizerState,
    depth: ID3D11DepthStencilState,
    sampler: ID3D11SamplerState,
}

fn e<T: std::fmt::Display>(what: &'static str) -> impl Fn(T) -> String {
    move |x| format!("{what}: {x}")
}

impl ModelPipeline {
    pub fn new(gpu: &Gpu) -> Result<Self, String> {
        let d = &gpu.device;
        let vsb = compile_src(SHADER, "vsmain", "vs_5_0")?;
        let psb = compile_src(SHADER, "psmain", "ps_5_0")?;
        let el = |name: &'static [u8], fmt, off| D3D11_INPUT_ELEMENT_DESC {
            SemanticName: PCSTR(name.as_ptr()),
            SemanticIndex: 0,
            Format: fmt,
            InputSlot: 0,
            AlignedByteOffset: off,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        };
        let elems = [
            el(b"POSITION\0", DXGI_FORMAT_R32G32B32_FLOAT, 0),
            el(b"NORMAL\0", DXGI_FORMAT_R32G32B32_FLOAT, 12),
            el(b"TEXCOORD\0", DXGI_FORMAT_R32G32_FLOAT, 24),
            el(b"BLENDINDICES\0", DXGI_FORMAT_R32G32B32A32_FLOAT, 32),
            el(b"BLENDWEIGHT\0", DXGI_FORMAT_R32G32B32A32_FLOAT, 48),
        ];
        let (mut vs, mut ps, mut layout, mut cb, mut raster, mut depth, mut sampler) = (None, None, None, None, None, None, None);
        unsafe {
            d.CreateVertexShader(&vsb, None, Some(&mut vs)).map_err(e("3D 頂点シェーダ"))?;
            d.CreatePixelShader(&psb, None, Some(&mut ps)).map_err(e("3D ピクセルシェーダ"))?;
            d.CreateInputLayout(&elems, &vsb, Some(&mut layout)).map_err(e("頂点の形式"))?;
            d.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: std::mem::size_of::<FrameCb>() as u32,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut cb),
            )
            .map_err(e("定数バッファ"))?;
            d.CreateRasterizerState(&D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() }, Some(&mut raster))
                .map_err(e("ラスタライザ"))?;
            d.CreateDepthStencilState(
                &D3D11_DEPTH_STENCIL_DESC { DepthEnable: true.into(), DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ALL, DepthFunc: D3D11_COMPARISON_LESS, ..Default::default() },
                Some(&mut depth),
            )
            .map_err(e("深度"))?;
            d.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_WRAP,
                    AddressV: D3D11_TEXTURE_ADDRESS_WRAP,
                    AddressW: D3D11_TEXTURE_ADDRESS_WRAP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )
            .map_err(e("サンプラ"))?;
        }
        Ok(Self { vs: vs.unwrap(), ps: ps.unwrap(), layout: layout.unwrap(), cb: cb.unwrap(), raster: raster.unwrap(), depth: depth.unwrap(), sampler: sampler.unwrap() })
    }
}

impl Model {
    pub fn load(gpu: &Gpu, path: &Path) -> Result<Model, String> {
        let data = ModelData::load(path)?;
        let d = &gpu.device;
        let buffer = |bytes: &[u8], bind: D3D11_BIND_FLAG| -> Result<ID3D11Buffer, String> {
            let desc = D3D11_BUFFER_DESC { ByteWidth: bytes.len() as u32, Usage: D3D11_USAGE_IMMUTABLE, BindFlags: bind.0 as u32, ..Default::default() };
            let init = D3D11_SUBRESOURCE_DATA { pSysMem: bytes.as_ptr() as _, ..Default::default() };
            let mut b = None;
            unsafe { d.CreateBuffer(&desc, Some(&init), Some(&mut b)) }.map_err(e("頂点バッファ"))?;
            Ok(b.unwrap())
        };
        let mut prims = Vec::new();
        for p in &data.prims {
            let vb = buffer(unsafe { std::slice::from_raw_parts(p.vertices.as_ptr() as *const u8, p.vertices.len() * 4) }, D3D11_BIND_VERTEX_BUFFER)?;
            let ib = buffer(unsafe { std::slice::from_raw_parts(p.indices.as_ptr() as *const u8, p.indices.len() * 4) }, D3D11_BIND_INDEX_BUFFER)?;
            prims.push(Prim { vb, ib, count: p.indices.len() as u32 });
        }
        let textures = data.images.iter().map(|i| i.as_ref().and_then(|(w, h, px)| gpu.create_texture(*w, *h, Some(px), false).ok())).collect();
        Ok(Model { data, prims, textures, target: None })
    }

    /// クリップ内の時刻 `t` 秒・3D の向き(度)で、(w, h) の画像に描く。背景は透明。
    pub fn render(&mut self, gpu: &Gpu, pipe: &ModelPipeline, (w, h): (u32, u32), t: f64, rx: f64, ry: f64) -> Result<&Texture, String> {
        if self.target.as_ref().is_none_or(|(tex, _, _)| (tex.width, tex.height) != (w, h)) {
            let tex = gpu.create_texture(w, h, None, true)?;
            let rtv = gpu.rtv(&tex)?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_D32_FLOAT,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_DEPTH_STENCIL.0 as u32,
                ..Default::default()
            };
            let (mut dt, mut dsv) = (None, None);
            unsafe {
                gpu.device.CreateTexture2D(&desc, None, Some(&mut dt)).map_err(e("深度テクスチャ"))?;
                gpu.device.CreateDepthStencilView(&dt.unwrap(), None, Some(&mut dsv)).map_err(e("深度"))?;
            }
            self.target = Some((tex, rtv, dsv.unwrap()));
        }
        let m = &self.data;
        let dist = m.radius / (0.85 * (FOV_Y.to_radians() / 2.0).tan());
        let proj = perspective(FOV_Y.to_radians(), w as f32 / h as f32, (dist - m.radius * 1.5).max(dist * 0.01), dist + m.radius * 1.5);
        let viewproj = mul(&proj, &translate([0.0, 0.0, -dist]));
        let model = mul(&mul(&rot_x(rx as f32), &rot_y(ry as f32)), &translate(m.center.map(|v| -v)));
        let globals = m.globals(Some(t as f32));
        let (_, rtv, dsv) = self.target.as_ref().unwrap();
        let ctx = &gpu.ctx;
        unsafe {
            ctx.ClearRenderTargetView(rtv, &[0.0; 4]);
            ctx.ClearDepthStencilView(dsv, D3D11_CLEAR_DEPTH.0 as u32, 1.0, 0);
            ctx.OMSetRenderTargets(Some(&[Some(rtv.clone())]), dsv);
            ctx.OMSetBlendState(None, None, 0xffff_ffff);
            ctx.OMSetDepthStencilState(&pipe.depth, 0);
            ctx.RSSetState(&pipe.raster);
            ctx.RSSetViewports(Some(&[D3D11_VIEWPORT { Width: w as f32, Height: h as f32, MaxDepth: 1.0, ..Default::default() }]));
            ctx.IASetInputLayout(&pipe.layout);
            ctx.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            ctx.VSSetShader(&pipe.vs, None);
            ctx.PSSetShader(&pipe.ps, None);
            ctx.VSSetConstantBuffers(0, Some(&[Some(pipe.cb.clone())]));
            ctx.PSSetConstantBuffers(0, Some(&[Some(pipe.cb.clone())]));
            ctx.PSSetSamplers(0, Some(&[Some(pipe.sampler.clone())]));
        }
        // 光は左上手前から
        let l = [0.45f32, -0.6, -0.65];
        let ln = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
        let mut cb = Box::new(FrameCb { viewproj, model, base_color: [1.0; 4], light_dir: [0.0; 4], has_tex: [0.0; 4], joints: [IDENTITY; MAX_JOINTS] });
        for (p, g) in m.prims.iter().zip(&self.prims) {
            let palette = m.palette(p, &globals);
            cb.joints[..palette.len()].copy_from_slice(&palette);
            cb.base_color = p.color;
            cb.light_dir = [l[0] / ln, l[1] / ln, l[2] / ln, p.cutoff];
            let tex = p.image.and_then(|i| self.textures.get(i)).and_then(|t| t.as_ref());
            cb.has_tex = [if tex.is_some() { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0];
            // 骨の行列は使う分だけ送る
            let bytes = std::mem::size_of::<FrameCb>() - (MAX_JOINTS - palette.len().max(1)) * std::mem::size_of::<M4>();
            unsafe {
                let mut mp = D3D11_MAPPED_SUBRESOURCE::default();
                if ctx.Map(&pipe.cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mp)).is_err() {
                    continue;
                }
                std::ptr::copy_nonoverlapping(&*cb as *const FrameCb as *const u8, mp.pData as *mut u8, bytes);
                ctx.Unmap(&pipe.cb, 0);
                ctx.PSSetShaderResources(0, Some(&[tex.map(|t| t.srv.clone())]));
                let stride = (STRIDE * 4) as u32;
                let offset = 0u32;
                ctx.IASetVertexBuffers(0, 1, Some(&Some(g.vb.clone())), Some(&stride), Some(&offset));
                ctx.IASetIndexBuffer(&g.ib, DXGI_FORMAT_R32_UINT, 0);
                ctx.DrawIndexed(g.count, 0, 0);
            }
        }
        unsafe {
            ctx.PSSetShaderResources(0, Some(&[None]));
            ctx.OMSetRenderTargets(None, None);
            ctx.OMSetDepthStencilState(None, 0);
            ctx.IASetInputLayout(None);
        }
        Ok(&self.target.as_ref().unwrap().0)
    }
}
