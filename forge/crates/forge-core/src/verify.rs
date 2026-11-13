//! 生成した .anm2 の独立検証。
//!
//! emit とは別のコード(厳格な行文法)で読み直し、テンプレート由来の行だけで
//! できていることを確かめる。emit 側にバグや見落としがあっても、AI 由来の
//! 文字列が Lua として実行される形のファイルは書き出さない。

use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

const LABEL: &str = r#"[^,:=@\[\]\\"'\-{}\x00-\x1f\x7f\x{85}\x{2028}\x{2029}\x{feff}]{1,24}"#;
const NAME: &str = r#"[^,:=@\[\]\\"'\-{}/*?<>|.\x00-\x1f\x7f\x{85}\x{2028}\x{2029}\x{feff}]{1,40}"#;
const ID: &str = r"[a-z][a-z0-9_]{0,31}";
const SHADER: &str = r"[A-Za-z_][A-Za-z0-9_]{0,31}";
const NUM: &str = r"-?[0-9]{1,9}(?:\.[0-9]{1,9})?";

struct Rx {
    info: Regex,
    track: Regex,
    check: Regex,
    color: Regex,
    select: Regex,
    shader_open: Regex,
    rgb: Regex,
    consts: Regex,
    item_param: Regex,
    item_color: Regex,
    pixelshader: Regex,
}

fn rx() -> &'static Rx {
    static R: OnceLock<Rx> = OnceLock::new();
    R.get_or_init(|| {
        let r = |s: String| Regex::new(&s).expect("regex");
        Rx {
            info: r(format!(r"^--information:{NAME} \(Forge [0-9.]+\)$")),
            track: r(format!(r"^--track@p_({ID}):{LABEL},{NUM},{NUM},{NUM},(?:1|0\.1|0\.01|0\.001|0\.0001)$")),
            check: r(format!(r"^--check@p_({ID}):{LABEL},[01]$")),
            color: r(format!(r"^--color@p_({ID}):{LABEL},0x[0-9a-f]{{6}}$")),
            select: r(format!(r"^--select@p_({ID}):{LABEL}=[0-9]{{1,2}}(?:,{LABEL}=[0-9]{{1,2}}){{2,16}}$")),
            shader_open: r(format!(r"^--\[\[pixelshader@({SHADER}):$")),
            rgb: r(format!(r"^local fg_r_({ID}), fg_g_({ID}), fg_b_({ID}) = RGB\(p_({ID})\)$")),
            consts: r(r"^local fg_c = \{(.+)\}$".to_string()),
            item_param: r(format!(r"^p_({ID})$")),
            item_color: r(format!(r"^fg_[rgb]_({ID}) / 255$")),
            pixelshader: r(format!(
                r#"^obj\.pixelshader\("({SHADER})", "(?:object|tempbuffer)", \{{"(?:object|tempbuffer|random)"(?:, "(?:object|tempbuffer|random)"){{0,3}}\}}, fg_c, "(?:copy|mask|draw|add)", "(?:clip|clamp|loop|mirror|dot)"\)$"#
            )),
        }
    })
}

/// 生成物が期待した構造だけでできているかを確かめる。
pub fn verify_anm2(text: &str) -> Result<(), String> {
    if text.starts_with('\u{feff}') {
        return Err("BOM が付いています".into());
    }
    if !text.ends_with("\r\n") {
        return Err("末尾が改行で終わっていません".into());
    }
    let body = text.replace("\r\n", "\n");
    if body.contains('\r') {
        return Err("単独の CR が含まれています".into());
    }
    if body.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("制御文字が含まれています".into());
    }
    let lines: Vec<&str> = body.trim_end_matches('\n').split('\n').collect();
    let rx = rx();
    let mut i = 0;
    let err = |i: usize, what: &str, line: &str| Err(format!("{}行目: {what}: {line:?}", i + 1));

    // ---- 1. ヘッダ
    if lines.len() < 3 || !rx.info.is_match(lines[0]) {
        return err(0, "--information 行が不正です", lines.first().copied().unwrap_or(""));
    }
    if lines[1] != "--label:Forge" {
        return err(1, "--label 行が不正です", lines[1]);
    }
    if lines[2] != "--filter" {
        return err(2, "--filter 行がありません", lines[2]);
    }
    i = i.max(3);
    let mut params: HashSet<String> = HashSet::new();
    let mut colors: HashSet<String> = HashSet::new();
    while i < lines.len() && !lines[i].starts_with("--[[") {
        let l = lines[i];
        let id = if let Some(c) = rx.track.captures(l).or_else(|| rx.check.captures(l)).or_else(|| rx.select.captures(l)) {
            c[1].to_string()
        } else if let Some(c) = rx.color.captures(l) {
            colors.insert(c[1].to_string());
            c[1].to_string()
        } else {
            return err(i, "設定項目の行として認識できません", l);
        };
        if !params.insert(id) {
            return err(i, "変数名が重複しています", l);
        }
        i += 1;
    }

    // ---- 2. シェーダブロック
    let mut shaders: HashSet<String> = HashSet::new();
    while i < lines.len() && lines[i].starts_with("--[[") {
        let Some(c) = rx.shader_open.captures(lines[i]) else {
            return err(i, "シェーダブロックの開始行が不正です", lines[i]);
        };
        if !shaders.insert(c[1].to_string()) {
            return err(i, "シェーダ名が重複しています", lines[i]);
        }
        i += 1;
        loop {
            if i >= lines.len() {
                return Err("シェーダブロックが閉じられていません".into());
            }
            let l = lines[i];
            if l == "]]" {
                i += 1;
                break;
            }
            if l.contains("]]") {
                return err(i, "シェーダ内に ]] があります", l);
            }
            if !l.is_empty() && !l.starts_with('\t') {
                return err(i, "シェーダ内の行がタブで始まっていません", l);
            }
            if !l.is_ascii() {
                return err(i, "シェーダ内に ASCII 以外の文字があります", l);
            }
            i += 1;
        }
    }
    if shaders.is_empty() {
        return Err("シェーダブロックがありません".into());
    }

    // ---- 3. Lua 本体(固定の行だけを許す)
    let expect = |i: usize, want: &str| -> Result<(), String> {
        match lines.get(i) {
            Some(l) if *l == want => Ok(()),
            Some(l) => Err(format!("{}行目: 想定外の行です: {l:?}", i + 1)),
            None => Err("Lua 本体が途中で終わっています".into()),
        }
    };
    expect(i, "local fg_progress = 0")?;
    expect(i + 1, "if obj.totaltime > 0 then fg_progress = obj.time / obj.totaltime end")?;
    expect(i + 2, "local fg_seed = obj.rand(0, 65535, 1, 0)")?;
    i += 3;
    let mut rgb_done: HashSet<String> = HashSet::new();
    while let Some(c) = lines.get(i).and_then(|l| rx.rgb.captures(l)) {
        let id = c[1].to_string();
        if c[2] != id || c[3] != id || c[4] != id || !colors.contains(&id) || !rgb_done.insert(id) {
            return err(i, "RGB 行が設定項目と対応していません", lines[i]);
        }
        i += 1;
    }
    let Some(c) = lines.get(i).and_then(|l| rx.consts.captures(l)) else {
        return err(i, "定数テーブルの行がありません", lines.get(i).copied().unwrap_or(""));
    };
    let items: Vec<&str> = c.get(1).map(|m| m.as_str()).unwrap_or("").split(", ").collect();
    let builtin = ["obj.w", "obj.h", "obj.time", "fg_progress", "obj.frame", "fg_seed"];
    if items.len() < builtin.len() || items[..builtin.len()] != builtin {
        return err(i, "定数テーブルの先頭が組み込み値と一致しません", lines[i]);
    }
    for it in &items[builtin.len()..] {
        let ok = if let Some(c) = rx.item_param.captures(it) {
            params.contains(&c[1]) && !colors.contains(&c[1])
        } else if let Some(c) = rx.item_color.captures(it) {
            rgb_done.contains(&c[1])
        } else {
            false
        };
        if !ok {
            return err(i, &format!("定数テーブルの要素 {it:?} が不正です"), lines[i]);
        }
    }
    i += 1;
    if lines.get(i) == Some(&"obj.copybuffer(\"tempbuffer\", \"object\")") {
        i += 1;
    }
    let mut passes = 0;
    while i < lines.len() {
        let Some(c) = rx.pixelshader.captures(lines[i]) else {
            return err(i, "想定外の行です", lines[i]);
        };
        if !shaders.contains(&c[1]) {
            return err(i, "未定義のシェーダを呼んでいます", lines[i]);
        }
        passes += 1;
        i += 1;
    }
    if passes == 0 {
        return Err("obj.pixelshader の呼び出しがありません".into());
    }
    Ok(())
}
