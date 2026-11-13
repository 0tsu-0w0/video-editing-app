//! forge-gpu: D3D11 での HLSL コンパイルと試走(ホストの obj.pixelshader を模す)。
//!
//! ホストと同じ規約で描く:
//! - 画素は乗算済みアルファの float。
//! - 定数バッファ(b0)は float の配列。
//! - 入力は `float4 pos : SV_Position` と、任意で `float2 uv : TEXCOORD`。
//!
//! ここでの結果は一次検証であり、最終確認は AviUtl2 の実機で行う。

mod analyze;
mod d3d;

pub use analyze::{analyze, test_image, Finding};
pub use d3d::{compile, Gpu};

use forge_core::spec::{EffectSpec, Issue};
use forge_core::hlsl;
use std::collections::HashMap;

/// 乗算済みアルファの RGBA float 画像。
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    /// RGBA の順に width*height*4 個。
    pub data: Vec<f32>,
}

impl Image {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, data: vec![0.0; (width * height * 4) as usize] }
    }

    /// ストレートアルファの 8bit RGBA から作る。
    pub fn from_rgba8(width: u32, height: u32, bytes: &[u8]) -> Self {
        let mut data = Vec::with_capacity(bytes.len());
        for px in bytes.chunks_exact(4) {
            let a = px[3] as f32 / 255.0;
            data.extend([px[0] as f32 / 255.0 * a, px[1] as f32 / 255.0 * a, px[2] as f32 / 255.0 * a, a]);
        }
        Self { width, height, data }
    }

    /// ストレートアルファの 8bit RGBA にする。
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.data.len());
        for px in self.data.chunks_exact(4) {
            let a = if px[3].is_finite() { px[3].clamp(0.0, 1.0) } else { 0.0 };
            for c in &px[..3] {
                let v = if a > 1e-5 && c.is_finite() { (c / a).clamp(0.0, 1.0) } else { 0.0 };
                out.push((v * 255.0 + 0.5) as u8);
            }
            out.push((a * 255.0 + 0.5) as u8);
        }
        out
    }
}

/// 1回の描画に渡す、時間まわりの値。
#[derive(Debug, Clone, Copy)]
pub struct FrameInfo {
    pub time: f32,
    pub progress: f32,
    pub frame: f32,
    pub seed: f32,
}

impl Default for FrameInfo {
    fn default() -> Self {
        Self { time: 0.0, progress: 0.0, frame: 0.0, seed: 12345.0 }
    }
}

/// コンパイル済みのエフェクト。
pub struct Compiled {
    shaders: HashMap<String, d3d::PixelShader>,
}

fn fix_line_numbers(log: &str, prelude_lines: usize) -> String {
    // "(12,5-9): error X3004: ..." の行番号を、AI が書いたコード基準に直す。
    let mut out = String::new();
    for line in log.lines() {
        let mut fixed = line.to_string();
        if let (Some(open), Some(close)) = (line.find('('), line.find("):")) {
            if open < close {
                let inner = &line[open + 1..close];
                let num: String = inner.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = num.parse::<usize>() {
                    if n > prelude_lines {
                        fixed = format!("{}行目{}{}", n - prelude_lines, &inner[num.len()..].replace(',', " 列"), &line[close + 1..]);
                    } else {
                        fixed = format!("(自動生成部分){}", &line[close + 1..]);
                    }
                }
            }
        }
        out.push_str(&fixed);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// spec の全シェーダをコンパイルする。
pub fn compile_spec(gpu: &Gpu, spec: &EffectSpec) -> Result<Compiled, Vec<Issue>> {
    let mut shaders = HashMap::new();
    let mut issues = Vec::new();
    for s in &spec.shaders {
        let src = hlsl::full_source(spec, s);
        match compile(&src, &s.name, "ps_5_0").and_then(|code| gpu.create_pixel_shader(&code)) {
            Ok(ps) => {
                shaders.insert(s.name.clone(), ps);
            }
            Err(log) => issues.push(Issue {
                code: "E_COMPILE",
                message: format!(
                    "shaders[name={:?}] のコンパイルに失敗しました:\n{}",
                    s.name,
                    fix_line_numbers(&log, hlsl::prelude_lines(spec, &s.name))
                ),
            }),
        }
    }
    if issues.is_empty() {
        Ok(Compiled { shaders })
    } else {
        Err(issues)
    }
}

/// パラメータ値を、定数バッファの並びにする。`overrides` に無いものは既定値。
pub fn constants(
    spec: &EffectSpec,
    size: (u32, u32),
    info: FrameInfo,
    overrides: &HashMap<String, Vec<f32>>,
) -> Vec<f32> {
    let mut c = vec![size.0 as f32, size.1 as f32, info.time, info.progress, info.frame, info.seed];
    for p in &spec.parameters {
        let d = p.default_values();
        match overrides.get(&p.id) {
            Some(v) if v.len() == d.len() => c.extend(v),
            _ => c.extend(d),
        }
    }
    c
}

/// エフェクトを入力画像に適用する(全パスを実行)。
pub fn render(
    gpu: &Gpu,
    spec: &EffectSpec,
    compiled: &Compiled,
    input: &Image,
    info: FrameInfo,
    overrides: &HashMap<String, Vec<f32>>,
) -> Result<Image, String> {
    let consts = constants(spec, (input.width, input.height), info, overrides);
    let cb = gpu.create_constants(&consts)?;
    let object = gpu.create_texture(input)?;
    let temp = gpu.create_texture(input)?; // obj.copybuffer("tempbuffer","object") 相当
    let random = gpu.random_texture()?;

    for pass in &spec.passes {
        let ps = compiled.shaders.get(&pass.shader).ok_or("未コンパイルのシェーダです")?;
        let target = if pass.target == "tempbuffer" { &temp } else { &object };
        // レンダーターゲットと同じバッファを読む場合は複製する(ホストと同じ)。
        let mut copies = Vec::new();
        for r in &pass.resources {
            if *r == pass.target {
                copies.push(Some(gpu.clone_texture(target)?));
            } else {
                copies.push(None);
            }
        }
        let resources: Vec<&d3d::Texture> = pass
            .resources
            .iter()
            .zip(&copies)
            .map(|(r, c)| match (c, r.as_str()) {
                (Some(c), _) => c,
                (None, "tempbuffer") => &temp,
                (None, "random") => &random,
                _ => &object,
            })
            .collect();
        gpu.draw(ps, target, &resources, &cb, pass.blend, pass.sampler)?;
    }
    gpu.read_texture(&object)
}

/// 試走: 合成テスト画像に複数の時刻で適用し、問題を調べる。
/// 戻り値は (エラー, 警告)。
pub fn trial_run(gpu: &Gpu, spec: &EffectSpec, compiled: &Compiled) -> (Vec<Issue>, Vec<String>) {
    let input = test_image(256, 144);
    let mut outputs = Vec::new();
    for (i, t) in [0.0f32, 0.5, 1.37].iter().enumerate() {
        let info = FrameInfo { time: *t, progress: i as f32 / 2.0, frame: (*t * 30.0).floor(), seed: 12345.0 };
        match render(gpu, spec, compiled, &input, info, &HashMap::new()) {
            Ok(img) => outputs.push(img),
            Err(e) => {
                return (vec![Issue { code: "E_RENDER", message: format!("試走の描画に失敗しました: {e}") }], vec![])
            }
        }
    }
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for f in analyze(&input, &outputs) {
        if f.fatal {
            errors.push(Issue { code: f.code, message: f.message });
        } else {
            warnings.push(f.message);
        }
    }
    (errors, warnings)
}

#[cfg(test)]
mod tests;

/// 検証のどの段で失敗したか。
#[derive(Debug)]
pub struct Failure {
    pub stage: &'static str,
    pub issues: Vec<Issue>,
}

/// 検証に通ったエフェクト。
pub struct Checked {
    pub anm2: String,
    pub compiled: Compiled,
    pub warnings: Vec<String>,
}

/// 形式の検証 → 静的検査 → .anm2 生成と再検証 → コンパイル → 試走。
pub fn check_spec(gpu: &Gpu, spec: &EffectSpec) -> Result<Checked, Failure> {
    let anm2 = forge_core::emit::build(spec).map_err(|issues| {
        let stage = if issues.iter().any(|i| i.code == "E_STATIC") { "コードの静的検査" } else { "形式の検証" };
        Failure { stage, issues }
    })?;
    let compiled =
        compile_spec(gpu, spec).map_err(|issues| Failure { stage: "HLSL のコンパイル", issues })?;
    let (errors, warnings) = trial_run(gpu, spec, &compiled);
    if !errors.is_empty() {
        return Err(Failure { stage: "試走(テスト画像への適用)", issues: errors });
    }
    Ok(Checked { anm2, compiled, warnings })
}
