//! 検証7: 平面トラッキング(壁のポスター・机の上など)。
//!
//! 正解が分かる合成映像: 背景(別の動き)の手前に、立体的に傾きながら動く平面(ポスター)があり、
//! 途中で手前を物体が横切って平面の一部を隠す(1280x720、240 フレーム、ノイズあり)。
//! 利用者がフレーム 0 で平面の四隅を指定した想定で、各フレームの四隅を推定し、正解と比べる。
//!
//! 方式: 平面の内側の「角」になっている点を多数選んで点トラッキングし、
//! 最初のフレームの位置との対応から RANSAC でホモグラフィ(平面の射影変換)を求める。
//! 隠れ・見失いで点が減ったら、現在の平面の内側から点を補充する。

use std::time::Instant;
use studio_spikes::*;

const FW: usize = 1280;
const FH: usize = 720;
const FRAMES: usize = 240;
const POSTER: f32 = 1024.0;

/// フレーム k の平面の四隅(画面座標)。平面を 3D で回転・移動させて透視投影する。
fn plane_corners(k: usize) -> [(f32, f32); 4] {
    let t = k as f32 / FRAMES as f32 * std::f32::consts::TAU;
    let ay = 0.6 * t.sin(); // Y 軸回転 ±34°
    let ax = 0.25 * (2.0 * t).sin(); // X 軸回転 ±14°
    let (tx, ty, tz) = (120.0 * (0.5 * t).sin(), 50.0 * t.cos(), 1400.0 + 250.0 * (1.5 * t).sin());
    let (hw, hh) = (300.0f32, 210.0f32);
    let f = 1100.0;
    [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(x, y)| {
        // Y 軸回転 → X 軸回転 → 移動 → 投影
        let (x1, z1) = (x * ay.cos(), -x * ay.sin());
        let (y2, z2) = (y * ax.cos() - z1 * ax.sin(), y * ax.sin() + z1 * ax.cos());
        let (x3, y3, z3) = (x1 + tx, y2 + ty, z2 + tz);
        (FW as f32 / 2.0 + f * x3 / z3, FH as f32 / 2.0 + f * y3 / z3)
    })
}

fn poster_to_screen(k: usize) -> Hom {
    let src = [(0.0, 0.0), (POSTER, 0.0), (POSTER, POSTER * 0.7), (0.0, POSTER * 0.7)];
    fit_homography(&src, &plane_corners(k)).unwrap()
}

/// 手前を横切る物体(フレーム 90〜170)。
fn occluder(k: usize) -> Option<(f32, f32, f32, f32)> {
    if !(90..170).contains(&k) {
        return None;
    }
    let x = -300.0 + (k - 90) as f32 / 80.0 * 1900.0;
    Some((x, 120.0, x + 260.0, 640.0))
}

fn render(k: usize, bg: &Img, poster: &Img, occ: &Img, rng: &mut Rng) -> Img {
    let inv = invert(&poster_to_screen(k));
    let quad = plane_corners(k);
    let t = k as f32 / FRAMES as f32 * std::f32::consts::TAU;
    let (bx, by) = (500.0 + 200.0 * t.sin(), 500.0 + 80.0 * (2.0 * t).cos()); // 背景は別の動き
    let o = occluder(k);
    let mut f = Img::new(FW, FH);
    for y in 0..FH {
        for x in 0..FW {
            let (sx, sy) = (x as f32 + 0.5, y as f32 + 0.5);
            let v = if let Some((x0, y0, _x1, _y1)) = o.filter(|r| sx >= r.0 && sx < r.2 && sy >= r.1 && sy < r.3) {
                occ.sample(sx - x0 + 300.0, sy - y0 + 300.0) * 0.6
            } else if inside_quad(&quad, (sx, sy)) {
                let (u, v) = apply(&inv, (sx, sy));
                poster.sample(u - 0.5, v - 0.5)
            } else {
                bg.sample(sx + bx - 0.5, sy + by - 0.5) * 0.8
            };
            f.p[y * FW + x] = (v + rng.gauss() * 0.02).clamp(0.0, 1.0);
        }
    }
    f
}

struct Feature {
    reference: (f32, f32), // フレーム 0 での位置
    pos: (f32, f32),
}

/// 現在の四角形の内側(縁から少し内側)で、既存の点から離れた「角」を選ぶ。
fn detect(img: &Img, quad: &[(f32, f32); 4], existing: &[(f32, f32)], want: usize) -> Vec<(f32, f32)> {
    let (minx, maxx) = quad.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (miny, maxy) = quad.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let shrink = {
        let c = quad.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / 4.0, a.1 + p.1 / 4.0));
        quad.map(|p| (c.0 + (p.0 - c.0) * 0.92, c.1 + (p.1 - c.1) * 0.92))
    };
    let mut cand = Vec::new();
    let mut y = miny.max(10.0);
    while y < maxy.min(FH as f32 - 10.0) {
        let mut x = minx.max(10.0);
        while x < maxx.min(FW as f32 - 10.0) {
            if inside_quad(&shrink, (x, y)) && existing.iter().all(|e| dist(*e, (x, y)) > 14.0) {
                let s = corner_score(img, (x, y));
                if s > 2e-3 {
                    cand.push(((x, y), s));
                }
            }
            x += 12.0;
        }
        y += 12.0;
    }
    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    cand.into_iter().take(want).map(|c| c.0).collect()
}

fn main() {
    let mut rng = Rng(0x2545F4914F6CDD1D);
    let t0 = Instant::now();
    let bg = texture(2048, &mut rng, 1.0);
    let poster = texture(1024, &mut rng, 0.6);
    let occ = texture(1024, &mut rng, 1.5);
    let frames: Vec<Img> = (0..FRAMES).map(|k| render(k, &bg, &poster, &occ, &mut rng)).collect();
    println!("[準備] 合成映像 {}x{} {} フレーム({:.1} 秒)", FW, FH, FRAMES, t0.elapsed().as_secs_f64());
    if let Some(dir) = std::env::args().nth(1) {
        for k in [0, 60, 130, 200] {
            let px: Vec<u8> = frames[k].p.iter().map(|v| (v * 255.0) as u8).collect();
            image::save_buffer(format!("{dir}/planar_{k}.png"), &px, FW as u32, FH as u32, image::ExtendedColorType::L8).unwrap();
        }
    }

    let t = Instant::now();
    let corners0 = plane_corners(0);
    let mut prev_pyr = pyramid(&frames[0]);
    let mut feats: Vec<Feature> = detect(&frames[0], &corners0, &[], 200)
        .into_iter()
        .map(|p| Feature { reference: p, pos: p })
        .collect();
    let mut h_cur: Hom = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut errs = Vec::new();
    let mut occ_errs = Vec::new();
    let (mut min_feats, mut refills, mut fails) = (feats.len(), 0, 0);
    for k in 1..FRAMES {
        let pyr = pyramid(&frames[k]);
        // 1. 点を追う(往復チェック + 見た目のチェック)
        feats.retain_mut(|f| {
            let Some(q) = track(&prev_pyr, &pyr, f.pos) else { return false };
            let Some(back) = track(&pyr, &prev_pyr, q) else { return false };
            if dist(back, f.pos) > 0.7 || ncc(&frames[k - 1], f.pos, &frames[k], q) < 0.8 {
                return false;
            }
            f.pos = q;
            true
        });
        // 2. RANSAC でホモグラフィ(フレーム 0 → フレーム k)
        let src: Vec<_> = feats.iter().map(|f| f.reference).collect();
        let dst: Vec<_> = feats.iter().map(|f| f.pos).collect();
        match ransac_homography(&src, &dst, 1.5, &mut rng) {
            Some((h, inliers)) => {
                h_cur = h;
                let keep: std::collections::HashSet<usize> = inliers.into_iter().collect();
                let mut i = 0;
                feats.retain(|_| {
                    let k = keep.contains(&i);
                    i += 1;
                    k
                });
            }
            None => fails += 1, // 推定できないときは前の値を使う
        }
        min_feats = min_feats.min(feats.len());
        let est = corners0.map(|c| apply(&h_cur, c));
        let truth = plane_corners(k);
        let e = est.iter().zip(&truth).map(|(a, b)| dist(*a, *b)).sum::<f32>() / 4.0;
        errs.push(e);
        if occluder(k).is_some() {
            occ_errs.push(e);
        }
        // 3. 点が減ったら補充(現在の平面の内側から選び、フレーム 0 の位置へ戻す)
        if feats.len() < 120 {
            let existing: Vec<_> = feats.iter().map(|f| f.pos).collect();
            let inv = invert(&h_cur);
            for p in detect(&frames[k], &est, &existing, 200 - feats.len()) {
                feats.push(Feature { reference: apply(&inv, p), pos: p });
            }
            refills += 1;
        }
        prev_pyr = pyr;
    }
    let secs = t.elapsed().as_secs_f64();
    let mean = errs.iter().sum::<f32>() / errs.len() as f32;
    let mut sorted = errs.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let occ_mean = occ_errs.iter().sum::<f32>() / occ_errs.len().max(1) as f32;
    let occ_max = occ_errs.iter().cloned().fold(0.0f32, f32::max);
    println!("[平面トラッキング] 四隅の誤差(4 隅の平均):");
    println!("    全体: 平均 {:.2}px / 95% 点 {:.2}px / 最大 {:.2}px / 最終フレーム {:.2}px",
        mean, sorted[sorted.len() * 95 / 100], sorted.last().unwrap(), errs.last().unwrap());
    println!("    物体が手前を横切っている間: 平均 {:.2}px / 最大 {:.2}px", occ_mean, occ_max);
    println!("    追跡中の点の最少数 {} / 補充 {} 回 / 推定できなかったフレーム {}", min_feats, refills, fails);
    println!("    速度: {:.1} ms/フレーム(1280x720、CPU 1 スレッド、ピラミッド作成込み)", secs * 1000.0 / (FRAMES - 1) as f64);
}
