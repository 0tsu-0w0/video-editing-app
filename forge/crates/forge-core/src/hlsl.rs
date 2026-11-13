//! HLSL の前置コード(prelude)生成と静的検査。
//!
//! AI は関数だけを書く。定数バッファ・テクスチャ・サンプラの宣言は Forge が
//! 生成して前に付けるので、Lua 側の値の並びと必ず一致する。

use crate::spec::{EffectSpec, Issue, ParamKind, Shader, BUILTIN_CONSTANTS};

/// AI が書いたコードに含まれてはいけない語。
const FORBIDDEN_WORDS: [(&str, &str); 7] = [
    ("cbuffer", "定数バッファは Forge が宣言します。cbuffer を書かないでください"),
    ("register", "register(...) は Forge が宣言します。書かないでください"),
    ("while", "while は使えません。回数が定数の for を使ってください"),
    ("goto", "goto は使えません"),
    ("Texture2D", "テクスチャは tex0〜tex3 を使ってください。宣言は不要です"),
    ("SamplerState", "サンプラは smp を使ってください。宣言は不要です"),
    ("RWTexture2D", "RWTexture2D は使えません"),
];

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `word` が識別子として(単語境界つきで)含まれるか。
pub fn contains_word(src: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(i) = src[start..].find(word) {
        let a = start + i;
        let b = a + word.len();
        let before_ok = src[..a].chars().next_back().map_or(true, |c| !is_ident_char(c));
        let after_ok = src[b..].chars().next().map_or(true, |c| !is_ident_char(c));
        if before_ok && after_ok {
            return true;
        }
        start = b;
    }
    false
}

/// コメントと文字列リテラルを空白に置き換える(行番号は保つ)。
pub fn strip_comments(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            out.push_str("  ");
            i += 2;
            while i < b.len() && !(b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/') {
                out.push(if b[i] == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            if i < b.len() {
                out.push_str("  ");
                i += 2;
            }
        } else if b[i] == '"' {
            out.push(' ');
            i += 1;
            while i < b.len() && b[i] != '"' && b[i] != '\n' {
                out.push(' ');
                i += 1;
            }
            if i < b.len() && b[i] == '"' {
                out.push(' ');
                i += 1;
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// `]]` は Lua の長いコメントを閉じてしまうので、意味を変えずに `] ]` へ直す。
/// コメントは丸ごと取り除く(コメント内の `]]` や Lua 風の記述を残さない)。
pub fn sanitize_code(code: &str) -> String {
    let mut s = strip_comments(code).replace('\r', "");
    while s.contains("]]") {
        s = s.replace("]]", "] ]");
    }
    // 行末の空白を落とす
    s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n").trim_matches('\n').to_string()
}

/// AI が書いたシェーダ本体の静的検査。
pub fn lint(shader: &Shader) -> Vec<Issue> {
    let mut out = Vec::new();
    let code = strip_comments(&shader.code);
    let at = format!("shaders[name={:?}]", shader.name);
    let push = |out: &mut Vec<Issue>, m: String| out.push(Issue { code: "E_STATIC", message: m });

    if !shader.code.is_ascii() {
        // コメント内の日本語は許すが、コード部分は ASCII のみ。
        if !code.is_ascii() {
            push(&mut out, format!("{at}: コード部分に ASCII 以外の文字があります(コメント以外は ASCII のみ)"));
        }
    }
    if code.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        push(&mut out, format!("{at}: 制御文字が含まれています"));
    }
    for (w, msg) in FORBIDDEN_WORDS {
        if contains_word(&code, w) {
            push(&mut out, format!("{at}: {msg}"));
        }
    }
    for line in code.lines() {
        let t = line.trim_start();
        if t.starts_with('#') {
            let d = t[1..].trim_start();
            if !(d.starts_with("define") || d.starts_with("if") || d.starts_with("else") || d.starts_with("endif") || d.starts_with("undef")) {
                push(&mut out, format!("{at}: プリプロセッサは #define と #if 系のみ使えます({:?})", t.trim_end()));
            }
        }
    }
    if !contains_word(&code, &shader.name) {
        push(&mut out, format!("{at}: エントリーポイント関数 {} が見つかりません。name と同じ名前の関数を定義してください", shader.name));
    }
    if !code.contains("SV_Target") {
        push(&mut out, format!("{at}: 戻り値に : SV_Target を付けてください"));
    }
    if !code.contains("SV_Position") {
        push(&mut out, format!("{at}: 第1引数は float4 pos : SV_Position にしてください"));
    }
    out
}

/// シェーダごとに必要なテクスチャ数(そのシェーダを使うパスの resources の最大数)。
pub fn texture_count(spec: &EffectSpec, shader_name: &str) -> usize {
    spec.passes
        .iter()
        .filter(|p| p.shader == shader_name)
        .map(|p| p.resources.len())
        .max()
        .unwrap_or(1)
        .clamp(1, 4)
}

/// 定数バッファ・テクスチャ・サンプラの宣言を生成する。
pub fn prelude(spec: &EffectSpec, shader_name: &str) -> String {
    let mut s = String::new();
    s.push_str("cbuffer constant0 : register(b0) {\n");
    for n in BUILTIN_CONSTANTS {
        s.push_str(&format!("\tfloat {n};\n"));
    }
    for p in &spec.parameters {
        for n in p.constant_names() {
            s.push_str(&format!("\tfloat {n};\n"));
        }
    }
    s.push_str("};\n");
    s.push_str("#define resolution float2(fg_res_x, fg_res_y)\n");
    for p in &spec.parameters {
        if let ParamKind::Color { .. } = p.kind {
            s.push_str(&format!("#define {0} float3({0}_r, {0}_g, {0}_b)\n", p.id));
        }
    }
    for i in 0..texture_count(spec, shader_name) {
        s.push_str(&format!("Texture2D tex{i} : register(t{i});\n"));
    }
    s.push_str("SamplerState smp : register(s0);\n");
    s
}

/// ホスト(とハーネス)へ渡す完全な HLSL ソース。
pub fn full_source(spec: &EffectSpec, shader: &Shader) -> String {
    format!("{}{}\n", prelude(spec, &shader.name), sanitize_code(&shader.code))
}

/// prelude の行数(コンパイルエラーの行番号を AI のコード基準に直すため)。
pub fn prelude_lines(spec: &EffectSpec, shader_name: &str) -> usize {
    prelude(spec, shader_name).lines().count()
}
