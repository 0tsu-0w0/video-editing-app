//! 試走結果の判定と、合成テスト画像。

use crate::Image;

#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub code: &'static str,
    pub fatal: bool,
    pub message: String,
}

/// グラデーション + 市松 + 半透明の縁を持つ円(乗算済みアルファ)。
/// 色・明るさ・位置・透明度のどれを触っても差が出るようにしてある。
pub fn test_image(width: u32, height: u32) -> Image {
    let mut img = Image::new(width, height);
    let (cx, cy) = (width as f32 / 2.0, height as f32 / 2.0);
    let radius = height as f32 * 0.45;
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let u = fx / width as f32;
            let v = fy / height as f32;
            let check = if ((x / 16) + (y / 16)) % 2 == 0 { 1.0 } else { 0.55 };
            let r = u * check;
            let g = v * check;
            let b = (1.0 - u) * (0.4 + 0.6 * check);
            // 円の外へ向かって 8px かけて透明になる
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let a = ((radius - d) / 8.0 + 1.0).clamp(0.0, 1.0);
            let i = ((y * width + x) * 4) as usize;
            img.data[i] = r * a;
            img.data[i + 1] = g * a;
            img.data[i + 2] = b * a;
            img.data[i + 3] = a;
        }
    }
    img
}

fn max_abs_diff(a: &Image, b: &Image) -> f32 {
    a.data.iter().zip(&b.data).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

/// 入力と、複数時刻の出力から問題を探す。
pub fn analyze(input: &Image, outputs: &[Image]) -> Vec<Finding> {
    let mut out = Vec::new();
    let Some(first) = outputs.first() else { return out };
    let fatal = |code, message: &str| Finding { code, fatal: true, message: message.to_string() };

    if outputs.iter().any(|o| o.data.iter().any(|v| !v.is_finite())) {
        out.push(fatal(
            "E_RENDER_NAN",
            "出力に NaN または無限大があります。0 での除算(a が 0 のときの rgb / a など)や、負の数の pow / sqrt / log を避けてください",
        ));
        return out;
    }
    if outputs.iter().all(|o| o.data.chunks_exact(4).all(|p| p[3] <= 1e-4)) {
        out.push(fatal(
            "E_RENDER_BLANK",
            "出力が全て透明です。戻り値のアルファ(a)に入力のアルファを入れてください",
        ));
        return out;
    }
    let uniform = |o: &Image| {
        let p0 = &o.data[..4];
        o.data.chunks_exact(4).all(|p| p.iter().zip(p0).all(|(a, b)| (a - b).abs() < 1e-4))
    };
    if outputs.iter().all(uniform) {
        out.push(fatal(
            "E_RENDER_BLANK",
            "出力が全面同じ色です。入力画像(tex0)を読んで反映してください",
        ));
        return out;
    }
    if outputs.iter().all(|o| max_abs_diff(input, o) < 1.0 / 255.0) {
        out.push(fatal(
            "E_RENDER_NOOP",
            "既定値のままでは入力と出力に差がありません。既定値で効果がはっきり見えるようにしてください",
        ));
        return out;
    }

    // ---- 警告(加算系の効果では正当な場合もあるので致命的にしない)
    let n = (first.width * first.height) as usize;
    let over = first
        .data
        .chunks_exact(4)
        .filter(|p| p[..3].iter().any(|c| *c > p[3] + 0.02))
        .count();
    if over * 20 > n {
        out.push(Finding {
            code: "W_PREMULT",
            fatal: false,
            message: format!(
                "画素の {}% で rgb がアルファを超えています。乗算済みアルファの扱い(rgb に a を掛け直す)を確認してください。発光・加算の効果なら問題ありません",
                over * 100 / n
            ),
        });
    }
    let neg = first.data.iter().filter(|v| **v < -0.02).count();
    if neg > 0 {
        out.push(Finding {
            code: "W_NEGATIVE",
            fatal: false,
            message: "出力に負の値があります。saturate / max(…, 0) で抑えることを検討してください".to_string(),
        });
    }
    out
}
