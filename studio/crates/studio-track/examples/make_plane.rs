//! 確認用: 模様のある板が、遠近を変えながら動く動画を作る(平面トラッキングの確認用)。
//! 板の四隅の正しい位置(画面中央が原点)を、最初と最後のフレームについて表示する。
//! 使い方: cargo run --release -p studio-track --example make_plane -- 出力.mp4
use studio_core::Rate;
use studio_media::{ExportSettings, Exporter};
use studio_track::vision::{apply, fit_homography, invert, texture, Rng};

const W: u32 = 1280;
const H: u32 = 720;
const FRAMES: u32 = 120;

/// フレーム k での板の四隅(画素、左上原点)。左へ回り込みながら近づく。
fn corners(k: u32) -> [(f32, f32); 4] {
    let t = k as f32 / (FRAMES - 1) as f32;
    let a = [(380.0, 180.0), (900.0, 200.0), (880.0, 560.0), (400.0, 540.0)];
    let b = [(300.0, 120.0), (760.0, 230.0), (750.0, 520.0), (290.0, 620.0)];
    let mut c = [(0.0, 0.0); 4];
    for i in 0..4 {
        c[i] = (a[i].0 + (b[i].0 - a[i].0) * t, a[i].1 + (b[i].1 - a[i].1) * t);
    }
    c
}

fn main() {
    let path = std::env::args().nth(1).expect("出力先を指定してください");
    let bg = texture(2048, &mut Rng(11), 0.4);
    let board = texture(1024, &mut Rng(5), 1.0);
    let mut ex = Exporter::create(
        std::path::Path::new(&path),
        &ExportSettings { width: W, height: H, frame_rate: Rate::new(30, 1), video_bitrate: 12_000_000, sample_rate: None, audio_bitrate: 0 },
    )
    .unwrap();
    let unit = [(0.0, 0.0), (1024.0, 0.0), (1024.0, 768.0), (0.0, 768.0)];
    for k in 0..FRAMES {
        let c = corners(k);
        // 画面 → 板の模様 の射影変換
        let h = invert(&fit_homography(&unit, &c).unwrap());
        let mut bgra = vec![255u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let p = (x as f32 + 0.5, y as f32 + 0.5);
                let u = apply(&h, p);
                let i = ((y * W + x) * 4) as usize;
                if u.0 >= 0.0 && u.1 >= 0.0 && u.0 < 1024.0 && u.1 < 768.0 {
                    let v = board.sample(u.0, u.1 + 100.0);
                    let c = (60.0 + v * 195.0) as u8;
                    bgra[i..i + 3].copy_from_slice(&[(c as f32 * 0.7) as u8, (c as f32 * 0.85) as u8, c]);
                } else {
                    // 背景はゆっくり右へ流れる(カメラが動いている)
                    let v = bg.sample(x as f32 * 0.8 - k as f32 * 1.5 + 400.0, y as f32 * 0.8 + 300.0);
                    let c = (40.0 + v * 80.0) as u8;
                    bgra[i..i + 3].copy_from_slice(&[c, c, (c as f32 * 0.9) as u8]);
                }
            }
        }
        ex.write_frame(&bgra).unwrap();
    }
    ex.finish().unwrap();
    let centered = |c: [(f32, f32); 4]| c.map(|p| [p.0 - W as f32 / 2.0, p.1 - H as f32 / 2.0]);
    println!("最初の四隅 {:?}", centered(corners(0)));
    println!("最後の四隅 {:?}", centered(corners(FRAMES - 1)));
}
