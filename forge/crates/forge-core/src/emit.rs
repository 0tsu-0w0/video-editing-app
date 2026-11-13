//! EffectSpec から AviUtl2 のスクリプト(.anm2)を生成する。
//!
//! Lua 部分は固定テンプレートだけで作る。AI が書いた文字列が Lua として
//! 解釈される経路を作らないこと(ラベルはヘッダ行のみ、HLSL は長いコメント内のみ)。

use crate::hlsl;
use crate::spec::{parse_color, step_str, EffectSpec, Issue, ParamKind};

pub const MENU_LABEL: &str = "Forge";

pub fn fmt_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 小数 `d` 桁に丸めて、末尾の 0 を落とす。
pub fn fmt_dec(v: f64, d: usize) -> String {
    let s = format!("{v:.d$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    if s == "-0" { "0".to_string() } else { s }
}

fn header_lines(spec: &EffectSpec) -> Vec<String> {
    let mut l = Vec::new();
    l.push(format!("--information:{} (Forge {})", spec.meta.name, env!("CARGO_PKG_VERSION")));
    l.push(format!("--label:{MENU_LABEL}"));
    l.push("--filter".to_string());
    for p in &spec.parameters {
        let var = format!("p_{}", p.id);
        match &p.kind {
            ParamKind::Float { default, min, max, step } => {
                let st = step_str(*step).unwrap_or("0.01");
                let d = st.split('.').nth(1).map_or(0, |f| f.len());
                l.push(format!(
                    "--track@{var}:{},{},{},{},{st}",
                    p.label,
                    fmt_dec(*min, d),
                    fmt_dec(*max, d),
                    fmt_dec(*default, d)
                ))
            }
            ParamKind::Int { default, min, max } => {
                l.push(format!("--track@{var}:{},{min},{max},{default},1", p.label))
            }
            ParamKind::Bool { default } => {
                l.push(format!("--check@{var}:{},{}", p.label, if *default { 1 } else { 0 }))
            }
            ParamKind::Color { default } => l.push(format!(
                "--color@{var}:{},0x{:06x}",
                p.label,
                parse_color(default).unwrap_or(0xffffff)
            )),
            ParamKind::Select { options, default } => {
                let opts: Vec<String> =
                    options.iter().enumerate().map(|(i, o)| format!("{o}={i}")).collect();
                l.push(format!("--select@{var}:{}={default},{}", p.label, opts.join(",")))
            }
        }
    }
    l
}

fn lua_lines(spec: &EffectSpec) -> Vec<String> {
    let mut l = Vec::new();
    l.push("local fg_progress = 0".to_string());
    l.push("if obj.totaltime > 0 then fg_progress = obj.time / obj.totaltime end".to_string());
    l.push("local fg_seed = obj.rand(0, 65535, 1, 0)".to_string());
    let mut items: Vec<String> =
        ["obj.w", "obj.h", "obj.time", "fg_progress", "obj.frame", "fg_seed"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    for p in &spec.parameters {
        match p.kind {
            ParamKind::Color { .. } => {
                l.push(format!("local fg_r_{0}, fg_g_{0}, fg_b_{0} = RGB(p_{0})", p.id));
                for c in ["r", "g", "b"] {
                    items.push(format!("fg_{c}_{} / 255", p.id));
                }
            }
            _ => items.push(format!("p_{}", p.id)),
        }
    }
    l.push(format!("local fg_c = {{{}}}", items.join(", ")));
    if spec.passes.iter().any(|p| p.target == "tempbuffer") {
        // 仮想バッファをオブジェクトと同じ大きさで用意する。
        l.push("obj.copybuffer(\"tempbuffer\", \"object\")".to_string());
    }
    for p in &spec.passes {
        let res: Vec<String> = p.resources.iter().map(|r| format!("\"{r}\"")).collect();
        l.push(format!(
            "obj.pixelshader(\"{}\", \"{}\", {{{}}}, fg_c, \"{}\", \"{}\")",
            p.shader,
            p.target,
            res.join(", "),
            p.blend.as_str(),
            p.sampler.as_str()
        ));
    }
    l
}

/// .anm2 の本文を生成する。事前に `spec.validate()` と `hlsl::lint()` を通すこと。
/// 生成後は必ず `verify::verify_anm2()` を通す(`build()` がまとめて行う)。
pub fn emit_anm2(spec: &EffectSpec) -> String {
    let mut lines = header_lines(spec);
    for s in &spec.shaders {
        lines.push(format!("--[[pixelshader@{}:", s.name));
        for hl in hlsl::full_source(spec, s).lines() {
            // 行頭を必ずタブにする: ホストが行頭の `--xxx@` や `@名前` を
            // 設定項目・スクリプト区切りとして解釈する余地を無くす。
            lines.push(format!("\t{hl}").trim_end().to_string());
        }
        lines.push("]]".to_string());
    }
    lines.extend(lua_lines(spec));
    let mut out = lines.join("\r\n");
    out.push_str("\r\n");
    out
}

/// 検証 → 生成 → 再検証をまとめて行う。
pub fn build(spec: &EffectSpec) -> Result<String, Vec<Issue>> {
    let mut issues = spec.validate();
    if issues.is_empty() {
        for s in &spec.shaders {
            issues.extend(hlsl::lint(s));
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }
    let text = emit_anm2(spec);
    crate::verify::verify_anm2(&text).map_err(|m| {
        vec![Issue { code: "E_VERIFY", message: format!("生成した .anm2 が検証を通りません: {m}") }]
    })?;
    Ok(text)
}

/// 出力ファイル名(拡張子つき)。
pub fn file_name(spec: &EffectSpec) -> String {
    format!("{}.anm2", spec.meta.name)
}
