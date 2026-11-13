use super::*;
use forge_core::spec::{Blend, EffectSpec, Sampler};

const SIG: &str = "float4 psmain(float4 pos : SV_Position, float2 uv : TEXCOORD) : SV_Target";

fn gpu() -> Gpu {
    // テストは WARP(ソフトウェア描画)で行い、GPU の有無や機種に依存させない。
    Gpu::new(true).expect("WARP デバイス")
}

fn example(name: &str) -> EffectSpec {
    let path = format!("{}/../../examples/{name}.json", env!("CARGO_MANIFEST_DIR"));
    EffectSpec::from_json(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// 1パス・パラメータ無しの spec を、本体だけ差し替えて作る。
fn with_body(body: &str) -> EffectSpec {
    let mut s = example("invert");
    s.shaders[0].code = format!("{SIG} {{\n{body}\n}}");
    s
}

fn px(img: &Image, x: u32, y: u32) -> [f32; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.data[i], img.data[i + 1], img.data[i + 2], img.data[i + 3]]
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 2e-3
}

#[test]
fn examples_compile_and_pass_trial_run() {
    let gpu = gpu();
    for name in ["invert", "vhs", "blur", "mosaic"] {
        let spec = example(name);
        forge_core::emit::build(&spec).unwrap();
        let compiled = compile_spec(&gpu, &spec).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let (errors, warnings) = trial_run(&gpu, &spec, &compiled);
        assert!(errors.is_empty(), "{name}: {errors:?}");
        assert!(warnings.is_empty(), "{name}: {warnings:?}");
    }
}

#[test]
fn invert_produces_expected_pixels() {
    let gpu = gpu();
    let spec = example("invert");
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let input = test_image(64, 64);
    let out = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &HashMap::new()).unwrap();
    // 中心は不透明: rgb が 1 - rgb になる
    let (i, o) = (px(&input, 32, 32), px(&out, 32, 32));
    assert!(close(o[3], 1.0));
    for c in 0..3 {
        assert!(close(o[c], 1.0 - i[c]), "ch{c}: {} -> {}", i[c], o[c]);
    }
    // 角は透明のまま(乗算済みなので rgb も 0)
    assert_eq!(px(&out, 0, 0), [0.0; 4]);
    // amount = 0 なら入力と同じ
    let mut ov = HashMap::new();
    ov.insert("amount".to_string(), vec![0.0]);
    let same = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &ov).unwrap();
    assert!(same.data.iter().zip(&input.data).all(|(a, b)| close(*a, *b)));
}

/// Lua のテーブル(= constants())と cbuffer の並びが一致し、
/// 組み込み値・各型のパラメータがシェーダへ正しく届く。
#[test]
fn constants_reach_the_shader_in_order() {
    let gpu = gpu();
    let mut spec = example("vhs");
    // 値をそのまま色として出すシェーダに差し替える
    let probes = [
        ("float4(fg_res_x / 1000.0, fg_res_y / 1000.0, time, 1)", [0.064, 0.032, 0.25, 1.0]),
        ("float4(progress, frame / 100.0, seed / 100000.0, 1)", [0.5, 0.07, 0.12345, 1.0]),
        ("float4(noise_intensity, color_shift / 64.0, jitter / 50.0, 1)", [0.25, 6.0 / 64.0, 3.0 / 50.0, 1.0]),
        ("float4(tint, scanlines)", [1.0, 232.0 / 255.0, 208.0 / 255.0, 1.0]),
        ("float4(resolution / 1000.0, grain, 1)", [0.064, 0.032, 0.0, 1.0]),
        ("float4(uv, pos.x / 64.0, 1)", [16.5 / 64.0, 8.5 / 32.0, 16.5 / 64.0, 1.0]),
    ];
    let info = FrameInfo { time: 0.25, progress: 0.5, frame: 7.0, seed: 12345.0 };
    for (expr, want) in probes {
        spec.shaders[0].code = format!("{SIG} {{ return {expr}; }}");
        let compiled = compile_spec(&gpu, &spec).unwrap_or_else(|e| panic!("{expr}: {e:?}"));
        let out = render(&gpu, &spec, &compiled, &test_image(64, 32), info, &HashMap::new()).unwrap();
        let got = px(&out, 16, 8);
        for c in 0..4 {
            assert!(close(got[c], want[c]), "{expr}: ch{c} = {} (期待 {})", got[c], want[c]);
        }
    }
    // 上書きした値も届く
    spec.shaders[0].code = format!("{SIG} {{ return float4(tint, grain); }}");
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let mut ov = HashMap::new();
    ov.insert("tint".to_string(), vec![0.1, 0.2, 0.3]);
    ov.insert("grain".to_string(), vec![1.0]);
    let out = render(&gpu, &spec, &compiled, &test_image(64, 32), info, &ov).unwrap();
    let got = px(&out, 3, 3);
    for (c, w) in [0.1, 0.2, 0.3, 1.0].iter().enumerate() {
        assert!(close(got[c], *w));
    }
}

#[test]
fn compile_errors_are_reported_with_user_line_numbers() {
    let gpu = gpu();
    let spec = with_body("\tfloat4 c = tex0.Sample(smp, uv);\n\treturn undefined_name * c;");
    let err = compile_spec(&gpu, &spec).err().expect("コンパイルエラーになるはず");
    assert_eq!(err[0].code, "E_COMPILE");
    assert!(err[0].message.contains("undefined_name"), "{}", err[0].message);
    // AI が書いたコードの 3 行目(prelude の行数は引かれている)
    assert!(err[0].message.contains("3行目"), "{}", err[0].message);
}

#[test]
fn trial_run_detects_broken_effects() {
    let gpu = gpu();
    let cases = [
        ("\treturn tex0.Sample(smp, uv);", "E_RENDER_NOOP"),
        ("\treturn float4(0, 0, 0, 0);", "E_RENDER_BLANK"),
        ("\treturn float4(0.2, 0.4, 0.6, 1);", "E_RENDER_BLANK"),
        ("\tfloat4 c = tex0.Sample(smp, uv);\n\tfloat z = c.a - c.a;\n\treturn float4(c.rgb / z, c.a);", "E_RENDER_NAN"),
    ];
    for (body, want) in cases {
        let spec = with_body(body);
        let compiled = compile_spec(&gpu, &spec).unwrap_or_else(|e| panic!("{body}: {e:?}"));
        let (errors, _) = trial_run(&gpu, &spec, &compiled);
        assert_eq!(errors.len(), 1, "{body}: {errors:?}");
        assert_eq!(errors[0].code, want, "{body}");
    }
    // 乗算済みアルファを無視して明るくすると、警告になる(致命的ではない)
    let spec = with_body("\tfloat4 c = tex0.Sample(smp, uv);\n\treturn float4(c.rgb + 0.5, c.a);");
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let (errors, warnings) = trial_run(&gpu, &spec, &compiled);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(warnings.iter().any(|w| w.contains("乗算済み")), "{warnings:?}");
}

#[test]
fn samplers_and_blend_modes_behave_like_the_host_docs() {
    let gpu = gpu();
    let input = test_image(64, 64);
    // 画像の外(uv + 2)を読む: clip は透明、clamp は端の色
    let mut spec = with_body("\treturn tex0.Sample(smp, float2(0.5, uv.y + 2.0));");
    // テスト画像の下端中央のアルファは (28.8 - 31.5) / 8 + 1 = 0.6625(円の縁のぼかしの途中)
    for (sampler, want) in [(Sampler::Clip, 0.0), (Sampler::Dot, 0.0), (Sampler::Clamp, 0.6625)] {
        spec.passes[0].sampler = sampler;
        let compiled = compile_spec(&gpu, &spec).unwrap();
        let out = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &HashMap::new()).unwrap();
        let a = px(&out, 32, 32)[3];
        assert!(close(a, want), "{sampler:?}: {a} (期待 {want})");
    }
    // loop は 2 周して同じ場所を読む
    spec.passes[0].sampler = Sampler::Loop;
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let out = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &HashMap::new()).unwrap();
    assert!(close(px(&out, 10, 32)[3], 1.0));

    // add: 元の画素に加算される
    let mut spec = with_body("\treturn float4(0.25, 0, 0, 0);");
    spec.passes[0].blend = Blend::Add;
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let out = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &HashMap::new()).unwrap();
    let (i, o) = (px(&input, 32, 32), px(&out, 32, 32));
    assert!(close(o[0], i[0] + 0.25) && close(o[1], i[1]) && close(o[3], i[3]));
    // mask: アルファだけが掛かる
    let mut spec = with_body("\treturn float4(9, 9, 9, 0.5);");
    spec.passes[0].blend = Blend::Mask;
    let compiled = compile_spec(&gpu, &spec).unwrap();
    let out = render(&gpu, &spec, &compiled, &input, FrameInfo::default(), &HashMap::new()).unwrap();
    let (i, o) = (px(&input, 32, 32), px(&out, 32, 32));
    for c in 0..4 {
        assert!(close(o[c], i[c] * 0.5));
    }
}

#[test]
fn rgba8_roundtrip_handles_premultiplied_alpha() {
    let bytes = [255u8, 128, 0, 128, 10, 20, 30, 0, 0, 0, 0, 255, 255, 255, 255, 255];
    let img = Image::from_rgba8(2, 2, &bytes);
    assert!(close(img.data[0], 128.0 / 255.0)); // r * a
    let back = img.to_rgba8();
    assert_eq!(&back[..4], &[255, 128, 0, 128]);
    assert_eq!(&back[4..8], &[0, 0, 0, 0]); // 透明画素の色は捨てられる
    assert_eq!(&back[8..], &bytes[8..]);
}
