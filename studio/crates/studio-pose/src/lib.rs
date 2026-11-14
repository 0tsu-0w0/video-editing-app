//! studio-pose: 動画の中の人物を追い、体の部位(頭・手・腰など)の動きを取り出す。
//!
//! - 人物の検出: RTMDet-nano(OpenMMLab)
//! - 関節の推定: RTMPose-m(17 関節、COCO の並び)
//! どちらも ONNX Runtime の CPU で動かす(このクラスの GPU では CPU の方が速かった)。外部へは何も送らない。
//!
//! モデルのファイルはアプリに含めない(大きさと、学習済みの重みの利用条件の確認が必要なため)。
//! `models_dir()` の場所に `rtmdet/end2end.onnx` と `rtmpose/end2end.onnx` を置く。
//!
//! 人物の追い方: 毎フレーム人物を検出し、前のフレームの枠と最も重なる人物を同じ人とみなす。
//! 見失っても数フレームは前の枠で推定を続け、それ以上続いたら止める。

use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use studio_core::{AssetKind, Id, Project, Time};
use studio_media::VideoReader;
use studio_track::TrackPoint;

/// 17 関節(COCO の並び)の名前。
pub const KEYPOINTS: [&str; 17] = ["鼻", "左目", "右目", "左耳", "右耳", "左肩", "右肩", "左肘", "右肘", "左手首", "右手首", "左腰", "右腰", "左膝", "右膝", "左足首", "右足首"];

/// 追従に使える部位: (名前, 表示名, 使う関節)。
pub const PARTS: [(&str, &str, &[usize]); 8] = [
    ("head", "頭", &[0, 1, 2, 3, 4]),
    ("neck", "首元", &[5, 6]),
    ("body", "胴体の中心", &[5, 6, 11, 12]),
    ("hips", "腰", &[11, 12]),
    ("left_hand", "左手", &[9]),
    ("right_hand", "右手", &[10]),
    ("left_foot", "左足", &[15]),
    ("right_foot", "右足", &[16]),
];

/// 骨格の線(描画用)。
pub const SKELETON: [(usize, usize); 16] = [(5, 6), (5, 7), (7, 9), (6, 8), (8, 10), (5, 11), (6, 12), (11, 12), (11, 13), (13, 15), (12, 14), (14, 16), (0, 1), (0, 2), (1, 3), (2, 4)];

/// 関節の位置と信頼度(素材の画素)。
pub type Keypoints = [(f32, f32, f32); 17];

/// モデルを置く場所。環境変数 STUDIO_POSE_MODELS、無ければ %LOCALAPPDATA%\ForgeStudio\models。
pub fn models_dir() -> PathBuf {
    if let Ok(d) = std::env::var("STUDIO_POSE_MODELS") {
        return PathBuf::from(d);
    }
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into())).join("ForgeStudio").join("models")
}

/// モデルが揃っているか。
pub fn models_ready(dir: &Path) -> bool {
    dir.join("rtmdet").join("end2end.onnx").is_file() && dir.join("rtmpose").join("end2end.onnx").is_file()
}

fn env() -> Result<&'static ort::environment::Environment, String> {
    static ENV: std::sync::OnceLock<Result<ort::environment::Environment, String>> = std::sync::OnceLock::new();
    // 利用状況の送信(テレメトリ)は止める
    ENV.get_or_init(|| ort::init().with_name("studio").with_telemetry(false).build().map_err(|e| e.to_string())).as_ref().map_err(|e| format!("ONNX Runtime を初期化できません: {e}"))
}

pub struct PoseModels {
    det: Session,
    pose: Session,
}

/// BGRA の画像。
struct Frame<'a> {
    px: &'a [u8],
    w: u32,
    h: u32,
}

impl Frame<'_> {
    /// (x, y) の RGB(0〜255)。範囲外は fill。
    fn rgb(&self, x: f32, y: f32, fill: f32) -> [f32; 3] {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let at = |xx: i64, yy: i64| {
            if xx < 0 || yy < 0 || xx >= self.w as i64 || yy >= self.h as i64 {
                [fill; 3]
            } else {
                let i = ((yy as u32 * self.w + xx as u32) * 4) as usize;
                [self.px[i + 2] as f32, self.px[i + 1] as f32, self.px[i] as f32]
            }
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1, y0), at(x0, y0 + 1), at(x0 + 1, y0 + 1));
        [0, 1, 2].map(|i| (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy)
    }
}

impl PoseModels {
    pub fn load(dir: &Path) -> Result<PoseModels, String> {
        if !models_ready(dir) {
            return Err(format!(
                "人物の推定モデルがありません。{} に rtmdet\\end2end.onnx と rtmpose\\end2end.onnx を置いてください",
                dir.display()
            ));
        }
        let open = |p: PathBuf| -> Result<Session, String> {
            Session::builder(env()?).map_err(|e| e.to_string())?.commit_from_file(&p).map_err(|e| format!("{}: {e}", p.display()))
        };
        Ok(PoseModels { det: open(dir.join("rtmdet").join("end2end.onnx"))?, pose: open(dir.join("rtmpose").join("end2end.onnx"))? })
    }

    /// 人物の枠 (x1, y1, x2, y2, スコア) をすべて返す。
    fn detect(&mut self, f: &Frame) -> Result<Vec<[f32; 5]>, String> {
        let s = (320.0 / f.w as f32).min(320.0 / f.h as f32);
        let mean = [103.53, 116.28, 123.675];
        let std = [57.375, 57.12, 58.395];
        let mut data = vec![0.0f32; 3 * 320 * 320];
        for y in 0..320 {
            for x in 0..320 {
                let (sx, sy) = ((x as f32 + 0.5) / s - 0.5, (y as f32 + 0.5) / s - 0.5);
                let rgb = if sx < f.w as f32 && sy < f.h as f32 { f.rgb(sx, sy, 114.0) } else { [114.0; 3] };
                let bgr = [rgb[2], rgb[1], rgb[0]];
                for c in 0..3 {
                    data[c * 320 * 320 + y * 320 + x] = (bgr[c] - mean[c]) / std[c];
                }
            }
        }
        let input = Tensor::from_array(([1usize, 3, 320, 320], data)).map_err(|e| e.to_string())?;
        let out = self.det.run(ort::inputs!["input" => input]).map_err(|e| e.to_string())?;
        let (shape, dets) = out["dets"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let n = shape[1] as usize;
        Ok((0..n).map(|i| &dets[i * 5..i * 5 + 5]).filter(|d| d[4] > 0.3).map(|d| [d[0] / s, d[1] / s, d[2] / s, d[3] / s, d[4]]).collect())
    }

    /// 枠の中の人物の関節を推定する。
    fn pose(&mut self, f: &Frame, b: [f32; 4]) -> Result<Keypoints, String> {
        // 1.25 倍に広げ、縦横比 3:4 に合わせて切り出す
        let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        let (mut w, mut h) = ((b[2] - b[0]) * 1.25, (b[3] - b[1]) * 1.25);
        if w > h * 0.75 {
            h = w / 0.75;
        } else {
            w = h * 0.75;
        }
        let mean = [123.675, 116.28, 103.53];
        let std = [58.395, 57.12, 57.375];
        let mut data = vec![0.0f32; 3 * 256 * 192];
        for v in 0..256 {
            for u in 0..192 {
                let sx = cx - w / 2.0 + (u as f32 + 0.5) * w / 192.0 - 0.5;
                let sy = cy - h / 2.0 + (v as f32 + 0.5) * h / 256.0 - 0.5;
                let rgb = f.rgb(sx, sy, 0.0);
                for c in 0..3 {
                    data[c * 256 * 192 + v * 192 + u] = (rgb[c] - mean[c]) / std[c];
                }
            }
        }
        let input = Tensor::from_array(([1usize, 3, 256, 192], data)).map_err(|e| e.to_string())?;
        let out = self.pose.run(ort::inputs!["input" => input]).map_err(|e| e.to_string())?;
        let (_, sx) = out["simcc_x"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let (_, sy) = out["simcc_y"].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        // 最大値の位置を、両隣との放物線で小数まで求める
        let peak = |v: &[f32]| {
            let (i, m) = v.iter().enumerate().fold((0, f32::MIN), |a, (i, x)| if *x > a.1 { (i, *x) } else { a });
            let off = if i > 0 && i + 1 < v.len() {
                let (l, r) = (v[i - 1], v[i + 1]);
                let d = l - 2.0 * m + r;
                if d < 0.0 { (0.5 * (l - r) / d).clamp(-0.5, 0.5) } else { 0.0 }
            } else {
                0.0
            };
            (i as f32 + off, m)
        };
        let mut k = [(0.0, 0.0, 0.0); 17];
        for (j, o) in k.iter_mut().enumerate() {
            let (ix, vx) = peak(&sx[j * 384..(j + 1) * 384]);
            let (iy, vy) = peak(&sy[j * 512..(j + 1) * 512]);
            *o = (cx - w / 2.0 + ix / 2.0 * w / 192.0, cy - h / 2.0 + iy / 2.0 * h / 256.0, vx.min(vy));
        }
        Ok(k)
    }
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (x1, y1, x2, y2) = (a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3]));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let area = |r: [f32; 4]| (r[2] - r[0]).max(0.0) * (r[3] - r[1]).max(0.0);
    inter / (area(a) + area(b) - inter).max(1e-6)
}

/// 1 フレーム分の結果(素材の画素)。見失ったら None。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PersonKey {
    pub time: Time,
    pub bbox: Option<[f32; 4]>,
    pub keypoints: Option<Keypoints>,
}

/// 動画クリップ `source_clip` の上で、時刻 `from` に素材の画素 `pick` にいる人物を、`to` まで追う。
pub fn track_person(
    models: &mut PoseModels,
    project: &Project,
    source_clip: Id,
    from: Time,
    to: Time,
    pick: (f32, f32),
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Vec<PersonKey>, String> {
    const MAX_MISSES: usize = 5;
    let clip = project.clip(source_clip).ok_or("追跡する動画のクリップがありません")?;
    let asset = project.assets.get(&clip.asset).ok_or("素材がありません")?;
    if !matches!(asset.kind, AssetKind::Video { .. }) {
        return Err("人物を追えるのは動画のクリップだけです".into());
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
    let mut out = Vec::with_capacity(total);
    let mut prev: Option<[f32; 4]> = None;
    let mut misses = 0;
    for (i, n) in (n0..=n1).enumerate() {
        let t = fr.frame_to_time(n).max(from);
        let f = reader.frame_at(clip.source_at(t - clip.start)).map_err(|e| e.to_string())?;
        let frame = Frame { px: &f.bgra, w: f.width, h: f.height };
        let dets = models.detect(&frame)?;
        let chosen = match prev {
            None => {
                // クリックした点を含む人物(重なるなら小さい方 = 手前の人)、無ければ最も近い人物
                let inside = dets.iter().filter(|d| pick.0 >= d[0] && pick.0 <= d[2] && pick.1 >= d[1] && pick.1 <= d[3]).min_by(|a, b| ((a[2] - a[0]) * (a[3] - a[1])).total_cmp(&((b[2] - b[0]) * (b[3] - b[1]))));
                let near = || {
                    dets.iter()
                        .map(|d| {
                            let (cx, cy) = ((d[0] + d[2]) / 2.0, (d[1] + d[3]) / 2.0);
                            (d, ((cx - pick.0).powi(2) + (cy - pick.1).powi(2)).sqrt() / (d[3] - d[1]).max(1.0))
                        })
                        .filter(|(_, r)| *r < 1.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(d, _)| d)
                };
                let d = inside.or_else(near).ok_or("指定した位置に人物が見つかりません。人物の体の上をクリックしてください")?;
                Some([d[0], d[1], d[2], d[3]])
            }
            Some(p) => {
                let best = dets.iter().map(|d| ([d[0], d[1], d[2], d[3]], iou(p, [d[0], d[1], d[2], d[3]]))).max_by(|a, b| a.1.total_cmp(&b.1));
                match best {
                    Some((b, o)) if o > 0.25 => {
                        misses = 0;
                        Some(b)
                    }
                    // 見つからない・別人しかいない: 数フレームは前の枠で続ける(一瞬隠れたとき)
                    _ => {
                        misses += 1;
                        (misses <= MAX_MISSES).then_some(p)
                    }
                }
            }
        };
        let keypoints = match chosen {
            Some(b) => Some(models.pose(&frame, b)?),
            None => None,
        };
        out.push(PersonKey { time: t, bbox: chosen, keypoints });
        prev = chosen;
        if !progress(i + 1, total) {
            return Err("中止しました".into());
        }
        if chosen.is_none() {
            break;
        }
    }
    // 見失って止めた場合、前の枠で続けた分は当てにならないので見失いとして扱う
    if out.last().is_some_and(|k| k.bbox.is_none()) {
        let n = out.len();
        for k in out[n.saturating_sub(MAX_MISSES + 1)..].iter_mut() {
            k.bbox = None;
            k.keypoints = None;
        }
    }
    Ok(out)
}

/// 部位の位置(関係する関節のうち、信頼度が十分なものの平均)。
pub fn part_point(k: &Keypoints, part: &str) -> Option<(f32, f32)> {
    let joints = PARTS.iter().find(|p| p.0 == part)?.2;
    let ok: Vec<&(f32, f32, f32)> = joints.iter().map(|&j| &k[j]).filter(|p| p.2 > 0.3).collect();
    // 2 つ以上の関節で決める部位は、半分以上見えていること
    if ok.is_empty() || ok.len() * 2 < joints.len() {
        return None;
    }
    let n = ok.len() as f32;
    Some((ok.iter().map(|p| p.0).sum::<f32>() / n, ok.iter().map(|p| p.1).sum::<f32>() / n))
}

/// 人物の追跡結果を、部位の位置の点の列にする(細かい揺れを前後 2 フレームの平均でならす)。
/// 結果は `studio_track::follow_commands` で、ほかのクリップの追従に使える。
pub fn part_track(keys: &[PersonKey], part: &str) -> Result<Vec<TrackPoint>, String> {
    if !PARTS.iter().any(|p| p.0 == part) {
        return Err(format!("部位「{part}」は使えません。{} のどれかを指定してください", PARTS.iter().map(|p| p.0).collect::<Vec<_>>().join(" / ")));
    }
    let raw: Vec<Option<(f32, f32)>> = keys.iter().map(|k| k.keypoints.as_ref().and_then(|kp| part_point(kp, part))).collect();
    let mut out = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        let pos = raw[i].map(|_| {
            let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
            for j in i.saturating_sub(2)..(i + 3).min(raw.len()) {
                if let Some(p) = raw[j] {
                    let w = if j == i { 2.0 } else { 1.0 };
                    sx += p.0 * w;
                    sy += p.1 * w;
                    n += w;
                }
            }
            (sx / n, sy / n)
        });
        out.push(TrackPoint { time: k.time, pos });
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
