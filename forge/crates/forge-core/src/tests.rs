use crate::emit::{build, emit_anm2, fmt_dec};
use crate::spec::*;
use crate::verify::verify_anm2;
use crate::{au2fx, hlsl, prompt};

const EXAMPLES: [&str; 4] = [
    include_str!("../../../examples/invert.json"),
    include_str!("../../../examples/vhs.json"),
    include_str!("../../../examples/blur.json"),
    include_str!("../../../examples/mosaic.json"),
];

fn invert() -> EffectSpec {
    EffectSpec::from_json(EXAMPLES[0]).unwrap()
}

#[test]
fn examples_build_and_verify() {
    for src in EXAMPLES {
        let spec = EffectSpec::from_json(src).unwrap();
        let text = build(&spec).unwrap_or_else(|e| panic!("{}: {e:?}", spec.meta.name));
        assert!(text.contains("\r\n"));
        assert!(!text.replace("\r\n", "").contains('\r'));
        verify_anm2(&text).unwrap();
    }
}

#[test]
fn vhs_header_and_constants_line_up() {
    let spec = EffectSpec::from_json(EXAMPLES[1]).unwrap();
    let text = build(&spec).unwrap();
    assert!(text.contains("--track@p_noise_intensity:ノイズ強度,0,1,0.25,0.01\r\n"));
    assert!(text.contains("--check@p_scanlines:走査線,1\r\n"));
    assert!(text.contains("--color@p_tint:色味,0xffe8d0\r\n"));
    assert!(text.contains("--select@p_grain:ノイズ種類=0,細かい=0,粗い=1\r\n"));
    assert!(text.contains(
        "local fg_c = {obj.w, obj.h, obj.time, fg_progress, obj.frame, fg_seed, p_noise_intensity, p_color_shift, p_jitter, p_scanlines, fg_r_tint / 255, fg_g_tint / 255, fg_b_tint / 255, p_grain}\r\n"
    ));
    assert!(text.contains(
        "obj.pixelshader(\"psmain\", \"object\", {\"object\", \"random\"}, fg_c, \"copy\", \"loop\")\r\n"
    ));
    // Lua のテーブルと cbuffer の並びが一致する
    let names = spec.constant_names();
    assert_eq!(names.len(), 14);
    let prelude = hlsl::prelude(&spec, "psmain");
    let mut last = 0;
    for n in &names {
        let at = prelude.find(&format!("float {n};")).unwrap_or_else(|| panic!("{n}"));
        assert!(at >= last, "{n} の順序が違う");
        last = at;
    }
    assert!(prelude.contains("Texture2D tex1 : register(t1);"));
}

#[test]
fn blur_prepares_tempbuffer() {
    let spec = EffectSpec::from_json(EXAMPLES[2]).unwrap();
    let text = build(&spec).unwrap();
    let a = text.find("obj.copybuffer(\"tempbuffer\", \"object\")").unwrap();
    let b = text.find("obj.pixelshader(\"blur_h\", \"tempbuffer\"").unwrap();
    let c = text.find("obj.pixelshader(\"blur_v\", \"object\", {\"tempbuffer\"}").unwrap();
    assert!(a < b && b < c);
}

#[test]
fn number_formatting() {
    assert_eq!(fmt_dec(0.30000000000000004, 2), "0.3");
    assert_eq!(fmt_dec(-0.0, 2), "0");
    assert_eq!(fmt_dec(12.0, 1), "12");
    assert_eq!(fmt_dec(-1.25, 2), "-1.25");
    assert_eq!(fmt_dec(100.0, 0), "100");
}

// ---------------------------------------------------------------- 注入

/// ラベル経由でヘッダ行や Lua を差し込もうとする入力は、すべて検証で落ちる。
#[test]
fn hostile_labels_are_rejected() {
    let bad = [
        "強さ\nos.execute('calc')",
        "強さ\r\nobj.effect()",
        "a,0,1,0\n--[[",
        "x]]",
        "x--y",
        "名前:別名",
        "a=b",
        "a@b",
        "a\\nb",
        "a\"b",
        "a\u{2028}b",
        "a\u{0085}b",
        "",
        " 前後の空白 ",
        "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわをん",
    ];
    for b in bad {
        let mut s = invert();
        s.parameters[0].label = b.to_string();
        assert!(build(&s).is_err(), "label {b:?} が通ってしまった");

        let mut s = invert();
        s.meta.name = b.to_string();
        assert!(build(&s).is_err(), "name {b:?} が通ってしまった");

        let mut s = invert();
        s.parameters.push(Param {
            id: "mode".into(),
            label: "種類".into(),
            kind: ParamKind::Select { options: vec!["普通".into(), b.to_string()], default: 0 },
        });
        assert!(build(&s).is_err(), "option {b:?} が通ってしまった");
    }
}

#[test]
fn hostile_ids_and_names_are_rejected() {
    for id in ["Amount", "1a", "a b", "a;b", "time", "fg_x", "p_x", "a\nb", "amount)", ""] {
        let mut s = invert();
        s.parameters[0].id = id.to_string();
        assert!(build(&s).is_err(), "id {id:?} が通ってしまった");
    }
    for name in ["ps main", "ps\"main", "a]]", "ps:main", "1ps", "", "ps\nmain"] {
        let mut s = invert();
        s.shaders[0].name = name.to_string();
        s.passes[0].shader = name.to_string();
        assert!(build(&s).is_err(), "shader name {name:?} が通ってしまった");
    }
    for t in ["framebuffer", "object\", os.exit(), \"", "cache:x"] {
        let mut s = invert();
        s.passes[0].target = t.to_string();
        assert!(build(&s).is_err(), "target {t:?} が通ってしまった");
        let mut s = invert();
        s.passes[0].resources = vec![t.to_string()];
        assert!(build(&s).is_err(), "resource {t:?} が通ってしまった");
    }
}

/// HLSL 内の `]]` は Lua の長いコメントを閉じる。意味を変えずに無害化され、
/// 生成物のコメントの外に AI の文字列が出ない。
#[test]
fn bracket_close_in_hlsl_cannot_escape_comment() {
    let mut s = invert();
    s.shaders[0].code = "static const int idx[2] = {0, 1};\nstatic const float v[2] = {1, 0};\n\
        float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target {\n\
        \treturn tex0.Sample(smp, uv) * v[idx[0]];\n}\n\
        // ]] os.execute('calc') --[[\n/* ]]\nos.execute('calc')\n--[[ */"
        .to_string();
    let text = build(&s).unwrap();
    assert!(!text.contains("os.execute"), "コメントは取り除かれるはず");
    assert!(text.contains("v[idx[0] ]"));
    // シェーダブロックの後ろには固定の Lua しか無い
    let tail = text.rsplit("]]\r\n").next().unwrap();
    assert!(tail.starts_with("local fg_progress = 0\r\n"));
    assert_eq!(text.matches("]]").count(), 1);
}

#[test]
fn lint_rejects_forbidden_constructs() {
    let cases = [
        "cbuffer c : register(b0) { float x; };",
        "Texture2D t : register(t5);",
        "#include \"x.hlsl\"",
        "#pragma warning(disable: 1)",
        "void f() { while (true) {} }",
        "float4 other(float4 pos : SV_Position) : SV_Target { return 0; }", // エントリが無い
    ];
    let sig = "float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target { return 0; }\n";
    for (i, c) in cases.iter().enumerate() {
        let mut s = invert();
        s.shaders[0].code = if i == cases.len() - 1 { c.to_string() } else { format!("{sig}{c}") };
        let e = build(&s).expect_err(c);
        assert!(e.iter().all(|x| x.code == "E_STATIC"), "{c}: {e:?}");
    }
    // コメント内の日本語と禁止語は問題ない
    let mut s = invert();
    s.shaders[0].code = format!("// 色を反転する。while は使わない\n{sig}");
    build(&s).unwrap();
    // コード部分の非 ASCII は拒否
    let mut s = invert();
    s.shaders[0].code = format!("{sig}float 変数 = 1;");
    assert!(build(&s).is_err());
}

/// 検証器は emit と独立に、少しでも違う行を拒否する。
#[test]
fn verifier_rejects_tampering() {
    let good = emit_anm2(&invert());
    verify_anm2(&good).unwrap();
    let tamper = |from: &str, to: &str| {
        assert!(good.contains(from), "置換元が無い: {from}");
        good.replacen(from, to, 1)
    };
    let bad = [
        tamper("local fg_progress = 0\r\n", "local fg_progress = 0\r\nos.execute('calc')\r\n"),
        tamper("]]\r\n", "]]\r\nrequire('x')\r\n"),
        tamper("--track@p_amount:強さ,0,1,1,0.01", "--track@p_amount:強さ,0,1,1,0.01\r\nprint(1)"),
        tamper("--track@p_amount:強さ", "--track@amount:強さ"),
        tamper("fg_c, \"copy\"", "fg_c, \"copy\" .. os.getenv('X')"),
        tamper("p_amount}", "p_amount, io.read()}"),
        tamper("p_amount}", "p_other}"),
        tamper("\"psmain\", \"object\"", "\"nope\", \"object\""),
        tamper("\tcbuffer constant0", "cbuffer constant0"),
        tamper("--filter\r\n", ""),
        good.replace("\r\n", "\n"),
        format!("\u{feff}{good}"),
        format!("{good}obj.draw()\r\n"),
    ];
    for (i, b) in bad.iter().enumerate() {
        assert!(verify_anm2(b).is_err(), "改変 {i} が検証を通ってしまった");
    }
}

// ---------------------------------------------------------------- 仕様検証

#[test]
fn validation_catches_structural_errors() {
    let check = |f: &dyn Fn(&mut EffectSpec), what: &str| {
        let mut s = invert();
        f(&mut s);
        assert!(!s.validate().is_empty(), "{what} を検出できない");
    };
    check(&|s| s.schema_version = "2.0".into(), "版違い");
    check(&|s| s.parameters[0].kind = ParamKind::Float { default: 2.0, min: 0.0, max: 1.0, step: 0.01 }, "範囲外の既定値");
    check(&|s| s.parameters[0].kind = ParamKind::Float { default: 0.5, min: 1.0, max: 0.0, step: 0.01 }, "min>max");
    check(&|s| s.parameters[0].kind = ParamKind::Float { default: 0.5, min: 0.0, max: 1.0, step: 0.05 }, "不正な step");
    check(&|s| s.parameters[0].kind = ParamKind::Float { default: f64::NAN, min: 0.0, max: 1.0, step: 0.01 }, "NaN");
    check(&|s| s.parameters[0].kind = ParamKind::Color { default: "red".into() }, "色の書式");
    check(&|s| s.parameters.push(s.parameters[0].clone()), "重複 id");
    check(&|s| s.passes.clear(), "パス無し");
    check(&|s| s.passes[0].target = "tempbuffer".into(), "最後が object でない");
    check(&|s| s.passes[0].resources = vec!["tempbuffer".into()], "未書き込みの tempbuffer");
    check(&|s| s.passes[0].shader = "nope".into(), "未定義シェーダ");
    check(&|s| s.shaders.clear(), "シェーダ無し");
    check(
        &|s| {
            // amount_r は color "amount" の要素名と衝突する
            s.parameters[0].kind = ParamKind::Color { default: "#ffffff".into() };
            s.parameters.push(Param { id: "amount_r".into(), label: "別".into(), kind: ParamKind::Bool { default: false } });
        },
        "要素名の衝突",
    );
    let mut s = invert();
    s.refusal = Some("前のフレームは参照できません".into());
    assert_eq!(s.validate()[0].code, "E_REFUSAL");
}

// ---------------------------------------------------------------- 依頼文・取り込み

#[test]
fn extract_json_from_various_replies() {
    let json = EXAMPLES[0];
    let plain = prompt::extract_json(json).unwrap();
    let fenced = prompt::extract_json(&format!("できました。\n```json\n{json}\n```\n調整してください。")).unwrap();
    let chatty = prompt::extract_json(&format!("ポイントは {{中かっこ}} です。\n{json}\n以上 }}")).unwrap();
    for t in [plain, fenced, chatty] {
        assert_eq!(EffectSpec::from_json(&t).unwrap(), invert());
    }
    assert!(prompt::extract_json("JSON はありません { ただの文 }").is_err());
    assert!(prompt::extract_json("").is_err());
}

#[test]
fn request_texts_contain_the_contract_and_input() {
    let r = prompt::build_request("VHS風のノイズと色ズレ");
    assert!(r.contains("乗算済みアルファ") && r.contains("VHS風のノイズと色ズレ"));
    let issues = vec![Issue { code: "E_COMPILE", message: "psmain(3,5): error X3004".into() }];
    let rep = prompt::build_repair(EXAMPLES[0], "HLSL のコンパイル", &issues);
    assert!(rep.contains("X3004") && rep.contains("\"色反転\""));
    let rev = prompt::build_revision(&invert(), "もっと強く");
    assert!(rev.contains("もっと強く") && rev.contains("\"amount\""));
}

// ---------------------------------------------------------------- .au2fx

#[test]
fn au2fx_roundtrip_and_tamper_detection() {
    for src in EXAMPLES {
        let spec = EffectSpec::from_json(src).unwrap();
        let bytes = au2fx::pack(&spec).unwrap();
        let (m, back) = au2fx::unpack(&bytes).unwrap();
        assert_eq!(back, spec);
        assert_eq!(m.name, spec.meta.name);
        // 再生成した .anm2 は元と一致する
        assert_eq!(build(&back).unwrap(), build(&spec).unwrap());
    }
    assert!(au2fx::unpack(b"not a zip").is_err());

    // spec.json を差し替えた(ハッシュ不一致)ファイルは拒否
    use std::io::Write;
    let good = au2fx::pack(&invert()).unwrap();
    let (manifest, _) = au2fx::unpack(&good).unwrap();
    let mut evil = invert();
    evil.meta.name = "別物".into();
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opt = zip::write::SimpleFileOptions::default();
    zw.start_file("manifest.json", opt).unwrap();
    zw.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    zw.start_file("spec.json", opt).unwrap();
    zw.write_all(&serde_json::to_vec(&evil).unwrap()).unwrap();
    let forged = zw.finish().unwrap().into_inner();
    let e = au2fx::unpack(&forged).unwrap_err();
    assert!(e.message.contains("ハッシュ"), "{e}");

    // 未対応の major 版は拒否
    let mut m2 = manifest.clone();
    m2.format_version = "2.0".into();
    let mut zw = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zw.start_file("manifest.json", opt).unwrap();
    zw.write_all(&serde_json::to_vec(&m2).unwrap()).unwrap();
    zw.start_file("spec.json", opt).unwrap();
    zw.write_all(&serde_json::to_vec(&invert()).unwrap()).unwrap();
    assert!(au2fx::unpack(&zw.finish().unwrap().into_inner()).is_err());
}
