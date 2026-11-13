//! 文字を画像(BGRA、ストレートアルファ)にする。フォントは Windows のフォントフォルダから読む。

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use studio_core::project::{parse_rgba, TextStyle};

fn font_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Fonts")];
    if let Ok(l) = std::env::var("LOCALAPPDATA") {
        v.push(PathBuf::from(l).join("Microsoft").join("Windows").join("Fonts"));
    }
    v
}

/// フォントを読む(一度読んだものは使い回す)。見つからなければメイリオ、それも無ければエラー。
fn font(name: &str) -> Result<Arc<FontVec>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<FontVec>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(f) = cache.lock().unwrap().get(name) {
        return Ok(f.clone());
    }
    for candidate in [name, "meiryo", "YuGothM", "msgothic"] {
        for dir in font_dirs() {
            for ext in ["ttc", "ttf", "otf"] {
                let p = dir.join(format!("{candidate}.{ext}"));
                if let Ok(data) = std::fs::read(&p) {
                    if let Ok(f) = FontVec::try_from_vec_and_index(data, 0) {
                        let f = Arc::new(f);
                        cache.lock().unwrap().insert(name.to_string(), f.clone());
                        return Ok(f);
                    }
                }
            }
        }
    }
    Err(format!("フォント「{name}」が見つかりません"))
}

/// 文字を描いた画像。戻り値: (幅, 高さ, BGRA)
pub fn rasterize(text: &str, style: &TextStyle) -> Result<(u32, u32, Vec<u8>), String> {
    let f = font(&style.font)?;
    // size を 1 文字分(em)の大きさにする(フォントによって行の高さと em の比が違うため)
    let em = f.units_per_em().unwrap_or(1000.0);
    let sf = f.as_scaled(PxScale::from(style.size * f.height_unscaled() / em));
    let line_h = (sf.ascent() - sf.descent() + sf.line_gap()).ceil();
    let lines: Vec<&str> = if text.is_empty() { vec![" "] } else { text.split('\n').collect() };
    // 行ごとの幅
    let widths: Vec<f32> = lines
        .iter()
        .map(|l| {
            let mut w = 0.0;
            let mut prev = None;
            for c in l.chars() {
                let id = sf.glyph_id(c);
                if let Some(p) = prev {
                    w += sf.kern(p, id);
                }
                w += sf.h_advance(id);
                prev = Some(id);
            }
            w
        })
        .collect();
    let pad = style.outline.ceil() as u32 + 2;
    let text_w = widths.iter().cloned().fold(1.0f32, f32::max).ceil() as u32;
    let (w, h) = (text_w + pad * 2, (line_h * lines.len() as f32).ceil() as u32 + pad * 2);
    if w as u64 * h as u64 > 8192 * 8192 {
        return Err("文字の画像が大きすぎます".into());
    }
    let mut cov = vec![0.0f32; (w * h) as usize];
    for (li, l) in lines.iter().enumerate() {
        let x0 = match style.align.as_str() {
            "left" => 0.0,
            "right" => text_w as f32 - widths[li],
            _ => (text_w as f32 - widths[li]) / 2.0,
        } + pad as f32;
        let baseline = pad as f32 + li as f32 * line_h + sf.ascent();
        let mut x = x0;
        let mut prev = None;
        for c in l.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                x += sf.kern(p, id);
            }
            let g = id.with_scale_and_position(sf.scale(), ab_glyph::point(x, baseline));
            if let Some(o) = f.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, c| {
                    let (px, py) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                    if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                        let i = (py as u32 * w + px as u32) as usize;
                        cov[i] = cov[i].max(c);
                    }
                });
            }
            x += sf.h_advance(id);
            prev = Some(id);
        }
    }
    // 縁取り: 文字の形を太らせる(円形に広げ、端は滑らかに)
    let outline = if style.outline > 0.0 {
        let r = style.outline;
        let ri = r.ceil() as i64;
        let offsets: Vec<(i64, i64, f32)> = (-ri..=ri)
            .flat_map(|dy| (-ri..=ri).map(move |dx| (dx, dy)))
            .filter_map(|(dx, dy)| {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                let wgt = (r + 0.5 - d).clamp(0.0, 1.0);
                (wgt > 0.0).then_some((dx, dy, wgt))
            })
            .collect();
        let mut o = vec![0.0f32; cov.len()];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let mut m = 0.0f32;
                for &(dx, dy, wgt) in &offsets {
                    let (sx, sy) = (x + dx, y + dy);
                    if sx >= 0 && sy >= 0 && sx < w as i64 && sy < h as i64 {
                        m = m.max(cov[(sy * w as i64 + sx) as usize] * wgt);
                        if m >= 1.0 {
                            break;
                        }
                    }
                }
                o[(y * w as i64 + x) as usize] = m;
            }
        }
        Some(o)
    } else {
        None
    };
    let fill = parse_rgba(&style.color).ok_or("色の指定が不正です")?;
    let oc = parse_rgba(&style.outline_color).ok_or("縁取りの色の指定が不正です")?;
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    for i in 0..cov.len() {
        let af = cov[i] * fill[3] as f32 / 255.0;
        let ao = outline.as_ref().map_or(0.0, |o| o[i] * oc[3] as f32 / 255.0);
        let a = af + ao * (1.0 - af);
        if a <= 0.0 {
            continue;
        }
        for (k, ch) in [2usize, 1, 0].into_iter().enumerate() {
            // BGRA の並び: k=0 → B(色の 2 番目)...
            let v = (fill[ch] as f32 * af + oc[ch] as f32 * ao * (1.0 - af)) / a;
            bgra[i * 4 + k] = v.round().clamp(0.0, 255.0) as u8;
        }
        bgra[i * 4 + 3] = (a * 255.0).round() as u8;
    }
    Ok((w, h, bgra))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_japanese_with_outline() {
        let mut st = TextStyle { size: 48.0, color: "#FF0000".into(), ..Default::default() };
        let (w, h, px) = rasterize("あいう\nABC", &st).unwrap();
        assert!(w > 100 && h > 90, "{w}x{h}");
        let red = px.chunks_exact(4).filter(|p| p[3] > 200 && p[2] > 200 && p[1] < 50).count();
        assert!(red > 300, "赤い画素が少ない: {red}");
        st.outline = 4.0;
        st.outline_color = "#0000FF".into();
        let (w2, h2, px2) = rasterize("あいう\nABC", &st).unwrap();
        assert!(w2 > w && h2 > h);
        let blue = px2.chunks_exact(4).filter(|p| p[3] > 200 && p[0] > 200 && p[2] < 50).count();
        assert!(blue > 300, "縁取りが無い: {blue}");
        assert!(rasterize("x", &TextStyle { font: "no_such_font_zzz".into(), ..Default::default() }).is_ok(), "見つからないフォントはメイリオで代用");
    }
}
