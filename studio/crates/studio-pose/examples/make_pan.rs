//! 確認用: 写真を、拡大しながら右下へゆっくり動かした動画(4 秒)を作る。
//! 使い方: cargo run --release -p studio-pose --example make_pan -- 写真.jpg 出力.mp4
use studio_core::Rate;
use studio_media::{ExportSettings, Exporter};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let img = image::open(&a[1]).unwrap().to_rgb8();
    let (w, h) = (1280u32, 720u32);
    let mut ex = Exporter::create(std::path::Path::new(&a[2]), &ExportSettings { width: w, height: h, frame_rate: Rate::new(30, 1), video_bitrate: 10_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for k in 0..120 {
        let t = k as f32 / 119.0;
        let s = 1.6 + 0.4 * t; // 拡大率
        let (ox, oy) = (-120.0 + 260.0 * t, -60.0 + 90.0 * t); // 写真の左上の位置
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = ((x as f32 - ox) / s, (y as f32 - oy) / s);
                let i = ((y * w + x) * 4) as usize;
                let p = if sx >= 0.0 && sy >= 0.0 && (sx as u32) < img.width() && (sy as u32) < img.height() { img.get_pixel(sx as u32, sy as u32).0 } else { [90, 90, 90] };
                bgra[i..i + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        ex.write_frame(&bgra).unwrap();
    }
    ex.finish().unwrap();
}
