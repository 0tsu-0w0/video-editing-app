//! 検証8: 人物の検出と姿勢推定を、ONNX Runtime(CPU / DirectML)で端末内で動かせるか。
//!
//! モデル: OpenMMLab の RTMDet-nano(人物検出、入力 320x320)と RTMPose-m(17 関節、入力 192x256)。
//! 使い方: pose_onnx <モデルのフォルダ> <画像> <出力 PNG>
//! - 画像から人物を検出し、いちばん確からしい人物の関節を推定して、骨格を描いた画像を保存する。
//! - CPU と DirectML(GPU)それぞれで推論時間を測る。

use image::{Rgb, RgbImage};
use ort::session::Session;
use ort::value::Tensor;
use std::time::Instant;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const SKELETON: [(usize, usize); 16] = [
    (5, 6), (5, 7), (7, 9), (6, 8), (8, 10), (5, 11), (6, 12), (11, 12),
    (11, 13), (13, 15), (12, 14), (14, 16), (0, 1), (0, 2), (1, 3), (2, 4),
];
const NAMES: [&str; 17] = [
    "鼻", "左目", "右目", "左耳", "右耳", "左肩", "右肩", "左肘", "右肘", "左手首", "右手首",
    "左腰", "右腰", "左膝", "右膝", "左足首", "右足首",
];

fn env() -> &'static ort::environment::Environment {
    static ENV: std::sync::OnceLock<ort::environment::Environment> = std::sync::OnceLock::new();
    // 利用状況の送信(テレメトリ)は止める: 外部へ何も送らない方針
    ENV.get_or_init(|| ort::init().with_name("studio").with_telemetry(false).build().expect("ONNX Runtime を初期化できません"))
}

fn session(path: &str, gpu: bool) -> Res<Session> {
    let mut b = Session::builder(env())?;
    if gpu {
        b = b.with_execution_providers([ort::ep::DirectML::default().build().error_on_failure()])?;
    }
    Ok(b.commit_from_file(path)?)
}

fn bilinear(img: &RgbImage, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (img.width() as i32, img.height() as i32);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let px = |xx: i32, yy: i32| {
        if xx < 0 || yy < 0 || xx >= w || yy >= h {
            [0.0; 3]
        } else {
            img.get_pixel(xx as u32, yy as u32).0.map(|v| v as f32)
        }
    };
    let (x0, y0) = (x0 as i32, y0 as i32);
    let (a, b, c, d) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut o = [0.0; 3];
    for i in 0..3 {
        o[i] = (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy;
    }
    o
}

/// 人物検出の入力(320x320、縦横比を保って縮小し、余白は 114、BGR で正規化)。
fn detector_input(img: &RgbImage) -> (Vec<f32>, f32) {
    let s = (320.0 / img.width() as f32).min(320.0 / img.height() as f32);
    let mean = [103.53, 116.28, 123.675];
    let std = [57.375, 57.12, 58.395];
    let mut data = vec![0.0f32; 3 * 320 * 320];
    for y in 0..320 {
        for x in 0..320 {
            let (sx, sy) = ((x as f32 + 0.5) / s - 0.5, (y as f32 + 0.5) / s - 0.5);
            let rgb = if sx < img.width() as f32 && sy < img.height() as f32 { bilinear(img, sx, sy) } else { [114.0; 3] };
            let bgr = [rgb[2], rgb[1], rgb[0]];
            for c in 0..3 {
                data[c * 320 * 320 + y * 320 + x] = (bgr[c] - mean[c]) / std[c];
            }
        }
    }
    (data, s)
}

/// 人物を検出し、最も確からしい人物の枠 (x1, y1, x2, y2, スコア) を返す。
fn detect(sess: &mut Session, img: &RgbImage) -> Res<Option<[f32; 5]>> {
    let (data, s) = detector_input(img);
    let out = sess.run(ort::inputs!["input" => Tensor::from_array(([1usize, 3, 320, 320], data))?])?;
    let (shape, dets) = out["dets"].try_extract_tensor::<f32>()?;
    let n = shape[1] as usize;
    let mut best: Option<[f32; 5]> = None;
    for i in 0..n {
        let d = &dets[i * 5..i * 5 + 5];
        if d[4] > 0.3 && best.map_or(true, |b| d[4] > b[4]) {
            best = Some([d[0] / s, d[1] / s, d[2] / s, d[3] / s, d[4]]);
        }
    }
    Ok(best)
}

/// 枠から姿勢推定の切り出し範囲(中心と幅・高さ。1.25 倍に広げ、縦横比 3:4 に合わせる)。
fn crop_box(b: [f32; 5]) -> (f32, f32, f32, f32) {
    let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    let (mut w, mut h) = ((b[2] - b[0]) * 1.25, (b[3] - b[1]) * 1.25);
    if w > h * 0.75 {
        h = w / 0.75;
    } else {
        w = h * 0.75;
    }
    (cx, cy, w, h)
}

/// 関節の推定。戻り値: 17 個の (x, y, 信頼度)。
fn pose(sess: &mut Session, img: &RgbImage, b: (f32, f32, f32, f32)) -> Res<Vec<(f32, f32, f32)>> {
    let (cx, cy, w, h) = b;
    let mean = [123.675, 116.28, 103.53];
    let std = [58.395, 57.12, 57.375];
    let mut data = vec![0.0f32; 3 * 256 * 192];
    for v in 0..256 {
        for u in 0..192 {
            let sx = cx - w / 2.0 + (u as f32 + 0.5) * w / 192.0 - 0.5;
            let sy = cy - h / 2.0 + (v as f32 + 0.5) * h / 256.0 - 0.5;
            let rgb = bilinear(img, sx, sy);
            for c in 0..3 {
                data[c * 256 * 192 + v * 192 + u] = (rgb[c] - mean[c]) / std[c];
            }
        }
    }
    let out = sess.run(ort::inputs!["input" => Tensor::from_array(([1usize, 3, 256, 192], data))?])?;
    let (_, sx) = out["simcc_x"].try_extract_tensor::<f32>()?;
    let (_, sy) = out["simcc_y"].try_extract_tensor::<f32>()?;
    let argmax = |v: &[f32]| v.iter().enumerate().fold((0, f32::MIN), |a, (i, x)| if *x > a.1 { (i, *x) } else { a });
    let mut kps = Vec::new();
    for k in 0..17 {
        let (ix, vx) = argmax(&sx[k * 384..(k + 1) * 384]);
        let (iy, vy) = argmax(&sy[k * 512..(k + 1) * 512]);
        let (x_in, y_in) = (ix as f32 / 2.0, iy as f32 / 2.0);
        kps.push((cx - w / 2.0 + x_in * w / 192.0, cy - h / 2.0 + y_in * h / 256.0, vx.min(vy)));
    }
    Ok(kps)
}

fn draw(img: &mut RgbImage, b: [f32; 5], kps: &[(f32, f32, f32)]) {
    let mut put = |x: i32, y: i32, c: Rgb<u8>| {
        if x >= 0 && y >= 0 && (x as u32) < img.width() && (y as u32) < img.height() {
            img.put_pixel(x as u32, y as u32, c);
        }
    };
    let line = |put: &mut dyn FnMut(i32, i32, Rgb<u8>), a: (f32, f32), b: (f32, f32), c: Rgb<u8>| {
        let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) as i32).max(1);
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let (x, y) = ((a.0 + (b.0 - a.0) * t) as i32, (a.1 + (b.1 - a.1) * t) as i32);
            for d in -1..=1 {
                put(x + d, y, c);
                put(x, y + d, c);
            }
        }
    };
    let yellow = Rgb([255, 220, 0]);
    let (x1, y1, x2, y2) = (b[0], b[1], b[2], b[3]);
    for (a, c) in [((x1, y1), (x2, y1)), ((x2, y1), (x2, y2)), ((x2, y2), (x1, y2)), ((x1, y2), (x1, y1))] {
        line(&mut put, a, c, yellow);
    }
    for (a, c) in SKELETON {
        if kps[a].2 > 0.3 && kps[c].2 > 0.3 {
            line(&mut put, (kps[a].0, kps[a].1), (kps[c].0, kps[c].1), Rgb([0, 255, 120]));
        }
    }
    for k in kps.iter().filter(|k| k.2 > 0.3) {
        for dy in -3..=3 {
            for dx in -3..=3 {
                if dx * dx + dy * dy <= 9 {
                    put(k.0 as i32 + dx, k.1 as i32 + dy, Rgb([255, 40, 40]));
                }
            }
        }
    }
}

fn bench<F: FnMut() -> Res<()>>(mut f: F) -> Res<f64> {
    for _ in 0..3 {
        f()?;
    }
    let t = Instant::now();
    let n = 20;
    for _ in 0..n {
        f()?;
    }
    Ok(t.elapsed().as_secs_f64() * 1000.0 / n as f64)
}

fn main() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    let (dir, img_path, out) = (&args[1], &args[2], &args[3]);
    let img = image::open(img_path)?.to_rgb8();
    println!("画像: {}x{}", img.width(), img.height());
    let det_path = format!("{dir}/rtmdet/end2end.onnx");
    let pose_path = format!("{dir}/rtmpose/end2end.onnx");

    let mut det = session(&det_path, true)?;
    let mut pos = session(&pose_path, true)?;
    let b = detect(&mut det, &img)?.ok_or("人物が見つかりません")?;
    println!("[検出] 人物の枠 ({:.0}, {:.0})-({:.0}, {:.0}) スコア {:.2}", b[0], b[1], b[2], b[3], b[4]);
    let cb = crop_box(b);
    let kps = pose(&mut pos, &img, cb)?;
    for (i, k) in kps.iter().enumerate() {
        println!("  {:<4} ({:6.1}, {:6.1}) 信頼度 {:.2}", NAMES[i], k.0, k.1, k.2);
    }
    let mut vis = img.clone();
    draw(&mut vis, b, &kps);
    vis.save(out)?;
    println!("→ {out}");

    for (label, gpu) in [("DirectML(GPU)", true), ("CPU", false)] {
        let mut det = session(&det_path, gpu)?;
        let mut pos = session(&pose_path, gpu)?;
        let td = bench(|| detect(&mut det, &img).map(|_| ()))?;
        let tp = bench(|| pose(&mut pos, &img, cb).map(|_| ()))?;
        println!("[速度 {label}] 人物検出 {td:.1} ms / 姿勢推定(1 人){tp:.1} ms(前処理込み)");
    }
    Ok(())
}
