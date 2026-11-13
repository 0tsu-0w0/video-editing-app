//! forge: 自然言語から作ったエフェクトを AviUtl2 のスクリプト(.anm2)にするコマンド。

use forge_core::spec::{EffectSpec, Issue, ParamKind};
use forge_core::{au2fx, emit, prompt};
use forge_gpu::{FrameInfo, Gpu, Image};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
forge - AviUtl2 用エフェクト(.anm2)を作る

使い方:
  forge prompt <指示文> [-o 依頼文.txt]
      AI に渡す依頼文を作る。出力を Claude などに貼り付けて、返答を保存する。
  forge import <返答.txt> [--out-dir out]
      AI の返答を検証して .anm2 / .au2fx / プレビュー画像を作る。
      失敗したら、AI に渡す修復依頼文(repair_prompt.txt)を作る。
  forge revise <spec.json|.au2fx> <修正の指示> [-o 依頼文.txt]
      既存エフェクトの修正依頼文を作る。
  forge build <spec.json|.au2fx> [--out-dir out]
      保存済みのエフェクトから .anm2 などを作り直す。
  forge render <spec.json|.au2fx> [--input 画像] [--time 秒] [--set id=値] [-o out.png]
      画像にエフェクトを適用して保存する(--set は複数可。色は id=#RRGGBB)。
  forge install <ファイル.anm2>
      AviUtl2 のスクリプトフォルダ(ProgramData\\aviutl2\\Script\\Forge)へコピーする。

共通オプション:
  --software   GPU を使わずソフトウェア描画で試走する
";

struct Args {
    positional: Vec<String>,
    options: HashMap<String, Vec<String>>,
    flags: Vec<String>,
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let with_value = ["-o", "--out-dir", "--input", "--time", "--set"];
    let mut a = Args { positional: vec![], options: HashMap::new(), flags: vec![] };
    let mut it = raw.into_iter();
    while let Some(x) = it.next() {
        if with_value.contains(&x.as_str()) {
            let v = it.next().ok_or(format!("{x} には値が必要です"))?;
            a.options.entry(x).or_default().push(v);
        } else if x.starts_with("--") {
            a.flags.push(x);
        } else {
            a.positional.push(x);
        }
    }
    Ok(a)
}

impl Args {
    fn opt(&self, k: &str) -> Option<&str> {
        self.options.get(k).and_then(|v| v.last()).map(|s| s.as_str())
    }
    fn pos(&self, i: usize, what: &str) -> Result<&str, String> {
        self.positional.get(i).map(|s| s.as_str()).ok_or(format!("{what} を指定してください\n\n{USAGE}"))
    }
}

fn read_text(path: &str) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path} を読めません: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{path} が UTF-8 ではありません"))?;
    Ok(text.trim_start_matches('\u{feff}').to_string())
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{} を作れません: {e}", dir.display()))?;
    }
    std::fs::write(path, data).map_err(|e| format!("{} に書けません: {e}", path.display()))
}

/// spec.json か .au2fx を読む。
fn load_spec(path: &str) -> Result<EffectSpec, String> {
    if path.to_ascii_lowercase().ends_with(".au2fx") {
        let bytes = std::fs::read(path).map_err(|e| format!("{path} を読めません: {e}"))?;
        au2fx::unpack(&bytes).map(|(_, s)| s).map_err(|e| e.to_string())
    } else {
        EffectSpec::from_json(&read_text(path)?).map_err(|e| e.to_string())
    }
}

fn emit_text(text: &str, out: Option<&str>) -> Result<(), String> {
    match out {
        Some(p) => {
            write_file(Path::new(p), text.as_bytes())?;
            eprintln!("依頼文を {p} に書きました。内容を AI に貼り付けてください。");
        }
        None => print!("{text}"),
    }
    Ok(())
}

struct Failure {
    stage: &'static str,
    issues: Vec<Issue>,
}

struct Built {
    anm2: String,
    preview: Image,
    warnings: Vec<String>,
}

/// 検証 → 生成 → コンパイル → 試走。
fn pipeline(spec: &EffectSpec, gpu: &Gpu) -> Result<Built, Failure> {
    let anm2 = emit::build(spec).map_err(|issues| {
        let stage = if issues.iter().any(|i| i.code == "E_STATIC") { "コードの静的検査" } else { "形式の検証" };
        Failure { stage, issues }
    })?;
    let compiled = forge_gpu::compile_spec(gpu, spec)
        .map_err(|issues| Failure { stage: "HLSL のコンパイル", issues })?;
    let (errors, warnings) = forge_gpu::trial_run(gpu, spec, &compiled);
    if !errors.is_empty() {
        return Err(Failure { stage: "試走(テスト画像への適用)", issues: errors });
    }
    let input = forge_gpu::test_image(640, 360);
    let info = FrameInfo { time: 0.5, progress: 0.5, frame: 15.0, seed: 12345.0 };
    let preview = forge_gpu::render(gpu, spec, &compiled, &input, info, &HashMap::new())
        .map_err(|e| Failure { stage: "プレビューの描画", issues: vec![Issue { code: "E_RENDER", message: e }] })?;
    Ok(Built { anm2, preview, warnings })
}

fn save_png(path: &Path, img: &Image) -> Result<(), String> {
    let buf = image::RgbaImage::from_raw(img.width, img.height, img.to_rgba8())
        .ok_or("画像の変換に失敗しました")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    buf.save(path).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

fn write_outputs(spec: &EffectSpec, built: &Built, out_dir: &Path) -> Result<(), String> {
    let name = &spec.meta.name;
    let anm2 = out_dir.join(emit::file_name(spec));
    write_file(&anm2, built.anm2.as_bytes())?;
    write_file(&out_dir.join(format!("{name}.au2fx")), &au2fx::pack(spec).map_err(|e| e.to_string())?)?;
    write_file(
        &out_dir.join(format!("{name}.spec.json")),
        serde_json::to_string_pretty(spec).map_err(|e| e.to_string())?.as_bytes(),
    )?;
    save_png(&out_dir.join(format!("{name}.preview.png")), &built.preview)?;
    println!("OK: {name}");
    println!("  スクリプト : {}", anm2.display());
    println!("  保存形式   : {}", out_dir.join(format!("{name}.au2fx")).display());
    println!("  プレビュー : {}", out_dir.join(format!("{name}.preview.png")).display());
    if !spec.parameters.is_empty() {
        println!("  設定項目   :");
        for p in &spec.parameters {
            let kind = match &p.kind {
                ParamKind::Float { default, min, max, .. } => format!("{min}〜{max}(既定 {default})"),
                ParamKind::Int { default, min, max } => format!("{min}〜{max}(既定 {default})"),
                ParamKind::Bool { default } => format!("チェック(既定 {})", if *default { "ON" } else { "OFF" }),
                ParamKind::Color { default } => format!("色(既定 {default})"),
                ParamKind::Select { options, default } => format!("選択 {}(既定 {})", options.join("/"), options[*default]),
            };
            println!("    - {}: {kind}", p.label);
        }
    }
    for w in &built.warnings {
        println!("  注意: {w}");
    }
    println!("AviUtl2 で使うには: forge install \"{}\"", anm2.display());
    Ok(())
}

fn report_failure(f: &Failure) {
    eprintln!("失敗: {}で問題が見つかりました。", f.stage);
    for i in &f.issues {
        eprintln!("  {i}");
    }
}

fn parse_set(spec: &EffectSpec, sets: &[String]) -> Result<HashMap<String, Vec<f32>>, String> {
    let mut m = HashMap::new();
    for s in sets {
        let (id, v) = s.split_once('=').ok_or(format!("--set は id=値 の形にしてください: {s}"))?;
        let p = spec.parameters.iter().find(|p| p.id == id).ok_or(format!("パラメータ {id} はありません"))?;
        let vals = match &p.kind {
            ParamKind::Color { .. } => {
                let c = forge_core::spec::parse_color(v).ok_or(format!("{id} は #RRGGBB で指定してください"))?;
                vec![((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
            }
            ParamKind::Bool { .. } => vec![if matches!(v, "1" | "true" | "on") { 1.0 } else { 0.0 }],
            _ => vec![v.parse::<f32>().map_err(|_| format!("{id} の値 {v} は数値ではありません"))?],
        };
        m.insert(id.to_string(), vals);
    }
    Ok(m)
}

fn script_dir() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_string());
    Path::new(&base).join("aviutl2").join("Script").join("Forge")
}

fn run() -> Result<ExitCode, String> {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw[0] == "--help" || raw[0] == "-h" {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let cmd = raw.remove(0);
    let args = parse_args(raw)?;
    let software = args.flags.iter().any(|f| f == "--software");
    let out_dir = PathBuf::from(args.opt("--out-dir").unwrap_or("out"));

    match cmd.as_str() {
        "prompt" => {
            emit_text(&prompt::build_request(args.pos(0, "指示文")?), args.opt("-o"))?;
        }
        "revise" => {
            let spec = load_spec(args.pos(0, "spec.json または .au2fx")?)?;
            emit_text(&prompt::build_revision(&spec, args.pos(1, "修正の指示")?), args.opt("-o"))?;
        }
        "import" | "build" => {
            let path = args.pos(0, "入力ファイル")?;
            let gpu = Gpu::new(software)?;
            // import は AI の返答(説明文つきでもよい)、build は保存済みの spec。
            let (json, parsed) = if cmd == "import" {
                match prompt::extract_json(&read_text(path)?) {
                    Ok(j) => {
                        let p = EffectSpec::from_json(&j);
                        (j, p)
                    }
                    Err(e) => (String::new(), Err(e)),
                }
            } else {
                let s = load_spec(path)?;
                (serde_json::to_string_pretty(&s).unwrap_or_default(), Ok(s))
            };
            let result = match parsed {
                Ok(spec) => pipeline(&spec, &gpu).map(|b| (spec, b)),
                Err(e) => Err(Failure { stage: "返答の読み取り", issues: vec![e] }),
            };
            match result {
                Ok((spec, built)) => write_outputs(&spec, &built, &out_dir)?,
                Err(f) => {
                    report_failure(&f);
                    if cmd == "import" && !f.issues.iter().any(|i| i.code == "E_REFUSAL") {
                        let source = if json.is_empty() { read_text(path)? } else { json };
                        let p = out_dir.join("repair_prompt.txt");
                        write_file(&p, prompt::build_repair(&source, f.stage, &f.issues).as_bytes())?;
                        eprintln!("修復依頼文を {} に書きました。内容を AI に貼り付け、返答をもう一度 import してください。", p.display());
                    }
                    return Ok(ExitCode::from(2));
                }
            }
        }
        "render" => {
            let spec = load_spec(args.pos(0, "spec.json または .au2fx")?)?;
            let gpu = Gpu::new(software)?;
            if let Err(issues) = emit::build(&spec) {
                report_failure(&Failure { stage: "形式の検証", issues });
                return Ok(ExitCode::from(2));
            }
            let compiled = match forge_gpu::compile_spec(&gpu, &spec) {
                Ok(c) => c,
                Err(issues) => {
                    report_failure(&Failure { stage: "HLSL のコンパイル", issues });
                    return Ok(ExitCode::from(2));
                }
            };
            let input = match args.opt("--input") {
                Some(p) => {
                    let img = image::open(p).map_err(|e| format!("{p} を開けません: {e}"))?.to_rgba8();
                    Image::from_rgba8(img.width(), img.height(), img.as_raw())
                }
                None => forge_gpu::test_image(640, 360),
            };
            let time: f32 = match args.opt("--time") {
                Some(t) => t.parse().map_err(|_| "--time は秒数で指定してください".to_string())?,
                None => 0.5,
            };
            let overrides = parse_set(&spec, args.options.get("--set").map(|v| v.as_slice()).unwrap_or(&[]))?;
            let info = FrameInfo { time, progress: 0.5, frame: (time * 30.0).floor(), seed: 12345.0 };
            let out = forge_gpu::render(&gpu, &spec, &compiled, &input, info, &overrides)?;
            let path = PathBuf::from(args.opt("-o").unwrap_or("out/render.png"));
            save_png(&path, &out)?;
            println!("{} に保存しました({}描画)", path.display(), if gpu.software { "ソフトウェア" } else { "GPU " });
        }
        "install" => {
            let src = args.pos(0, ".anm2 ファイル")?;
            let text = std::fs::read_to_string(src).map_err(|e| format!("{src} を読めません: {e}"))?;
            // Forge が作った形のファイルだけをコピーする。
            forge_core::verify::verify_anm2(&text)
                .map_err(|e| format!("{src} は Forge が生成した .anm2 ではありません({e})"))?;
            let name = Path::new(src).file_name().ok_or("ファイル名がありません")?;
            let dst = script_dir().join(name);
            write_file(&dst, text.as_bytes())?;
            println!("{} にコピーしました。", dst.display());
            println!("新しいエフェクトは AviUtl2 の再起動後に「Forge」の中に出ます。");
            println!("既存エフェクトのシェーダだけを直した場合は「キャッシュを破棄」で反映されます。");
        }
        _ => return Err(format!("不明なコマンド: {cmd}\n\n{USAGE}")),
    }
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("エラー: {e}");
            ExitCode::FAILURE
        }
    }
}
