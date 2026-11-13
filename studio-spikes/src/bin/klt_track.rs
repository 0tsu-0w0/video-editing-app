//! 検証6: 点トラッキング(ピラミッド Lucas-Kanade)を、外部ライブラリなしの Rust で実用精度にできるか。
//!
//! 正解が分かっている合成映像を作る: 大きな模様画像を、カメラが動くように
//! 平行移動・回転・拡大縮小しながら切り出し、ノイズとぼけを加える(1280x720、300 フレーム)。
//! 画面上の点を追跡し、正解との誤差・ドリフト・見失いの検出・速度を測る。
//!
//! 比べる方式:
//! - 連続追跡: 前フレーム→次フレームを毎回追う(誤差が積み重なる)
//! - 基準つき追跡: 連続追跡の結果を、最初のフレームの見た目(テンプレート)に合わせて補正する

use std::time::Instant;

const FW: usize = 1280;
const FH: usize = 720;
const FRAMES: usize = 300;
const LEVELS: usize = 4;
const HALF: isize = 7; // 窓 15x15

#[derive(Clone)]
struct Img {
    w: usize,
    h: usize,
    p: Vec<f32>,
}

impl Img {
    fn new(w: usize, h: usize) -> Self {
        Self { w, h, p: vec![0.0; w * h] }
    }
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.p[y * self.w + x]
    }
    /// 双線形補間
    #[inline]
    fn sample(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let b = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        a * (1.0 - fy) + b * fy
    }
    /// [1 2 1]/4 でぼかして 1/2 に縮小
    fn down(&self) -> Img {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut o = Img::new(w, h);
        let k = [1.0, 2.0, 1.0];
        for y in 0..h {
            for x in 0..w {
                let mut s = 0.0;
                for (j, ky) in k.iter().enumerate() {
                    for (i, kx) in k.iter().enumerate() {
                        s += kx * ky * self.at(2 * x as isize + i as isize - 1, 2 * y as isize + j as isize - 1);
                    }
                }
                o.p[y * w + x] = s / 16.0;
            }
        }
        o
    }
}

fn pyramid(img: &Img) -> Vec<Img> {
    let mut v = vec![img.clone()];
    for _ in 1..LEVELS {
        let d = v.last().unwrap().down();
        v.push(d);
    }
    v
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f32 / (1u64 << 53) as f32
    }
    fn gauss(&mut self) -> f32 {
        let (u, v) = (self.next().max(1e-9), self.next());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
}

/// 実写に近い模様: 大小さまざまな、輪郭のはっきりした円と四角形を重ね、
/// 全体を軽くぼかす(レンズのぼけ相当)。
fn texture(size: usize, rng: &mut Rng) -> Img {
    let mut t = Img::new(size, size);
    for v in &mut t.p {
        *v = 0.5;
    }
    for &(count, radius) in &[(80, 160.0f32), (1500, 40.0), (12000, 10.0)] {
        for _ in 0..count {
            let (cx, cy) = (rng.next() * size as f32, rng.next() * size as f32);
            let r = radius * (0.4 + rng.next());
            let val = 0.1 + 0.8 * rng.next();
            let square = rng.next() < 0.5;
            let (x0, x1) = ((cx - r).max(0.0) as usize, ((cx + r) as usize).min(size - 1));
            let (y0, y1) = ((cy - r).max(0.0) as usize, ((cy + r) as usize).min(size - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let inside = square || (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2) <= r * r;
                    if inside {
                        t.p[y * size + x] = val;
                    }
                }
            }
        }
    }
    // 3x3 の軽いぼかし
    let src = t.clone();
    for y in 0..size {
        for x in 0..size {
            let mut s = 0.0;
            for j in -1..=1isize {
                for i in -1..=1isize {
                    let w = if i == 0 && j == 0 { 4.0 } else if i == 0 || j == 0 { 2.0 } else { 1.0 };
                    s += w * src.at(x as isize + i, y as isize + j);
                }
            }
            t.p[y * size + x] = s / 16.0;
        }
    }
    t
}

/// フレーム k のカメラ: 画面座標 → 模様画像の座標。
#[derive(Clone, Copy)]
struct Cam {
    cx: f32,
    cy: f32,
    angle: f32,
    scale: f32,
}

fn camera(k: usize) -> Cam {
    let t = k as f32 / FRAMES as f32 * std::f32::consts::TAU;
    Cam {
        cx: 1024.0 + 260.0 * t.sin() + 40.0 * (3.0 * t).sin(),
        cy: 1024.0 + 180.0 * (2.0 * t).sin(),
        angle: if std::env::var("KLT_RIGID").is_ok() { 0.0 } else { 0.12 * (1.5 * t).sin() }, // ±約 7 度
        scale: if std::env::var("KLT_RIGID").is_ok() { 1.0 } else { 1.0 + 0.12 * (0.8 * t).sin() }, // ±12%
    }
}

impl Cam {
    fn to_tex(&self, x: f32, y: f32) -> (f32, f32) {
        let (dx, dy) = ((x - FW as f32 / 2.0) * self.scale, (y - FH as f32 / 2.0) * self.scale);
        let (s, c) = self.angle.sin_cos();
        (self.cx + c * dx - s * dy, self.cy + s * dx + c * dy)
    }
    fn to_screen(&self, u: f32, v: f32) -> (f32, f32) {
        let (dx, dy) = (u - self.cx, v - self.cy);
        let (s, c) = self.angle.sin_cos();
        let (rx, ry) = (c * dx + s * dy, -s * dx + c * dy);
        (rx / self.scale + FW as f32 / 2.0, ry / self.scale + FH as f32 / 2.0)
    }
}

fn render(tex: &Img, cam: Cam, rng: &mut Rng) -> Img {
    let mut f = Img::new(FW, FH);
    for y in 0..FH {
        for x in 0..FW {
            let (u, v) = cam.to_tex(x as f32 + 0.5, y as f32 + 0.5);
            f.p[y * FW + x] = (tex.sample(u - 0.5, v - 0.5) + rng.gauss() * if std::env::var("KLT_NOISE0").is_ok() { 0.0 } else { 0.02 }).clamp(0.0, 1.0);
        }
    }
    f
}

/// 1 階層での LK。prev の点 p の窓を、next の p+d に合わせる d を求める。
/// 戻り値: (d, 収束したか, 最小固有値)
fn lk_level(prev: &Img, next: &Img, p: (f32, f32), mut d: (f32, f32)) -> ((f32, f32), bool, f32) {
    let n = ((2 * HALF + 1) * (2 * HALF + 1)) as usize;
    let mut ix = Vec::with_capacity(n);
    let mut iy = Vec::with_capacity(n);
    let mut i0 = Vec::with_capacity(n);
    let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
    for wy in -HALF..=HALF {
        for wx in -HALF..=HALF {
            let (x, y) = (p.0 + wx as f32, p.1 + wy as f32);
            let gx = (prev.sample(x + 1.0, y) - prev.sample(x - 1.0, y)) * 0.5;
            let gy = (prev.sample(x, y + 1.0) - prev.sample(x, y - 1.0)) * 0.5;
            gxx += gx * gx;
            gxy += gx * gy;
            gyy += gy * gy;
            ix.push(gx);
            iy.push(gy);
            i0.push(prev.sample(x, y));
        }
    }
    let det = gxx * gyy - gxy * gxy;
    let tr = gxx + gyy;
    let min_eig = (tr - ((gxx - gyy).powi(2) + 4.0 * gxy * gxy).sqrt()) / 2.0 / n as f32;
    if det.abs() < 1e-9 {
        return (d, false, min_eig);
    }
    for _ in 0..20 {
        let (mut bx, mut by) = (0.0f32, 0.0f32);
        let mut k = 0;
        for wy in -HALF..=HALF {
            for wx in -HALF..=HALF {
                let diff = i0[k] - next.sample(p.0 + d.0 + wx as f32, p.1 + d.1 + wy as f32);
                bx += diff * ix[k];
                by += diff * iy[k];
                k += 1;
            }
        }
        let (sx, sy) = ((gyy * bx - gxy * by) / det, (gxx * by - gxy * bx) / det);
        d = (d.0 + sx, d.1 + sy);
        if sx * sx + sy * sy < 1e-4 {
            return (d, true, min_eig);
        }
    }
    (d, true, min_eig)
}

/// ピラミッド LK。prev の点 p が next でどこにあるか。
fn track(prev: &[Img], next: &[Img], p: (f32, f32)) -> Option<(f32, f32)> {
    let mut g = (0.0f32, 0.0f32);
    for l in (0..LEVELS).rev() {
        let s = (1 << l) as f32;
        let pl = (p.0 / s, p.1 / s);
        let (d, ok, _) = lk_level(&prev[l], &next[l], pl, g);
        if !ok {
            return None;
        }
        g = if l > 0 { (d.0 * 2.0, d.1 * 2.0) } else { d };
    }
    let q = (p.0 + g.0, p.1 + g.1);
    if q.0 < 0.0 || q.1 < 0.0 || q.0 >= FW as f32 || q.1 >= FH as f32 {
        return None;
    }
    Some(q)
}


/// 窓どうしの正規化相互相関(-1〜1)。明るさ・コントラストの変化に影響されにくい。
fn ncc(a: &Img, pa: (f32, f32), b: &Img, pb: (f32, f32)) -> f32 {
    let n = ((2 * HALF + 1) * (2 * HALF + 1)) as f32;
    let (mut va, mut vb) = (Vec::with_capacity(n as usize), Vec::with_capacity(n as usize));
    for wy in -HALF..=HALF {
        for wx in -HALF..=HALF {
            va.push(a.sample(pa.0 + wx as f32, pa.1 + wy as f32));
            vb.push(b.sample(pb.0 + wx as f32, pb.1 + wy as f32));
        }
    }
    let (ma, mb) = (va.iter().sum::<f32>() / n, vb.iter().sum::<f32>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for i in 0..va.len() {
        let (x, y) = (va[i] - ma, vb[i] - mb);
        sab += x * y;
        saa += x * x;
        sbb += y * y;
    }
    sab / (saa * sbb).sqrt().max(1e-9)
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn main() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let t = Instant::now();
    let tex = texture(2048, &mut rng);
    let frames: Vec<Img> = (0..FRAMES).map(|k| render(&tex, camera(k), &mut rng)).collect();
    let pyrs: Vec<Vec<Img>> = frames.iter().map(pyramid).collect();
    println!("[準備] 合成映像 {}x{} {} フレーム({:.1} 秒)", FW, FH, FRAMES, t.elapsed().as_secs_f64());

    // 追跡する点: 画面の格子ごとに、周辺 64x64 の中で最小固有値が最大の点(角になっている点)を選ぶ
    let mut starts = Vec::new();
    for gy in 1..6 {
        for gx in 1..8 {
            let c = (gx as f32 * FW as f32 / 8.0, gy as f32 * FH as f32 / 6.0);
            let mut best = (c, 0.0f32);
            for dy in (-32..=32).step_by(4) {
                for dx in (-32..=32).step_by(4) {
                    let p = (c.0 + dx as f32, c.1 + dy as f32);
                    let (_, _, e) = lk_level(&frames[0], &frames[0], p, (0.0, 0.0));
                    if e > best.1 {
                        best = (p, e);
                    }
                }
            }
            if best.1 > 2e-3 {
                starts.push(best.0);
            }
        }
    }
    // 正解(模様画像上の位置を固定し、各フレームの画面座標へ戻す)
    let anchors: Vec<(f32, f32)> = starts.iter().map(|p| camera(0).to_tex(p.0 + 0.5, p.1 + 0.5)).collect();
    let truth = |k: usize, i: usize| { let (x, y) = camera(k).to_screen(anchors[i].0, anchors[i].1); (x - 0.5, y - 0.5) };

    if std::env::var("KLT_DEBUG").is_ok() {
        for (i, p) in starts.iter().enumerate().take(8) {
            let q = track(&pyrs[0], &pyrs[1], *p);
            let tr = truth(1, i);
            let (_, _, e) = lk_level(&frames[0], &frames[0], *p, (0.0, 0.0));
            println!("  点{i} 誤差 {:.3}px(最小固有値 {:.5})", q.map(|q| dist(q, tr)).unwrap_or(f32::NAN), e);
        }
        // 同じ画像どうし(移動 0)
        let q = track(&pyrs[0], &pyrs[0], starts[0]);
        println!("  同一画像: {:?} → {:?}", starts[0], q);
        // 既知の平行移動: フレーム0 を 3.3px, -2.1px ずらした画像
        let mut sh = Img::new(FW, FH);
        for y in 0..FH { for x in 0..FW { sh.p[y*FW+x] = frames[0].sample(x as f32 - 3.3, y as f32 + 2.1); } }
        let q = track(&pyrs[0], &pyramid(&sh), starts[0]);
        println!("  平行移動(+3.3,-2.1): {:?} → {:?}", starts[0], q);
        return;
    }
    for mode in ["連続追跡", "基準つき追跡"] {
        let t = Instant::now();
        let mut pos: Vec<Option<(f32, f32)>> = starts.iter().map(|p| Some(*p)).collect();
        let mut err_at = vec![Vec::new(); FRAMES];
        let mut fb_flag = 0;
        let mut calls = 0usize;
        let (mut lost_out, mut lost_look) = (0, 0);
        for k in 1..FRAMES {
            for i in 0..pos.len() {
                let Some(p) = pos[i] else { continue };
                calls += 1;
                let Some(mut q) = track(&pyrs[k - 1], &pyrs[k], p) else {
                    lost_out += 1;
                    pos[i] = None;
                    continue;
                };
                // 前向き/後ろ向きの往復で戻り位置がずれたら、信頼できない
                if let Some(back) = track(&pyrs[k], &pyrs[k - 1], q) {
                    if dist(back, p) > 0.5 {
                        fb_flag += 1;
                    }
                }
                if mode == "基準つき追跡" {
                    // 最初のフレームの窓に直接合わせ直す(誤差の積み重なりを打ち消す)
                    if let Some(r) = track(&pyrs[0], &pyrs[k], starts[i]).filter(|r| dist(*r, q) < 3.0) {
                        q = r;
                    }
                }
                // 直前のフレームと見た目が大きく違えば見失い(カット・隠れ)
                if ncc(&frames[k - 1], p, &frames[k], q) < 0.8 {
                    lost_look += 1;
                    pos[i] = None;
                    continue;
                }
                pos[i] = Some(q);
                err_at[k].push(dist(q, truth(k, i)));
            }
        }
        let secs = t.elapsed().as_secs_f64();
        let alive = pos.iter().filter(|p| p.is_some()).count();
        let all: Vec<f32> = err_at.iter().flatten().cloned().collect();
        let mean = all.iter().sum::<f32>() / all.len() as f32;
        let mut sorted = all.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p95 = sorted[sorted.len() * 95 / 100];
        let last = &err_at[FRAMES - 1];
        let last_mean = last.iter().sum::<f32>() / last.len().max(1) as f32;
        let at100 = &err_at[100];
        let m100 = at100.iter().sum::<f32>() / at100.len().max(1) as f32;
        let gone = (0..starts.len()).filter(|i| { let t = truth(FRAMES - 1, *i); t.0 < 8.0 || t.1 < 8.0 || t.0 > FW as f32 - 8.0 || t.1 > FH as f32 - 8.0 }).count();
        println!(
            "[{mode}] 点 {} 個 → 最後まで残った点 {}(画面外へ出て終了 {} / 見た目の変化で見失い {}。正解でも最終フレームで画面外の点 {})",
            starts.len(), alive, lost_out, lost_look, gone
        );
        println!(
            "    誤差: 平均 {:.3}px / 95% 点 {:.3}px / 100 フレーム目 {:.3}px / 300 フレーム目 {:.3}px",
            mean, p95, m100, last_mean
        );
        println!(
            "    往復チェックで要注意となった回数 {} / 速度: 1 点 1 フレームあたり {:.3} ms(30 点を 30fps なら 1 秒の映像に {:.0} ms)",
            fb_flag,
            secs * 1000.0 / calls as f64,
            secs * 1000.0 / calls as f64 * 30.0 * 30.0
        );
    }

    // 見失いの検出: 途中で別の映像に切り替わる(カット)と、点は見失いになるべき
    let other = render(&texture(2048, &mut Rng(42)), camera(150), &mut rng);
    let other_pyr = pyramid(&other);
    let mut lost = 0;
    for p in &starts {
        let ok = track(&pyrs[149], &other_pyr, *p).map(|q| ncc(&frames[149], *p, &other, q) >= 0.8).unwrap_or(false);
        if !ok {
            lost += 1;
        }
    }
    println!("[見失いの検出] 別の映像に切り替えたとき、{} 点中 {} 点を見失いと判定", starts.len(), lost);
}
