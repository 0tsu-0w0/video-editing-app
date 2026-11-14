//! studio-track: 動画クリップ上の点を追跡し、別のクリップをその動きに追従させる。
//!
//! 1. `track_point`: 動画クリップの素材を読み、指定した点をフレームごとに追う(ピラミッド Lucas-Kanade)。
//!    往復チェックと見た目の比較で見失いを検出し、見失った時点で止める。
//! 2. `follow_commands`: 追跡結果を、追従させたいクリップの x / y のキーフレーム(コマンド)にする。
//!    結果はふつうのキーフレームなので、人が後から直せる。

pub mod vision;

use std::path::Path;
use studio_core::eval::layers_at;
use studio_core::project::{Interp, Project};
use studio_core::{AssetKind, Command, Id, Time, TrackKind};
use studio_media::VideoReader;
use studio_core::project::PinKey;
use vision::{apply, corner_score, dist, inside_quad, invert, ncc, pyramid, ransac_homography, track, Img, Rng};

/// 解析するときの画像の最大幅(速さのため縮小する。結果は元の画素に戻す)
const ANALYSIS_WIDTH: u32 = 960;

/// 1 フレーム分の追跡結果(タイムライン上の時刻と、素材の画素での位置。見失ったら None)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackPoint {
    pub time: Time,
    pub pos: Option<(f32, f32)>,
}

fn gray(bgra: &[u8], w: u32, h: u32, scale: f32) -> Img {
    let (gw, gh) = (((w as f32 * scale).round() as usize).max(16), ((h as f32 * scale).round() as usize).max(16));
    let mut img = Img::new(gw, gh);
    for y in 0..gh {
        for x in 0..gw {
            // 縮小時は、対応する範囲の平均を取る(ちらつきを抑える)
            let (sx0, sy0) = ((x as f32 / scale) as u32, (y as f32 / scale) as u32);
            let (sx1, sy1) = ((((x + 1) as f32 / scale) as u32).max(sx0 + 1).min(w), (((y + 1) as f32 / scale) as u32).max(sy0 + 1).min(h));
            let mut s = 0.0;
            let mut n = 0.0;
            for yy in sy0..sy1 {
                for xx in sx0..sx1 {
                    let i = ((yy * w + xx) * 4) as usize;
                    s += 0.0722 * bgra[i] as f32 + 0.7152 * bgra[i + 1] as f32 + 0.2126 * bgra[i + 2] as f32;
                    n += 1.0;
                }
            }
            img.p[y * gw + x] = s / n / 255.0;
        }
    }
    img
}

fn analysis_scale(w: u32) -> f32 {
    (ANALYSIS_WIDTH as f32 / w as f32).min(1.0)
}

/// 素材の画素 p の近く(半径 radius 画素)で、最も追跡しやすい点(角)を探す。
/// 利用者がクリックした点が平らな場所でも、近くの模様のはっきりした点に吸着させる。
pub fn best_point_near(bgra: &[u8], w: u32, h: u32, p: (f32, f32), radius: f32) -> (f32, f32) {
    let s = analysis_scale(w);
    let g = gray(bgra, w, h, s);
    let c = (p.0 * s, p.1 * s);
    let r = (radius * s).max(4.0);
    let mut best = (c, corner_score(&g, c) * 1.2); // 指定した点そのものを少し優先する
    let step = 2.0;
    let mut dy = -r;
    while dy <= r {
        let mut dx = -r;
        while dx <= r {
            let q = (c.0 + dx, c.1 + dy);
            if q.0 > 8.0 && q.1 > 8.0 && q.0 < g.w as f32 - 8.0 && q.1 < g.h as f32 - 8.0 {
                let sc = corner_score(&g, q);
                if sc > best.1 {
                    best = (q, sc);
                }
            }
            dx += step;
        }
        dy += step;
    }
    (best.0 .0 / s, best.0 .1 / s)
}

/// 動画クリップ `source_clip` の上で、タイムライン時刻 `from` における素材の画素 `point` を、`to` まで追う。
/// `progress(済み, 全体)` が false を返したら中止する。
pub fn track_point(
    project: &Project,
    source_clip: Id,
    from: Time,
    to: Time,
    point: (f32, f32),
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Vec<TrackPoint>, String> {
    let clip = project.clip(source_clip).ok_or("追跡する動画のクリップがありません")?;
    let asset = project.assets.get(&clip.asset).ok_or("素材がありません")?;
    if !matches!(asset.kind, AssetKind::Video { .. }) {
        return Err("追跡できるのは動画のクリップだけです".into());
    }
    let from = from.max(clip.start);
    let to = to.min(clip.end());
    if to <= from {
        return Err("追跡する範囲が動画のクリップと重なっていません".into());
    }
    let fr = project.settings.frame_rate;
    let (n0, n1) = (fr.time_to_frame(from), fr.time_to_frame(to - Time(1)));
    let total = (n1 - n0 + 1) as usize;
    let mut reader = VideoReader::open(Path::new(&asset.path)).map_err(|e| e.to_string())?;
    let (w, _) = reader.size();
    let s = analysis_scale(w);

    let mut out = Vec::with_capacity(total);
    let mut prev: Option<(Img, Vec<Img>)> = None;
    let mut pos = Some((point.0 * s, point.1 * s));
    for (i, n) in (n0..=n1).enumerate() {
        let t = fr.frame_to_time(n).max(from);
        let f = reader.frame_at(clip.source_at(t - clip.start)).map_err(|e| e.to_string())?;
        let g = gray(&f.bgra, f.width, f.height, s);
        let pyr = pyramid(&g);
        if let (Some(p), Some((pg, ppyr))) = (pos, &prev) {
            pos = track(ppyr, &pyr, p).filter(|q| {
                // 往復で同じ所へ戻るか、見た目が大きく変わっていないか
                track(&pyr, ppyr, *q).is_some_and(|back| dist(back, p) < 0.8) && ncc(pg, p, &g, *q) > 0.75
            });
        }
        out.push(TrackPoint { time: t, pos: pos.map(|p| (p.0 / s, p.1 / s)) });
        prev = Some((g, pyr));
        if !progress(i + 1, total) {
            return Err("中止しました".into());
        }
        if pos.is_none() {
            break; // 見失ったら止める(後の結果は信頼できない)
        }
    }
    Ok(out)
}

/// 平面の追跡結果(素材の画素での四隅。左上・右上・右下・左下。見失ったら None)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneKey {
    pub time: Time,
    pub corners: Option<[(f32, f32); 4]>,
}

/// 四角形の内側(少し縮めた範囲)から、追跡しやすい点を最大 want 個選ぶ。
fn detect(img: &Img, quad: &[(f32, f32); 4], existing: &[(f32, f32)], want: usize) -> Vec<(f32, f32)> {
    let (minx, maxx) = quad.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    let (miny, maxy) = quad.iter().fold((f32::MAX, f32::MIN), |a, p| (a.0.min(p.1), a.1.max(p.1)));
    let c = quad.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / 4.0, a.1 + p.1 / 4.0));
    let shrink = quad.map(|p| (c.0 + (p.0 - c.0) * 0.92, c.1 + (p.1 - c.1) * 0.92));
    // 四角形の大きさに合わせて間隔を決める(小さい平面でも点を取れるように)
    let step = (((maxx - minx) * (maxy - miny)).max(1.0).sqrt() / 20.0).clamp(4.0, 12.0);
    let mut cand = Vec::new();
    let mut y = miny.max(10.0);
    while y < maxy.min(img.h as f32 - 10.0) {
        let mut x = minx.max(10.0);
        while x < maxx.min(img.w as f32 - 10.0) {
            if inside_quad(&shrink, (x, y)) && existing.iter().all(|e| dist(*e, (x, y)) > step * 1.2) {
                let s = corner_score(img, (x, y));
                if s > 1e-3 {
                    cand.push(((x, y), s));
                }
            }
            x += step;
        }
        y += step;
    }
    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    cand.into_iter().take(want).map(|c| c.0).collect()
}

/// 動画クリップ `source_clip` の上で、タイムライン時刻 `from` に素材の画素で指定した四角形(壁・机などの平面)を、`to` まで追う。
/// 平面の中の多数の点を追い、その動きに最もよく合う射影変換(RANSAC)で四隅を動かす。
/// 手前を物が横切っても、残りの点で推定を続ける。点が足りなくなったら止める。
pub fn track_plane(
    project: &Project,
    source_clip: Id,
    from: Time,
    to: Time,
    quad: [(f32, f32); 4],
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Vec<PlaneKey>, String> {
    const WANT: usize = 200;
    const MIN_POINTS: usize = 8;
    let clip = project.clip(source_clip).ok_or("追跡する動画のクリップがありません")?;
    let asset = project.assets.get(&clip.asset).ok_or("素材がありません")?;
    if !matches!(asset.kind, AssetKind::Video { .. }) {
        return Err("追跡できるのは動画のクリップだけです".into());
    }
    let from = from.max(clip.start);
    let to = to.min(clip.end());
    if to <= from {
        return Err("追跡する範囲が動画のクリップと重なっていません".into());
    }
    let fr = project.settings.frame_rate;
    let (n0, n1) = (fr.time_to_frame(from), fr.time_to_frame(to - Time(1)));
    let total = (n1 - n0 + 1) as usize;
    let mut reader = VideoReader::open(Path::new(&asset.path)).map_err(|e| e.to_string())?;
    let (w, _) = reader.size();
    let s = analysis_scale(w);
    let quad0 = quad.map(|p| (p.0 * s, p.1 * s));

    struct Feature {
        reference: (f32, f32),
        pos: (f32, f32),
    }
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut out = Vec::with_capacity(total);
    let mut prev: Option<(Img, Vec<Img>)> = None;
    let mut feats: Vec<Feature> = vec![];
    let mut h_cur: vision::Hom = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    for (i, n) in (n0..=n1).enumerate() {
        let t = fr.frame_to_time(n).max(from);
        let f = reader.frame_at(clip.source_at(t - clip.start)).map_err(|e| e.to_string())?;
        let g = gray(&f.bgra, f.width, f.height, s);
        let pyr = pyramid(&g);
        let mut lost = false;
        match &prev {
            None => {
                feats = detect(&g, &quad0, &[], WANT).into_iter().map(|p| Feature { reference: p, pos: p }).collect();
                if feats.len() < MIN_POINTS {
                    return Err(format!("指定した四角形の中に、追跡に使える模様が少なすぎます({} 点)。模様のある平面を囲んでください", feats.len()));
                }
            }
            Some((pg, ppyr)) => {
                feats.retain_mut(|ft| {
                    let Some(q) = track(ppyr, &pyr, ft.pos) else { return false };
                    let ok = track(&pyr, ppyr, q).is_some_and(|b| dist(b, ft.pos) < 0.8) && ncc(pg, ft.pos, &g, q) > 0.75;
                    ft.pos = q;
                    ok
                });
                let src: Vec<_> = feats.iter().map(|f| f.reference).collect();
                let dst: Vec<_> = feats.iter().map(|f| f.pos).collect();
                match (feats.len() >= MIN_POINTS).then(|| ransac_homography(&src, &dst, 1.5, &mut rng)).flatten() {
                    Some((h, inliers)) if inliers.len() >= MIN_POINTS => {
                        h_cur = h;
                        let keep: std::collections::HashSet<usize> = inliers.into_iter().collect();
                        let mut k = 0;
                        feats.retain(|_| {
                            k += 1;
                            keep.contains(&(k - 1))
                        });
                    }
                    _ => lost = true,
                }
                // 点が減ったら、今の平面の内側から補充する(最初のフレームでの位置に戻して覚える)
                if !lost && feats.len() < WANT * 6 / 10 {
                    let est = quad0.map(|c| apply(&h_cur, c));
                    let existing: Vec<_> = feats.iter().map(|f| f.pos).collect();
                    let inv = invert(&h_cur);
                    for p in detect(&g, &est, &existing, WANT - feats.len()) {
                        feats.push(Feature { reference: apply(&inv, p), pos: p });
                    }
                }
            }
        }
        let corners = (!lost).then(|| quad0.map(|c| apply(&h_cur, c)).map(|p| (p.0 / s, p.1 / s)));
        out.push(PlaneKey { time: t, corners });
        prev = Some((g, pyr));
        if !progress(i + 1, total) {
            return Err("中止しました".into());
        }
        if lost {
            break;
        }
    }
    Ok(out)
}

/// 平面の追跡結果を、`target` クリップの四隅のキーフレーム(set_pin コマンド)にする。
pub fn pin_commands(project: &Project, target: Id, source_clip: Id, track: &[PlaneKey]) -> Result<Vec<Command>, String> {
    let tc = project.clip(target).ok_or("貼り付けるクリップがありません")?;
    let (ti, _) = project.find_clip(target).unwrap();
    if project.tracks[ti].kind != TrackKind::Video {
        return Err("貼り付けられるのは映像トラックのクリップだけです".into());
    }
    if target == source_clip {
        return Err("追跡元と同じクリップには貼り付けられません".into());
    }
    let mut keys = vec![];
    for k in track.iter().filter(|k| tc.contains(k.time)) {
        let Some(c) = k.corners else { continue };
        let mut corners = [[0.0; 2]; 4];
        for (o, p) in corners.iter_mut().zip(c) {
            let q = to_screen(project, source_clip, k.time, p).ok_or("追跡元の位置を計算できません")?;
            *o = [q.0.clamp(-100_000.0, 100_000.0), q.1.clamp(-100_000.0, 100_000.0)];
        }
        keys.push(PinKey { time: k.time - tc.start, corners });
    }
    if keys.is_empty() {
        return Err("貼り付けるクリップの表示中に、追跡できたフレームがありません".into());
    }
    Ok(vec![Command::SetPin { clip: target, keys }])
}

/// 素材の画素 p が、時刻 t の画面のどこに表示されるか(画面中央が原点、y は下向き)。
fn to_screen(project: &Project, source_clip: Id, t: Time, p: (f32, f32)) -> Option<(f64, f64)> {
    let clip = project.clip(source_clip)?;
    let (w, h) = match project.assets.get(&clip.asset)?.kind {
        AssetKind::Video { width, height, .. } => (width as f64, height as f64),
        _ => return None,
    };
    // レイヤーの置き方は、不透明度 0 でも分かるよう値から直接求める
    let local = t - clip.start;
    let pr = &clip.params;
    let (x, y, scale, rot) = (pr.x.eval(local), pr.y.eval(local), pr.scale.eval(local), pr.rotation.eval(local).to_radians());
    let (dx, dy) = ((p.0 as f64 - w / 2.0) * scale, (p.1 as f64 - h / 2.0) * scale);
    let (s, c) = rot.sin_cos();
    Some((x + dx * c - dy * s, y + dx * s + dy * c))
}

/// 追跡結果を、`target` クリップを追従させる x / y のキーフレームにする。
/// 追跡を始めた時点の target の位置を基準に、点の画面上の移動量だけ動かす。
/// target の x / y にあった既存のキーフレームは置き換える。
pub fn follow_commands(project: &Project, target: Id, source_clip: Id, track: &[TrackPoint]) -> Result<Vec<Command>, String> {
    let tc = project.clip(target).ok_or("追従させるクリップがありません")?;
    let (ti, _) = project.find_clip(target).unwrap();
    if project.tracks[ti].kind != TrackKind::Video {
        return Err("追従させられるのは映像トラックのクリップだけです".into());
    }
    if target == source_clip {
        return Err("追跡元と同じクリップは追従させられません".into());
    }
    let pts: Vec<(Time, (f32, f32))> = track.iter().filter_map(|k| k.pos.map(|p| (k.time, p))).filter(|(t, _)| tc.contains(*t)).collect();
    let Some(&(t0, p0)) = pts.first() else {
        return Err("追従させるクリップの表示中に、追跡できたフレームがありません".into());
    };
    let s0 = to_screen(project, source_clip, t0, p0).ok_or("追跡元の位置を計算できません")?;
    let base = (tc.params.x.eval(t0 - tc.start), tc.params.y.eval(t0 - tc.start));
    let mut cmds = vec![
        Command::SetParam { clip: target, param: "x".into(), value: base.0 },
        Command::SetParam { clip: target, param: "y".into(), value: base.1 },
    ];
    for (t, p) in pts {
        let s = to_screen(project, source_clip, t, p).ok_or("追跡元の位置を計算できません")?;
        let (x, y) = (base.0 + s.0 - s0.0, base.1 + s.1 - s0.1);
        let time = t - tc.start;
        cmds.push(Command::SetKey { clip: target, param: "x".into(), time, value: x.clamp(-100_000.0, 100_000.0), interp: Interp::Linear });
        cmds.push(Command::SetKey { clip: target, param: "y".into(), time, value: y.clamp(-100_000.0, 100_000.0), interp: Interp::Linear });
    }
    Ok(cmds)
}

/// 時刻 t に、target より下(奥)のトラックで表示されている動画クリップ(追跡元の候補)。
pub fn source_below(project: &Project, target: Id, t: Time) -> Option<Id> {
    let (ti, _) = project.find_clip(target)?;
    let below: Vec<Id> = layers_at(project, t)
        .into_iter()
        .filter(|l| project.find_clip(l.clip).is_some_and(|(i, _)| i < ti))
        .filter(|l| project.assets.get(&l.asset).is_some_and(|a| matches!(a.kind, AssetKind::Video { .. })))
        .map(|l| l.clip)
        .collect();
    below.last().copied() // 最も手前の動画
}

/// 画面上の位置(中央原点)が、時刻 t の動画クリップの素材のどの画素か(to_screen の逆)。
pub fn screen_to_source(project: &Project, source_clip: Id, t: Time, s: (f64, f64)) -> Option<(f32, f32)> {
    let clip = project.clip(source_clip)?;
    let (w, h) = match project.assets.get(&clip.asset)?.kind {
        AssetKind::Video { width, height, .. } => (width as f64, height as f64),
        _ => return None,
    };
    let local = t - clip.start;
    let pr = &clip.params;
    let (x, y, scale, rot) = (pr.x.eval(local), pr.y.eval(local), pr.scale.eval(local), pr.rotation.eval(local).to_radians());
    if scale.abs() < 1e-9 {
        return None;
    }
    let (dx, dy) = (s.0 - x, s.1 - y);
    let (sn, c) = rot.sin_cos();
    let (rx, ry) = (dx * c + dy * sn, -dx * sn + dy * c);
    let p = (rx / scale + w / 2.0, ry / scale + h / 2.0);
    (p.0 >= 0.0 && p.1 >= 0.0 && p.0 < w && p.1 < h).then_some((p.0 as f32, p.1 as f32))
}

#[cfg(test)]
mod tests;
