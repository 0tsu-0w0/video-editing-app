//! 確認用: 模様が一定の速さ(1 フレームに右へ 2.5px、上へ 1.25px)で流れる動画を作る。
//! 使い方: cargo run --release -p studio-track --example make_moving -- 出力.mp4
use studio_core::Rate;
use studio_media::{ExportSettings, Exporter};
use studio_track::vision::{texture, Rng};

fn main() {
    let path = std::env::args().nth(1).expect("出力先を指定してください");
    let (w, h, frames) = (1280u32, 720u32, 120u32);
    let tex = texture(2048, &mut Rng(7), 1.0);
    let mut ex = Exporter::create(std::path::Path::new(&path), &ExportSettings { width: w, height: h, frame_rate: Rate::new(30, 1), video_bitrate: 12_000_000, sample_rate: None, audio_bitrate: 0 }).unwrap();
    for k in 0..frames {
        let mut bgra = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let v = tex.sample(x as f32 - 2.5 * k as f32 + 300.0, y as f32 + 1.25 * k as f32 + 300.0);
                let i = ((y * w + x) * 4) as usize;
                let c = (v * 255.0) as u8;
                bgra[i..i + 3].copy_from_slice(&[c, (c as f32 * 0.9) as u8, (c as f32 * 0.8) as u8]);
            }
        }
        ex.write_frame(&bgra).unwrap();
    }
    ex.finish().unwrap();
    println!("{path}");
}
