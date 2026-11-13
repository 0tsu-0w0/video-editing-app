//! 検証プログラム間で共有する画像処理(グレー画像、ピラミッド、Lucas-Kanade、ホモグラフィ)。

pub const LEVELS: usize = 4;
pub const HALF: isize = 7; // 窓 15x15

#[derive(Clone)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub p: Vec<f32>,
}

impl Img {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, p: vec![0.0; w * h] }
    }
    #[inline]
    pub fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1) as usize;
        let y = y.clamp(0, self.h as isize - 1) as usize;
        self.p[y * self.w + x]
    }
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let a = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let b = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        a * (1.0 - fy) + b * fy
    }
    pub fn down(&self) -> Img {
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

pub fn pyramid(img: &Img) -> Vec<Img> {
    let mut v = vec![img.clone()];
    for _ in 1..LEVELS {
        let d = v.last().unwrap().down();
        v.push(d);
    }
    v
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f32 / (1u64 << 53) as f32
    }
    pub fn gauss(&mut self) -> f32 {
        let (u, v) = (self.next().max(1e-9), self.next());
        (-2.0 * u.ln()).sqrt() * (std::f32::consts::TAU * v).cos()
    }
    pub fn below(&mut self, n: usize) -> usize {
        ((self.next() * n as f32) as usize).min(n - 1)
    }
}

/// 実写に近い模様(輪郭のはっきりした円と四角形 + 軽いぼかし)。
pub fn texture(size: usize, rng: &mut Rng, scale: f32) -> Img {
    let mut t = Img::new(size, size);
    t.p.fill(0.5);
    for &(count, radius) in &[(80usize, 160.0f32), (1500, 40.0), (12000, 10.0)] {
        let count = (count as f32 * (size * size) as f32 / (2048.0 * 2048.0)) as usize;
        for _ in 0..count.max(1) {
            let (cx, cy) = (rng.next() * size as f32, rng.next() * size as f32);
            let r = radius * scale * (0.4 + rng.next());
            let val = 0.1 + 0.8 * rng.next();
            let square = rng.next() < 0.5;
            let (x0, x1) = ((cx - r).max(0.0) as usize, ((cx + r) as usize).min(size - 1));
            let (y0, y1) = ((cy - r).max(0.0) as usize, ((cy + r) as usize).min(size - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if square || (x as f32 - cx).powi(2) + (y as f32 - cy).powi(2) <= r * r {
                        t.p[y * size + x] = val;
                    }
                }
            }
        }
    }
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

/// 窓の最小固有値(大きいほど「角」で追跡しやすい)。
pub fn corner_score(img: &Img, p: (f32, f32)) -> f32 {
    let (mut gxx, mut gxy, mut gyy) = (0.0f32, 0.0f32, 0.0f32);
    for wy in -HALF..=HALF {
        for wx in -HALF..=HALF {
            let (x, y) = (p.0 + wx as f32, p.1 + wy as f32);
            let gx = (img.sample(x + 1.0, y) - img.sample(x - 1.0, y)) * 0.5;
            let gy = (img.sample(x, y + 1.0) - img.sample(x, y - 1.0)) * 0.5;
            gxx += gx * gx;
            gxy += gx * gy;
            gyy += gy * gy;
        }
    }
    let n = ((2 * HALF + 1) * (2 * HALF + 1)) as f32;
    ((gxx + gyy) - ((gxx - gyy).powi(2) + 4.0 * gxy * gxy).sqrt()) / 2.0 / n
}

fn lk_level(prev: &Img, next: &Img, p: (f32, f32), mut d: (f32, f32)) -> Option<(f32, f32)> {
    let n = ((2 * HALF + 1) * (2 * HALF + 1)) as usize;
    let (mut ix, mut iy, mut i0) = (Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n));
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
    if det.abs() < 1e-9 {
        return None;
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
            break;
        }
    }
    Some(d)
}

/// ピラミッド Lucas-Kanade。prev の点 p が next でどこにあるか。
pub fn track(prev: &[Img], next: &[Img], p: (f32, f32)) -> Option<(f32, f32)> {
    let mut g = (0.0f32, 0.0f32);
    for l in (0..prev.len()).rev() {
        let s = (1 << l) as f32;
        let d = lk_level(&prev[l], &next[l], (p.0 / s, p.1 / s), g)?;
        g = if l > 0 { (d.0 * 2.0, d.1 * 2.0) } else { d };
    }
    let q = (p.0 + g.0, p.1 + g.1);
    let (w, h) = (prev[0].w as f32, prev[0].h as f32);
    if q.0 < 0.0 || q.1 < 0.0 || q.0 >= w || q.1 >= h || !q.0.is_finite() {
        return None;
    }
    Some(q)
}

/// 窓どうしの正規化相互相関(-1〜1)。
pub fn ncc(a: &Img, pa: (f32, f32), b: &Img, pb: (f32, f32)) -> f32 {
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

pub fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

// ------------------------------------------------------------ ホモグラフィ

/// 3x3 行列(行優先、h[8] = 1 に正規化)。
pub type Hom = [f64; 9];

pub fn apply(h: &Hom, p: (f32, f32)) -> (f32, f32) {
    let (x, y) = (p.0 as f64, p.1 as f64);
    let w = h[6] * x + h[7] * y + h[8];
    (((h[0] * x + h[1] * y + h[2]) / w) as f32, ((h[3] * x + h[4] * y + h[5]) / w) as f32)
}

pub fn invert(h: &Hom) -> Hom {
    let [a, b, c, d, e, f, g, hh, i] = *h;
    let co = [
        e * i - f * hh, -(b * i - c * hh), b * f - c * e,
        -(d * i - f * g), a * i - c * g, -(a * f - c * d),
        d * hh - e * g, -(a * hh - b * g), a * e - b * d,
    ];
    let det = a * co[0] + b * co[3] + c * co[6];
    let mut r = [0.0; 9];
    for k in 0..9 {
        r[k] = co[k] / det;
    }
    let s = r[8];
    r.map(|v| v / s)
}

/// 対応点(4 点以上)から最小二乗でホモグラフィを求める(h33 = 1、座標は正規化して計算)。
pub fn fit_homography(src: &[(f32, f32)], dst: &[(f32, f32)]) -> Option<Hom> {
    if src.len() < 4 {
        return None;
    }
    let norm = |pts: &[(f32, f32)]| {
        let n = pts.len() as f64;
        let (mx, my) = pts.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 as f64 / n, a.1 + p.1 as f64 / n));
        let md = pts.iter().map(|p| ((p.0 as f64 - mx).powi(2) + (p.1 as f64 - my).powi(2)).sqrt()).sum::<f64>() / n;
        let s = std::f64::consts::SQRT_2 / md.max(1e-9);
        (mx, my, s)
    };
    let (sx, sy, ss) = norm(src);
    let (dx, dy, ds) = norm(dst);
    // 正規方程式 A^T A h = A^T b(8 未知数)
    let mut ata = [[0.0f64; 8]; 8];
    let mut atb = [0.0f64; 8];
    for (p, q) in src.iter().zip(dst) {
        let (x, y) = ((p.0 as f64 - sx) * ss, (p.1 as f64 - sy) * ss);
        let (u, v) = ((q.0 as f64 - dx) * ds, (q.1 as f64 - dy) * ds);
        let rows = [
            ([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u),
            ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v),
        ];
        for (r, b) in rows {
            for i in 0..8 {
                atb[i] += r[i] * b;
                for j in 0..8 {
                    ata[i][j] += r[i] * r[j];
                }
            }
        }
    }
    let h = solve8(ata, atb)?;
    let hn = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
    // 正規化を戻す: H = Td^-1 * Hn * Ts
    let ts = [ss, 0.0, -ss * sx, 0.0, ss, -ss * sy, 0.0, 0.0, 1.0];
    let td_inv = [1.0 / ds, 0.0, dx, 0.0, 1.0 / ds, dy, 0.0, 0.0, 1.0];
    let m = matmul(&td_inv, &matmul(&hn, &ts));
    let s = m[8];
    if s.abs() < 1e-12 {
        return None;
    }
    Some(m.map(|v| v / s))
}

fn matmul(a: &Hom, b: &Hom) -> Hom {
    let mut r = [0.0; 9];
    for i in 0..3 {
        for j in 0..3 {
            r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
        }
    }
    r
}

fn solve8(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for c in 0..8 {
        let p = (c..8).max_by(|&i, &j| a[i][c].abs().partial_cmp(&a[j][c].abs()).unwrap())?;
        if a[p][c].abs() < 1e-12 {
            return None;
        }
        a.swap(c, p);
        b.swap(c, p);
        for r in 0..8 {
            if r != c {
                let f = a[r][c] / a[c][c];
                for k in c..8 {
                    a[r][k] -= f * a[c][k];
                }
                b[r] -= f * b[c];
            }
        }
    }
    let mut x = [0.0; 8];
    for i in 0..8 {
        x[i] = b[i] / a[i][i];
    }
    Some(x)
}

/// RANSAC でホモグラフィを求め、外れ値を除いて最小二乗で仕上げる。戻り値: (H, 正常値の添字)
pub fn ransac_homography(src: &[(f32, f32)], dst: &[(f32, f32)], thresh: f32, rng: &mut Rng) -> Option<(Hom, Vec<usize>)> {
    let n = src.len();
    if n < 4 {
        return None;
    }
    let mut best: Vec<usize> = Vec::new();
    for _ in 0..300 {
        let mut idx = [0usize; 4];
        for k in 0..4 {
            loop {
                let c = rng.below(n);
                if !idx[..k].contains(&c) {
                    idx[k] = c;
                    break;
                }
            }
        }
        let s: Vec<_> = idx.iter().map(|&i| src[i]).collect();
        let d: Vec<_> = idx.iter().map(|&i| dst[i]).collect();
        let Some(h) = fit_homography(&s, &d) else { continue };
        let inl: Vec<usize> = (0..n).filter(|&i| dist(apply(&h, src[i]), dst[i]) < thresh).collect();
        if inl.len() > best.len() {
            best = inl;
            if best.len() * 10 >= n * 9 {
                break;
            }
        }
    }
    if best.len() < 6 {
        return None;
    }
    // 仕上げ: 正常値で最小二乗 → もう一度正常値を選び直して最小二乗
    let mut h = fit_homography(
        &best.iter().map(|&i| src[i]).collect::<Vec<_>>(),
        &best.iter().map(|&i| dst[i]).collect::<Vec<_>>(),
    )?;
    let inl: Vec<usize> = (0..n).filter(|&i| dist(apply(&h, src[i]), dst[i]) < thresh).collect();
    if inl.len() >= 6 {
        h = fit_homography(
            &inl.iter().map(|&i| src[i]).collect::<Vec<_>>(),
            &inl.iter().map(|&i| dst[i]).collect::<Vec<_>>(),
        )?;
        return Some((h, inl));
    }
    Some((h, best))
}

/// 点が四角形(時計回り/反時計回りどちらでも)の内側か。
pub fn inside_quad(q: &[(f32, f32); 4], p: (f32, f32)) -> bool {
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b.0 - a.0) * (p.1 - a.1) - (b.1 - a.1) * (p.0 - a.0);
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign && c != 0.0 {
            return false;
        }
    }
    true
}
