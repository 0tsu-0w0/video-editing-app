//! EffectSpec: AI が返す構造化データと、その検証。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA_VERSION: &str = "1.0";
pub const MAX_PARAMS: usize = 24;
pub const MAX_SHADERS: usize = 4;
pub const MAX_PASSES: usize = 6;
pub const MAX_HLSL_BYTES: usize = 64 * 1024;
pub const MAX_LABEL_CHARS: usize = 24;
pub const MAX_NAME_CHARS: usize = 40;

/// Forge が定数バッファの先頭に必ず並べる値(順序固定)。
pub const BUILTIN_CONSTANTS: [&str; 6] =
    ["fg_res_x", "fg_res_y", "time", "progress", "frame", "seed"];

/// シェーダ内で予約されている識別子(パラメータIDに使えない)。
const RESERVED_IDS: [&str; 12] = [
    "time", "progress", "frame", "seed", "resolution", "smp", "tex0", "tex1", "tex2", "tex3",
    "pos", "uv",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EffectSpec {
    pub schema_version: String,
    pub meta: Meta,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Param>,
    #[serde(default)]
    pub shaders: Vec<Shader>,
    #[serde(default)]
    pub passes: Vec<Pass>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Param {
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: ParamKind,
}

fn default_step() -> f64 {
    0.01
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ParamKind {
    Float {
        default: f64,
        min: f64,
        max: f64,
        #[serde(default = "default_step")]
        step: f64,
    },
    Int {
        default: i64,
        min: i64,
        max: i64,
    },
    Bool {
        default: bool,
    },
    Color {
        default: String,
    },
    Select {
        options: Vec<String>,
        #[serde(default)]
        default: usize,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Shader {
    /// 登録名 = エントリーポイント名。
    pub name: String,
    /// 関数本体のみ(cbuffer・テクスチャ宣言は Forge が前置する)。
    pub code: String,
}

fn default_target() -> String {
    "object".into()
}
fn default_resources() -> Vec<String> {
    vec!["object".into()]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pass {
    pub shader: String,
    #[serde(default = "default_target")]
    pub target: String,
    #[serde(default = "default_resources")]
    pub resources: Vec<String>,
    #[serde(default)]
    pub sampler: Sampler,
    #[serde(default)]
    pub blend: Blend,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Sampler {
    Clip,
    #[default]
    Clamp,
    Loop,
    Mirror,
    Dot,
}

impl Sampler {
    pub fn as_str(self) -> &'static str {
        match self {
            Sampler::Clip => "clip",
            Sampler::Clamp => "clamp",
            Sampler::Loop => "loop",
            Sampler::Mirror => "mirror",
            Sampler::Dot => "dot",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Blend {
    #[default]
    Copy,
    Mask,
    Draw,
    Add,
}

impl Blend {
    pub fn as_str(self) -> &'static str {
        match self {
            Blend::Copy => "copy",
            Blend::Mask => "mask",
            Blend::Draw => "draw",
            Blend::Add => "add",
        }
    }
}

pub const TARGETS: [&str; 2] = ["object", "tempbuffer"];
pub const RESOURCES: [&str; 3] = ["object", "tempbuffer", "random"];

/// トラックバーの移動単位として許される値。
pub const STEPS: [(f64, &str); 5] =
    [(1.0, "1"), (0.1, "0.1"), (0.01, "0.01"), (0.001, "0.001"), (0.0001, "0.0001")];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

fn issue(code: &'static str, message: impl Into<String>) -> Issue {
    Issue { code, message: message.into() }
}

/// `[a-z][a-z0-9_]{0,31}`
pub fn is_valid_id(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    s.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// `[A-Za-z_][A-Za-z0-9_]{0,31}`
pub fn is_valid_shader_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.len() <= 32 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 設定項目名・選択肢として安全な文字列か。
/// ヘッダ行(`--track@変数名:項目名,…`)を壊す文字を全て拒否する。
pub fn is_safe_label(s: &str, max_chars: usize) -> bool {
    let n = s.chars().count();
    if n == 0 || n > max_chars || s.trim() != s {
        return false;
    }
    !s.chars().any(|c| {
        c.is_control()
            || matches!(c, ',' | ':' | '=' | '@' | '[' | ']' | '\\' | '"' | '\'' | '-' | '{' | '}')
            // 行区切りとして扱われうる Unicode
            || matches!(c, '\u{2028}' | '\u{2029}' | '\u{0085}' | '\u{FEFF}')
    })
}

/// ファイル名にも使うエフェクト名として安全か。
pub fn is_safe_name(s: &str) -> bool {
    is_safe_label(s, MAX_NAME_CHARS)
        && !s.chars().any(|c| matches!(c, '/' | '*' | '?' | '<' | '>' | '|' | '.'))
}

pub fn parse_color(s: &str) -> Option<u32> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(hex, 16).ok()
}

pub fn step_str(step: f64) -> Option<&'static str> {
    STEPS.iter().find(|(v, _)| (v - step).abs() < v * 1e-6).map(|(_, s)| *s)
}

impl Param {
    /// 定数バッファ上の要素名(color は3要素)。
    pub fn constant_names(&self) -> Vec<String> {
        match self.kind {
            ParamKind::Color { .. } => ["r", "g", "b"]
                .iter()
                .map(|c| format!("{}_{}", self.id, c))
                .collect(),
            _ => vec![self.id.clone()],
        }
    }

    /// 既定値を定数バッファの値(float 列)にする。
    pub fn default_values(&self) -> Vec<f32> {
        match &self.kind {
            ParamKind::Float { default, .. } => vec![*default as f32],
            ParamKind::Int { default, .. } => vec![*default as f32],
            ParamKind::Bool { default } => vec![if *default { 1.0 } else { 0.0 }],
            ParamKind::Select { default, .. } => vec![*default as f32],
            ParamKind::Color { default } => {
                let c = parse_color(default).unwrap_or(0xffffff);
                vec![
                    ((c >> 16) & 0xff) as f32 / 255.0,
                    ((c >> 8) & 0xff) as f32 / 255.0,
                    (c & 0xff) as f32 / 255.0,
                ]
            }
        }
    }
}

impl EffectSpec {
    pub fn from_json(text: &str) -> Result<Self, Issue> {
        serde_json::from_str(text).map_err(|e| issue("E_SCHEMA", format!("JSON を読めません: {e}")))
    }

    /// 定数バッファの全要素名(組み込み値 + パラメータ)。
    pub fn constant_names(&self) -> Vec<String> {
        let mut v: Vec<String> = BUILTIN_CONSTANTS.iter().map(|s| s.to_string()).collect();
        for p in &self.parameters {
            v.extend(p.constant_names());
        }
        v
    }

    /// 構造の検証。問題が無ければ空の Vec を返す。
    pub fn validate(&self) -> Vec<Issue> {
        let mut out = Vec::new();

        if self.schema_version != SCHEMA_VERSION {
            out.push(issue(
                "E_SCHEMA",
                format!("schema_version は \"{SCHEMA_VERSION}\" にしてください(現在: {:?})", self.schema_version),
            ));
        }
        if let Some(r) = &self.refusal {
            out.push(issue("E_REFUSAL", format!("AI が生成を断りました: {r}")));
            return out;
        }
        if !is_safe_name(&self.meta.name) {
            out.push(issue(
                "E_SCHEMA",
                format!(
                    "meta.name {:?} は使えません。1〜{MAX_NAME_CHARS}文字で、記号 , : = @ [ ] \\ \" ' - {{ }} / * ? < > | . と改行を含めないでください",
                    self.meta.name
                ),
            ));
        }

        // ---- parameters
        if self.parameters.len() > MAX_PARAMS {
            out.push(issue(
                "E_SCHEMA",
                format!("parameters は最大 {MAX_PARAMS} 個です(現在 {} 個)", self.parameters.len()),
            ));
        }
        let mut names: HashSet<String> = BUILTIN_CONSTANTS.iter().map(|s| s.to_string()).collect();
        let mut labels: HashSet<&str> = HashSet::new();
        for p in &self.parameters {
            let at = format!("parameters[id={:?}]", p.id);
            if !is_valid_id(&p.id) {
                out.push(issue("E_SCHEMA", format!("{at}: id は [a-z][a-z0-9_]{{0,31}} にしてください")));
                continue;
            }
            if p.id.starts_with("fg_") || p.id.starts_with("p_") || RESERVED_IDS.contains(&p.id.as_str()) {
                out.push(issue("E_SCHEMA", format!("{at}: この id は予約されています。別の名前にしてください")));
            }
            for n in p.constant_names() {
                if !names.insert(n.clone()) {
                    out.push(issue("E_SCHEMA", format!("{at}: 名前 {n:?} が他と重複しています")));
                }
            }
            if !is_safe_label(&p.label, MAX_LABEL_CHARS) {
                out.push(issue(
                    "E_SCHEMA",
                    format!(
                        "{at}: label {:?} は使えません。1〜{MAX_LABEL_CHARS}文字で、記号 , : = @ [ ] \\ \" ' - {{ }} と改行を含めないでください",
                        p.label
                    ),
                ));
            } else if !labels.insert(p.label.as_str()) {
                out.push(issue("E_SCHEMA", format!("{at}: label {:?} が他と重複しています", p.label)));
            }
            match &p.kind {
                ParamKind::Float { default, min, max, step } => {
                    if ![*default, *min, *max, *step].iter().all(|v| v.is_finite()) {
                        out.push(issue("E_SCHEMA", format!("{at}: 数値が有限ではありません")));
                    } else if min >= max {
                        out.push(issue("E_SCHEMA", format!("{at}: min < max にしてください")));
                    } else if default < min || default > max {
                        out.push(issue("E_SCHEMA", format!("{at}: default が min〜max の範囲外です")));
                    } else if min.abs() > 1e6 || max.abs() > 1e6 {
                        out.push(issue("E_SCHEMA", format!("{at}: min/max は ±1000000 以内にしてください")));
                    }
                    if step_str(*step).is_none() {
                        out.push(issue(
                            "E_SCHEMA",
                            format!("{at}: step は 1, 0.1, 0.01, 0.001, 0.0001 のいずれかにしてください"),
                        ));
                    }
                }
                ParamKind::Int { default, min, max } => {
                    if min >= max {
                        out.push(issue("E_SCHEMA", format!("{at}: min < max にしてください")));
                    } else if default < min || default > max {
                        out.push(issue("E_SCHEMA", format!("{at}: default が min〜max の範囲外です")));
                    } else if min.abs() > 1_000_000 || max.abs() > 1_000_000 {
                        out.push(issue("E_SCHEMA", format!("{at}: min/max は ±1000000 以内にしてください")));
                    }
                }
                ParamKind::Bool { .. } => {}
                ParamKind::Color { default } => {
                    if parse_color(default).is_none() {
                        out.push(issue("E_SCHEMA", format!("{at}: color の default は \"#RRGGBB\" にしてください")));
                    }
                }
                ParamKind::Select { options, default } => {
                    if options.len() < 2 || options.len() > 16 {
                        out.push(issue("E_SCHEMA", format!("{at}: options は 2〜16 個にしてください")));
                    }
                    if *default >= options.len().max(1) {
                        out.push(issue("E_SCHEMA", format!("{at}: default は options の添字(0始まり)にしてください")));
                    }
                    let mut seen = HashSet::new();
                    for o in options {
                        if !is_safe_label(o, MAX_LABEL_CHARS) {
                            out.push(issue("E_SCHEMA", format!("{at}: 選択肢 {o:?} は使えない文字を含みます")));
                        } else if !seen.insert(o) {
                            out.push(issue("E_SCHEMA", format!("{at}: 選択肢 {o:?} が重複しています")));
                        }
                    }
                }
            }
        }

        // ---- shaders
        if self.shaders.is_empty() || self.shaders.len() > MAX_SHADERS {
            out.push(issue("E_SCHEMA", format!("shaders は 1〜{MAX_SHADERS} 個にしてください")));
        }
        let mut shader_names = HashSet::new();
        for s in &self.shaders {
            if !is_valid_shader_name(&s.name) {
                out.push(issue("E_SCHEMA", format!("shaders[name={:?}]: name は英数字と _ のみ(32文字以内)にしてください", s.name)));
            } else if s.name.starts_with("fg_") || names.contains(&s.name) {
                out.push(issue("E_SCHEMA", format!("shaders[name={:?}]: この name は予約済みか、パラメータと重複しています", s.name)));
            } else if !shader_names.insert(s.name.as_str()) {
                out.push(issue("E_SCHEMA", format!("shaders[name={:?}]: name が重複しています", s.name)));
            }
            if s.code.len() > MAX_HLSL_BYTES {
                out.push(issue("E_SCHEMA", format!("shaders[name={:?}]: code が大きすぎます(最大 {MAX_HLSL_BYTES} バイト)", s.name)));
            }
        }

        // ---- passes
        if self.passes.is_empty() || self.passes.len() > MAX_PASSES {
            out.push(issue("E_SCHEMA", format!("passes は 1〜{MAX_PASSES} 個にしてください")));
        }
        let mut temp_written = false;
        for (i, p) in self.passes.iter().enumerate() {
            let at = format!("passes[{i}]");
            if !shader_names.contains(p.shader.as_str()) {
                out.push(issue("E_SCHEMA", format!("{at}: shader {:?} が shaders にありません", p.shader)));
            }
            if !TARGETS.contains(&p.target.as_str()) {
                out.push(issue("E_SCHEMA", format!("{at}: target は \"object\" か \"tempbuffer\" にしてください")));
            }
            if p.resources.is_empty() || p.resources.len() > 4 {
                out.push(issue("E_SCHEMA", format!("{at}: resources は 1〜4 個にしてください")));
            }
            for r in &p.resources {
                if !RESOURCES.contains(&r.as_str()) {
                    out.push(issue(
                        "E_SCHEMA",
                        format!("{at}: resources は \"object\" \"tempbuffer\" \"random\" のいずれかにしてください({r:?})"),
                    ));
                }
                if r == "tempbuffer" && !temp_written {
                    out.push(issue(
                        "E_SCHEMA",
                        format!("{at}: tempbuffer を読む前に、先のパスで tempbuffer へ書き込んでください"),
                    ));
                }
            }
            if p.target == "tempbuffer" {
                temp_written = true;
            }
        }
        if let Some(last) = self.passes.last() {
            if last.target != "object" {
                out.push(issue("E_SCHEMA", "最後のパスの target は \"object\" にしてください"));
            }
        }
        out
    }
}
