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

/// 幅 2・高さ 1・厚さ 0.2 の赤い板。1 秒で上へ 1 動くアニメーション付き。
fn write_plate(path: &Path) {
    let (x, y, z) = (1.0f32, 0.5f32, 0.1f32);
    let pos: Vec<[f32; 3]> = (0..8).map(|i| [if i & 1 == 0 { -x } else { x }, if i & 2 == 0 { -y } else { y }, if i & 4 == 0 { -z } else { z }]).collect();
    let quads = [[0, 1, 3, 2], [4, 6, 7, 5], [0, 4, 5, 1], [2, 3, 7, 6], [0, 2, 6, 4], [1, 5, 7, 3]];
    let idx: Vec<u16> = quads.iter().flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]]).collect();
    let mut buf: Vec<u8> = vec![];
    for p in &pos {
        for v in p {
            buf.extend(v.to_le_bytes());
        }
    }
    for i in &idx {
        buf.extend(i.to_le_bytes());
    }
    for v in [0.0f32, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        buf.extend(v.to_le_bytes());
    }
    let b64 = {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in buf.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                s.push(if k <= c.len() { T[(n >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
            }
        }
        s
    };
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}},"indices":1,"material":0}}]}}],
"materials":[{{"pbrMetallicRoughness":{{"baseColorFactor":[1,0,0,1]}}}}],
"buffers":[{{"byteLength":{len},"uri":"data:application/octet-stream;base64,{b64}"}}],
"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":96}},{{"buffer":0,"byteOffset":96,"byteLength":72}},{{"buffer":0,"byteOffset":168,"byteLength":8}},{{"buffer":0,"byteOffset":176,"byteLength":24}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":8,"type":"VEC3","min":[-1,-0.5,-0.1],"max":[1,0.5,0.1]}},
{{"bufferView":1,"componentType":5123,"count":36,"type":"SCALAR"}},
{{"bufferView":2,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]}},
{{"bufferView":3,"componentType":5126,"count":2,"type":"VEC3"}}],
"animations":[{{"channels":[{{"sampler":0,"target":{{"node":0,"path":"translation"}}}}],"samplers":[{{"input":2,"output":3}}]}}]}}"#,
        len = buf.len()
    );
    std::fs::write(path, json).unwrap();
}

/// 赤い画素の (数, 左右の幅, 中心 x, 中心 y)。
fn red_stats(px: &[u8], w: usize) -> (usize, usize, f64, f64) {
    let (mut n, mut lo, mut hi, mut sx, mut sy) = (0, usize::MAX, 0, 0.0, 0.0);
    for (i, p) in px.chunks_exact(4).enumerate() {
        if p[2] > 80 && p[1] < 40 && p[0] < 40 {
            let (x, y) = (i % w, i / w);
            n += 1;
            lo = lo.min(x);
            hi = hi.max(x);
            sx += x as f64;
            sy += y as f64;
        }
    }
    (n, hi.saturating_sub(lo), sx / n.max(1) as f64, sy / n.max(1) as f64)
}

#[test]
fn model_renders_rotates_and_animates() {
    let d = dir();
    let path = d.join("plate.gltf");
    write_plate(&path);
    let kind = model::probe(&path).unwrap();
    assert_eq!(kind, AssetKind::Model { animation: 1.0 });
    let mut ed = Editor::new(Project::default());
    ed.execute(SetSettings { width: Some(W), height: Some(H), frame_rate: None, sample_rate: None }, Origin::User).unwrap();
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Video, name: None, index: None, id: None },
                ImportAsset { name: "plate".into(), path: path.to_string_lossy().into(), kind, id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let c = ed.execute(AddClip { track: o.created[0], asset: o.created[1], start: s(0.0), duration: None, source_in: None, id: None }, Origin::User).unwrap().created[0];
    let mut r = Renderer::new(Gpu::new(None).unwrap());
    let still = r.render_bgra(&ed.project, s(0.0)).unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let (n0, w0, cx0, cy0) = red_stats(&still, W as usize);
    assert!(n0 > 5000, "板が描かれていない: {n0}");
    // 囲む球が画面の高さの約 85% → 幅 2 の板は、だいたい画面の高さの 0.85 * 2 / √5.04 * ... の幅で中央に描かれる
    assert!((cx0 - W as f64 / 2.0).abs() < 3.0 && (cy0 - H as f64 / 2.0).abs() < 3.0, "中央にない: {cx0},{cy0}");
    // 横に 90 度向けると、薄い側面だけが見える
    ed.execute(SetParam { clip: c, param: "rot_y".into(), value: 90.0 }, Origin::User).unwrap();
    let side = r.render_bgra(&ed.project, s(0.0)).unwrap();
    let (_, w90, _, _) = red_stats(&side, W as usize);
    assert!(w90 * 4 < w0, "向きが変わっていない: 幅 {w0} → {w90}");
    ed.execute(SetParam { clip: c, param: "rot_y".into(), value: 0.0 }, Origin::User).unwrap();
    // 位置・拡大はふつうのレイヤーと同じ
    ed.execute(SetParam { clip: c, param: "x".into(), value: 100.0 }, Origin::User).unwrap();
    ed.execute(SetParam { clip: c, param: "scale".into(), value: 0.5 }, Origin::User).unwrap();
    let moved = r.render_bgra(&ed.project, s(0.0)).unwrap();
    let (_, wm, cxm, _) = red_stats(&moved, W as usize);
    assert!((cxm - (W as f64 / 2.0 + 100.0)).abs() < 3.0, "x {cxm}");
    assert!((wm as f64 - w0 as f64 / 2.0).abs() < 4.0, "幅 {wm} / {w0}");
    // アニメーション: 0.5 秒で上へ 0.5 動く(画面では上 = y が小さい)
    let anim = r.render_bgra(&ed.project, s(0.5)).unwrap();
    let (_, _, _, cya) = red_stats(&anim, W as usize);
    let (_, _, _, cym) = red_stats(&moved, W as usize);
    assert!(cya < cym - 10.0, "動いていない: {cym} → {cya}");
    eprintln!("板の幅 {w0}px → 横向き {w90}px、アニメーションで y {cym:.1} → {cya:.1}");
}

/// 16bit ステレオの WAV。f(秒) が左右に同じ値で入る。
fn write_wav(path: &Path, secs: f64, f: impl Fn(f64) -> f32) {
    let rate = 48_000u32;
    let n = (secs * rate as f64) as u32;
    let mut d = Vec::new();
    let data_len = n * 4;
    d.extend(b"RIFF");
    d.extend((36 + data_len).to_le_bytes());
    d.extend(b"WAVEfmt ");
    d.extend(16u32.to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(2u16.to_le_bytes());
    d.extend(rate.to_le_bytes());
    d.extend((rate * 4).to_le_bytes());
    d.extend(4u16.to_le_bytes());
    d.extend(16u16.to_le_bytes());
    d.extend(b"data");
    d.extend(data_len.to_le_bytes());
    for i in 0..n {
        let v = (f(i as f64 / rate as f64).clamp(-1.0, 1.0) * 32767.0) as i16;
        d.extend(v.to_le_bytes());
        d.extend(v.to_le_bytes());
    }
    std::fs::write(path, d).unwrap();
}

/// 再生と同じく 1024 標本ずつ続けてミックスした 3 秒分。
fn mix_all(p: &Project) -> Vec<f32> {
    let mut m = Mixer::new(48_000);
    let mut out = vec![];
    let mut s = 0;
    while s < 48_000 * 3 {
        out.extend(m.mix(p, s, 1024));
        s += 1024;
    }
    out
}

/// 時刻 [a, b) 秒の (左, 右) のピーク。
fn peak_in(buf: &[f32], a: f64, b: f64) -> (f32, f32) {
    let (i, j) = ((a * 48_000.0) as usize * 2, (b * 48_000.0) as usize * 2);
    super::peak(&buf[i..j])
}

#[test]
fn mixer_gain_pan_solo_eq_compressor_ducking() {
    use studio_core::project::AudioEffect;
    let d = dir();
    let (voice, bgm) = (d.join("voice.wav"), d.join("bgm.wav"));
    // 声: 1〜2 秒に 1kHz、BGM: ずっと 200Hz
    write_wav(&voice, 3.0, |t| if (1.0..2.0).contains(&t) { 0.5 * (t * 1000.0 * std::f64::consts::TAU).sin() as f32 } else { 0.0 });
    write_wav(&bgm, 3.0, |t| 0.25 * (t * 200.0 * std::f64::consts::TAU).sin() as f32);
    let mut ed = Editor::new(Project::default());
    let o = ed
        .transaction(
            "準備",
            vec![
                AddTrack { kind: TrackKind::Audio, name: Some("声".into()), index: None, id: None },
                AddTrack { kind: TrackKind::Audio, name: Some("BGM".into()), index: None, id: None },
                ImportAsset { name: "voice".into(), path: voice.to_string_lossy().into(), kind: studio_media::probe(&voice).unwrap(), id: None },
                ImportAsset { name: "bgm".into(), path: bgm.to_string_lossy().into(), kind: studio_media::probe(&bgm).unwrap(), id: None },
            ],
            Origin::User,
        )
        .unwrap();
    let [tv, tb, av, ab] = o.created[..] else { panic!() };
    ed.transaction(
        "配置",
        vec![
            AddClip { track: tv, asset: av, start: s(0.0), duration: None, source_in: None, id: None },
            AddClip { track: tb, asset: ab, start: s(0.0), duration: None, source_in: None, id: None },
        ],
        Origin::User,
    )
    .unwrap();
    let near = |a: f32, b: f32, tol: f32| (a - b).abs() <= tol;
    let base = mix_all(&ed.project);
    let (v0, b0) = (peak_in(&base, 1.2, 1.8), peak_in(&base, 0.2, 0.8));
    assert!(near(v0.0, 0.75, 0.03), "声 + BGM の山 {v0:?}");
    assert!(near(b0.0, 0.25, 0.01), "BGM {b0:?}");

    // 音量 -6dB で約半分、左右のバランスを左いっぱいにすると右は無音
    ed.execute(SetTrackAudio { track: tb, gain_db: Some(-6.0206), pan: Some(-1.0), solo: None }, Origin::User).unwrap();
    let m = mix_all(&ed.project);
    let b = peak_in(&m, 0.2, 0.8);
    assert!(near(b.0, 0.125, 0.006) && b.1 < 1e-4, "音量・バランス {b:?}");
    ed.undo();

    // ソロ: 声だけ
    ed.execute(SetTrackAudio { track: tv, gain_db: None, pan: None, solo: Some(true) }, Origin::User).unwrap();
    let m = mix_all(&ed.project);
    assert!(peak_in(&m, 0.2, 0.8).0 < 1e-4, "ソロなのに BGM が鳴っている");
    ed.undo();

    // イコライザ: 中音(1kHz)を +12dB → 声の 1kHz は約 4 倍(BGM はソロで消して測る)
    ed.execute(SetTrackAudio { track: tv, gain_db: Some(-20.0), pan: None, solo: Some(true) }, Origin::User).unwrap();
    let before = peak_in(&mix_all(&ed.project), 1.3, 1.8).0;
    ed.execute(SetTrackEffects { track: tv, effects: vec![AudioEffect::Eq { low_db: 0.0, mid_db: 12.0, high_db: 0.0 }] }, Origin::User).unwrap();
    let after = peak_in(&mix_all(&ed.project), 1.3, 1.8).0;
    assert!(near(after / before, 3.98, 0.15), "EQ の倍率 {}", after / before);
    // コンプレッサー: -6dB の音を、しきい値 -20dB・4:1 → 超えた 14dB が 3.5dB に(約 -16.5dB = 0.15)
    ed.execute(SetTrackAudio { track: tv, gain_db: Some(0.0), pan: None, solo: None }, Origin::User).unwrap();
    ed.execute(SetTrackEffects { track: tv, effects: vec![AudioEffect::Compressor { threshold_db: -20.0, ratio: 4.0, makeup_db: 0.0 }] }, Origin::User).unwrap();
    let c = peak_in(&mix_all(&ed.project), 1.5, 1.9).0;
    assert!(near(c, 0.15, 0.02), "コンプレッサー後 {c}");
    ed.execute(SetTrackEffects { track: tv, effects: vec![] }, Origin::User).unwrap();
    ed.execute(SetTrackAudio { track: tv, gain_db: None, pan: None, solo: Some(false) }, Origin::User).unwrap();

    // ダッキング: 声が鳴る間 BGM を -12dB。声のトラックを消音しても、きっかけには使える
    ed.execute(SetTrackEffects { track: tb, effects: vec![AudioEffect::Duck { source: tv, amount_db: -12.0 }] }, Origin::User).unwrap();
    ed.execute(SetTrackFlags { track: tv, muted: Some(true), locked: None }, Origin::User).unwrap();
    let mut mx = Mixer::new(48_000);
    let mut m = vec![];
    let mut lv = vec![];
    for k in 0..(48_000 * 3 / 1024) {
        m.extend(mx.mix(&ed.project, k * 1024, 1024));
        lv.push(mx.levels.clone());
    }
    let (pre, during, post) = (peak_in(&m, 0.5, 0.9).0, peak_in(&m, 1.5, 1.9).0, peak_in(&m, 2.8, 2.95).0);
    assert!(near(pre, 0.25, 0.01), "前 {pre}");
    assert!(near(during, 0.25 * 0.251, 0.01), "声の間 {during}");
    assert!(post > 0.2, "声の後は戻る {post}");
    // メーター: 消音中の声は 0、BGM は鳴っている
    let l = &lv[70]; // 約 1.5 秒
    let get = |id: Id| l.tracks.iter().find(|t| t.0 == id).unwrap();
    assert_eq!((get(tv).1, get(tv).2), (0.0, 0.0));
    assert!(get(tb).1 > 0.03 && l.master.0 > 0.03);
    eprintln!("ダッキング: 前 {pre:.3} → 声の間 {during:.3} → 後 {post:.3} / EQ ×{:.2} / コンプ {c:.3}", after / before);
}
