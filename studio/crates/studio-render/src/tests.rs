//! 合成・ミックス・書き出しを、実際の素材ファイルで確かめる。

use super::*;
use std::path::PathBuf;
use studio_core::command::Command::*;
use studio_core::project::Interp;
use studio_core::{Editor, Origin, Rate, TrackKind};

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 30;
const SECS: u32 = 4;
const BLOCK: u32 = 32;

fn s(x: f64) -> Time {
    Time::from_secs_f64(x)
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("studio-render-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// フレーム番号のブロック入りの素材動画(1 秒ごとにビープ)。
fn make_source(path: &Path) {
    let mut ex = Exporter::create(
        path,
        &ExportSettings { width: W, height: H, frame_rate: Rate::new(FPS, 1), video_bitrate: 4_000_000, sample_rate: Some(48_000), audio_bitrate: 192_000 },
    )
    .unwrap();
    for i in 0..FPS * SECS {
        let mut v = vec![60u8; (W * H * 4) as usize];
        for b in 0..8 {
            let c = if (i >> b) & 1 == 1 { 255 } else { 0 };
            for y in 0..BLOCK {
                for x in 0..BLOCK {
                    let k = ((y * W + b * BLOCK + x) * 4) as usize;
                    v[k..k + 3].copy_from_slice(&[c, c, c]);
                }
            }
        }
        ex.write_frame(&v).unwrap();
        let n = 48_000 / FPS;
        let audio: Vec<f32> = (0..n)
            .flat_map(|k| {
                let t = (i * n + k) as f32 / 48_000.0;
                let v = if i % FPS == 0 { (t * 1000.0 * std::f32::consts::TAU).sin() * 0.5 } else { 0.0 };
                [v, v]
            })
            .collect();
        ex.write_audio(&audio).unwrap();
    }
    ex.finish().unwrap();
}

fn read_index(bgra: &[u8], width: u32) -> u32 {
    let mut v = 0;
    for b in 0..8 {
        let (x, y) = (b * BLOCK + BLOCK / 2, BLOCK / 2);
        if bgra[((y * width + x) * 4 + 1) as usize] > 128 {
            v |= 1 << b;
        }
    }
    v
}

fn px(bgra: &[u8], width: u32, x: i32, y: i32) -> [u8; 4] {
    let i = ((y as u32 * width + x as u32) * 4) as usize;
    [bgra[i], bgra[i + 1], bgra[i + 2], bgra[i + 3]]
}

fn is_red(p: [u8; 4]) -> bool {
    p[2] > 200 && p[1] < 60 && p[0] < 60
}

/// 映像 2 トラック(動画 + 横長の赤い棒の画像)と音声 1 トラックのプロジェクト。
fn project(dir: &Path) -> (Project, Id) {
    let video = dir.join("source.mp4");
    make_source(&video);
    let bar = dir.join("bar.png");
    image::save_buffer(&bar, &[255, 0, 0, 255].repeat(200 * 40), 200, 40, image::ExtendedColorType::Rgba8).unwrap();
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: Some(Rate::new(FPS, 1)), sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: None, index: None, id: None },
                ImportAsset { name: "source.mp4".into(), path: video.to_string_lossy().into(), kind: studio_media::probe(&video).unwrap(), id: None },
                ImportAsset { name: "bar.png".into(), path: bar.to_string_lossy().into(), kind: studio_media::probe(&bar).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [v1, v2, a1, movie, img] = o.created[..] else { panic!() };
    let o = ed
        .transaction(
            "配置",
            vec![
                AddClip { track: v1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None },
                AddClip { track: v2, asset: img, start: s(1.0), duration: Some(s(2.0)), source_in: None, id: None },
                AddClip { track: a1, asset: movie, start: s(0.0), duration: None, source_in: None, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [_, bar_clip, audio_clip] = o.created[..] else { panic!() };
    ed.transaction(
        "動き",
        vec![
            SetParam { clip: bar_clip, param: "x".into(), value: 150.0 },
            // 1 秒目(クリップの 0 秒)から 2 秒目にかけて、0° → 90° に回る
            SetKey { clip: bar_clip, param: "rotation".into(), time: s(0.0), value: 0.0, interp: Interp::Linear },
            SetKey { clip: bar_clip, param: "rotation".into(), time: s(1.0), value: 90.0, interp: Interp::Linear },
            SetParam { clip: audio_clip, param: "volume".into(), value: 0.5 },
        ],
        Origin::User,
    )
    .unwrap();
    (ed.project, bar_clip)
}

#[test]
fn render_mix_and_export() {
    let d = dir();
    let (p, _bar) = project(&d);
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let (cx, cy) = (W as i32 / 2, H as i32 / 2);

    // 0.5 秒: 動画だけ(15 フレーム目)
    let f = r.render_bgra(&p, s(0.5)).unwrap();
    assert_eq!(read_index(&f, W), 15);
    assert!(!is_red(px(&f, W, cx + 150, cy)));
    // 1.0 秒: 赤い棒が横向き(幅 200)で x=+150 に出る
    let f = r.render_bgra(&p, s(1.0)).unwrap();
    assert_eq!(read_index(&f, W), 30);
    assert!(is_red(px(&f, W, cx + 150 + 90, cy)), "棒の右端付近");
    assert!(!is_red(px(&f, W, cx + 150, cy + 60)), "縦には伸びていない");
    // 2.5 秒: 90° 回って縦向き
    let f = r.render_bgra(&p, s(2.5)).unwrap();
    assert_eq!(read_index(&f, W), 75);
    assert!(is_red(px(&f, W, cx + 150, cy + 90)), "縦に伸びた");
    assert!(!is_red(px(&f, W, cx + 150 + 90, cy)), "横には無い");
    // 後ろへ戻る(シーク)
    let f = r.render_bgra(&p, s(0.2)).unwrap();
    assert_eq!(read_index(&f, W), 6);
    assert!(r.errors.is_empty(), "{:?}", r.errors);

    // ミックス: 1 秒目のビープが音量 0.5 で鳴る(元の振幅 0.5 → 0.25)
    let mut m = Mixer::new(48_000);
    let a = m.mix(&p, 48_000, 1600);
    let peak = a.iter().fold(0.0f32, |x, v| x.max(v.abs()));
    assert!((peak - 0.25).abs() < 0.03, "ピーク {peak}");
    let quiet = m.mix(&p, 48_000 + 4800, 1600);
    assert!(quiet.iter().all(|v| v.abs() < 0.02));

    // 書き出し → 読み戻し
    let out = d.join("export.mp4");
    // 素材の長さは、音声の符号化の余白でわずかに 4 秒を超えることがある → 最後の端数もフレームにする
    let rate = Rate::new(FPS, 1);
    let expect = rate.time_to_frame(p.duration() + Time(rate.frame_duration().0 - 1));
    assert!((120..=121).contains(&expect), "{}", p.duration());
    let mut last = 0;
    export(&p, &out, |pr| {
        last = pr.frame;
        assert_eq!(pr.total, expect);
        true
    })
    .unwrap();
    assert_eq!(last, expect);
    match studio_media::probe(&out).unwrap() {
        AssetKind::Video { width, height, duration, has_audio, .. } => {
            assert_eq!((width, height), (W, H));
            assert!(has_audio);
            assert!((duration.secs_f64() - SECS as f64).abs() < 0.1);
        }
        k => panic!("{k:?}"),
    }
    let mut vr = VideoReader::open(&out).unwrap();
    let f = vr.frame_at(s(2.5) + Time(Rate::new(FPS, 1).frame_duration().0 / 2)).unwrap();
    assert_eq!(read_index(&f.bgra, W), 75);
    assert!(is_red(px(&f.bgra, W, cx + 150, cy + 90)));
    let mut ar = AudioReader::open(&out, 48_000).unwrap();
    let a = ar.read(48_000 * 2, 1600).unwrap();
    let peak = a.iter().fold(0.0f32, |x, v| x.max(v.abs()));
    assert!((peak - 0.25).abs() < 0.05, "書き出し後のピーク {peak}");

    // 中止できる
    let r2 = export(&p, &d.join("cancel.mp4"), |pr| pr.frame < 10);
    assert!(r2.is_err());
    std::fs::remove_dir_all(&d).ok();
}

#[test]
fn corner_pin_draws_perspective_quad() {
    let gpu = Gpu::new(None).unwrap();
    // 左半分が赤、右半分が緑の 64x64
    let mut px = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let i = (y * 64 + x) * 4;
            px[i..i + 4].copy_from_slice(if x < 32 { &[0, 0, 255, 255] } else { &[0, 255, 0, 255] });
        }
    }
    let src = gpu.create_texture(64, 64, Some(&px), false).unwrap();
    let dst = gpu.create_texture(400, 400, None, true).unwrap();
    let rtv = gpu.rtv(&dst).unwrap();
    gpu.clear(&rtv, [0.0, 0.0, 0.0, 1.0]);
    // 台形(上が狭い): 画面中央原点
    let q = [[-50.0, -100.0], [50.0, -100.0], [150.0, 100.0], [-150.0, 100.0]];
    gpu.draw_pinned(&rtv, (400, 400), &src, &q, 1.0);
    let out = gpu.read(&dst).unwrap();
    let at = |x: f64, y: f64| {
        let (px, py) = ((x + 200.0) as usize, (y + 200.0) as usize);
        let i = (py * 400 + px) * 4;
        [out[i], out[i + 1], out[i + 2]]
    };
    assert_eq!(at(-100.0, 90.0), [0, 0, 255], "左下は赤");
    assert_eq!(at(100.0, 90.0), [0, 255, 0], "右下は緑");
    assert_eq!(at(-40.0, -95.0), [0, 0, 255]);
    assert_eq!(at(-120.0, -50.0), [0, 0, 0], "台形の外は描かない");
    assert_eq!(at(0.0, 150.0), [0, 0, 0]);
    // 中央の縦線は、射影変換では直線のまま(x = 0)
    assert_eq!(at(-3.0, 0.0), [0, 0, 255]);
    assert_eq!(at(3.0, 0.0), [0, 255, 0]);
    // 遠近: 上下の中点(y=0)は、素材の中央(v=0.5)より上の部分を表す。上が狭い = 奥なので、
    // 素材の上半分は画面で小さく見える。素材の横線 v=0.5 は y=0 より上(負)に来る
    let h = super::gpu::square_to_quad(&q);
    let map = |u: f64, v: f64| {
        let w = h[2][0] * u + h[2][1] * v + h[2][2];
        ((h[0][0] * u + h[0][1] * v + h[0][2]) / w, (h[1][0] * u + h[1][1] * v + h[1][2]) / w)
    };
    for (i, (u, v)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
        let p = map(u, v);
        assert!((p.0 - q[i][0]).abs() < 1e-9 && (p.1 - q[i][1]).abs() < 1e-9);
    }
    assert!(map(0.5, 0.5).1 < 0.0);
}
